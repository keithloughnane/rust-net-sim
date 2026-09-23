use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use slotmap::SecondaryMap;

use crate::logic::{Outgoing, SendQueue, outgoing};
use crate::monitor::{Alert, Monitor, MonitorConfig};
use crate::route::names;
use crate::trace::{DropReason, TraceEvent, TraceLog};
use crate::{
    AcceptRule, Context, ControllerLogic, Event, Link, LinkId, Network, NodeId, Packet, PacketId,
    PacketRoute, SendError, create_logic,
};

/// Traffic counters for one node.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NodeStats {
    /// Packets this node put on a link.
    pub sent: u64,
    /// Packet copies this node accepted.
    pub received: u64,
}

/// Why logic could not be attached.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LogicError {
    /// The node does not exist.
    UnknownNode(NodeId),
    /// No built-in logic has this name.
    UnknownKind(String),
}

impl fmt::Display for LogicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "unknown node {id:?}"),
            Self::UnknownKind(kind) => write!(f, "no logic named `{kind}`"),
        }
    }
}

impl std::error::Error for LogicError {}

/// Hard limits that keep a runaway network from taking the host down with it: the fuse.
///
/// When a tick reaches a limit it stops delivering, keeps the rest queued, and returns
/// [`TickOutcome::FuseTripped`] with a [`FuseReport`] explaining why. The world is never left
/// inconsistent; what to do next (pause, inspect, carry on) is the host's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most packets put on links in one tick.
    pub max_transmissions_per_tick: u64,
    /// Most packet copies handed to listeners in one tick.
    pub max_deliveries_per_tick: u64,
    /// Most packets waiting for the next tick. Sends beyond this are refused.
    pub max_pending: usize,
    /// Largest event payload, in bytes.
    pub max_payload_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_transmissions_per_tick: 5_000,
            max_deliveries_per_tick: 50_000,
            max_pending: 20_000,
            max_payload_bytes: 64 * 1024,
        }
    }
}

/// Which limit a tick hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FuseLimit {
    /// [`Limits::max_transmissions_per_tick`].
    Transmissions,
    /// [`Limits::max_deliveries_per_tick`].
    Deliveries,
    /// [`Limits::max_pending`]: sends were refused.
    Queue,
}

impl FuseLimit {
    /// A stable machine-readable name, such as `"deliveries"`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Transmissions => "transmissions",
            Self::Deliveries => "deliveries",
            Self::Queue => "queue",
        }
    }
}

/// What tripped the fuse, and the best available explanation of why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseReport {
    /// Tick it happened on.
    pub tick: u64,
    /// Which limit was hit.
    pub limit: FuseLimit,
    /// The configured limit.
    pub max: u64,
    /// Packets still queued when the tick stopped. They are delivered by later ticks if the host
    /// keeps going.
    pub held_back: usize,
    /// The nodes that sent the most this tick, busiest first.
    pub top_senders: Vec<(NodeId, u64)>,
    /// The links that carried the most this tick, busiest first.
    pub top_links: Vec<(LinkId, u64)>,
    /// The most recent monitor alerts, which usually name the cause (a relay loop,
    /// amplification, a flood).
    pub recent_alerts: Vec<Alert>,
}

/// How a tick ended.
#[must_use = "a tripped fuse means the network is running away; the host should react"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// Everything due was delivered.
    Completed,
    /// A hard limit was reached. See [`World::fuse_report`].
    FuseTripped,
}

/// Running totals for watching a world's load.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    /// The current tick.
    pub tick: u64,
    /// Packets put on links during the last tick.
    pub transmissions: u64,
    /// Packet copies delivered during the last tick.
    pub deliveries: u64,
    /// The most transmissions seen in any one tick.
    pub peak_transmissions: u64,
    /// Packets waiting for the next tick.
    pub pending: usize,
    /// Sends refused because the queue was full.
    pub refused_sends: u64,
    /// Packet copies dropped because their TTL ran out.
    pub ttl_drops: u64,
    /// Queued packets dropped because the sender had left the link.
    pub sender_left_drops: u64,
    /// Monitor alerts raised.
    pub alerts: u64,
    /// Ticks that tripped the fuse.
    pub fuse_trips: u64,
    /// Trace events waiting to be drained.
    pub trace_len: usize,
    /// Trace events lost because the buffer was full.
    pub trace_discarded: u64,
}

/// A self-contained simulation: a [`Network`], the logic attached to its nodes, and the clock.
///
/// The host owns the clock. Nothing moves between calls to [`World::tick`]: each tick runs
/// every logic's `on_tick`, then delivers everything that was sent before the tick began.
/// Anything sent while handling a packet waits for the next tick, so packets advance one hop
/// per tick and a tick always finishes. [`Limits`] bound how much one tick can do.
///
/// ```
/// use emergence_engine::{Event, PacketRoute, TickOutcome, World};
///
/// let mut world = World::new();
/// let net = world.network_mut();
/// let wifi = net.create_link("wifi")?;
/// let (a, b) = (net.create_node("a", "device")?, net.create_node("b", "device")?);
/// let root = net.root();
/// net.connect(root, a, Some(wifi))?;
/// net.connect(root, b, Some(wifi))?;
/// world.set_logic_kind(b, "responder")?;
///
/// world.send(a, wifi, PacketRoute::new("b", "wifi"), Event::new("ping"))?;
/// assert_eq!(world.tick(), TickOutcome::Completed); // b receives the ping, queues a pong
/// assert_eq!(world.tick(), TickOutcome::Completed); // a receives the pong
/// assert_eq!(world.stats(a).received, 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Default)]
pub struct World {
    network: Network,
    tick_count: u64,
    logics: SecondaryMap<NodeId, Box<dyn ControllerLogic>>,
    stats: SecondaryMap<NodeId, NodeStats>,
    pending: Vec<Outgoing>,
    next_packet: u64,
    trace: TraceLog,
    trace_packets: bool,
    limits: Limits,
    monitor: Monitor,
    topology_changed: bool,
    health: Health,
    fuse: Option<FuseReport>,
    /// Packets of the batch being delivered that have not gone out yet.
    reserved: usize,
    /// `health.refused_sends` when the last tick ended.
    refused_seen: u64,
}

impl World {
    /// Creates a world with an empty network, at tick zero, with packet tracing on.
    #[must_use]
    pub fn new() -> Self {
        Self {
            trace_packets: true,
            ..Self::default()
        }
    }

    /// Returns how many ticks have run since the world was created.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// The world's node/link graph.
    #[must_use]
    pub fn network(&self) -> &Network {
        &self.network
    }

    /// Mutable access to the world's node/link graph, for building or changing topology.
    pub fn network_mut(&mut self) -> &mut Network {
        self.topology_changed = true;
        &mut self.network
    }

    /// The fuse's limits.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Changes the fuse's limits.
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// The monitor's thresholds.
    #[must_use]
    pub fn monitor_config(&self) -> &MonitorConfig {
        &self.monitor.config
    }

    /// Changes the monitor's thresholds.
    pub fn set_monitor_config(&mut self, config: MonitorConfig) {
        self.monitor.config = config;
    }

    /// Whether every packet sent, delivered and dropped is recorded in the trace. Turning it off
    /// makes busy simulations much cheaper; alerts and notes are always recorded.
    pub fn set_trace_packets(&mut self, enabled: bool) {
        self.trace_packets = enabled;
    }

    /// Running totals for the world's load.
    #[must_use]
    pub fn health(&self) -> Health {
        Health {
            tick: self.tick_count,
            pending: self.pending.len(),
            alerts: self.monitor.alerts_raised,
            trace_len: self.trace.len(),
            trace_discarded: self.trace.discarded(),
            ..self.health
        }
    }

    /// Why the fuse tripped, if the last tick tripped it.
    #[must_use]
    pub fn fuse_report(&self) -> Option<&FuseReport> {
        self.fuse.as_ref()
    }

    /// Attaches `logic` to `node`, replacing any it had, and runs its `on_start`.
    ///
    /// # Errors
    ///
    /// Fails if the node does not exist.
    pub fn set_logic(
        &mut self,
        node: NodeId,
        mut logic: Box<dyn ControllerLogic>,
    ) -> Result<(), LogicError> {
        if self.network.node(node).is_none() {
            return Err(LogicError::UnknownNode(node));
        }
        let mut ctx = Context {
            network: &self.network,
            node,
            tick: self.tick_count,
            arrived_on: None,
            accepted_by: None,
            queue: SendQueue {
                items: &mut self.pending,
                limits: &self.limits,
                reserved: self.reserved,
                refused: &mut self.health.refused_sends,
            },
            trace: &mut self.trace,
        };
        logic.on_start(&mut ctx);
        self.logics.insert(node, logic);
        self.topology_changed = true;
        Ok(())
    }

    /// Attaches a built-in logic by name (see [`LOGIC_KINDS`](crate::LOGIC_KINDS)). An empty
    /// name or `"none"` removes the node's logic.
    ///
    /// # Errors
    ///
    /// Fails if the node does not exist or the name is unknown.
    pub fn set_logic_kind(&mut self, node: NodeId, kind: &str) -> Result<(), LogicError> {
        if kind.is_empty() || kind == "none" {
            if self.network.node(node).is_none() {
                return Err(LogicError::UnknownNode(node));
            }
            self.logics.remove(node);
            self.topology_changed = true;
            return Ok(());
        }
        let logic = create_logic(kind).ok_or_else(|| LogicError::UnknownKind(kind.to_owned()))?;
        self.set_logic(node, logic)
    }

    /// The kind of logic attached to `node`, if any.
    #[must_use]
    pub fn logic_kind(&self, node: NodeId) -> Option<&'static str> {
        self.logics.get(node).map(|l| l.kind())
    }

    /// Traffic counters for `node`. All zero for unknown nodes.
    #[must_use]
    pub fn stats(&self, node: NodeId) -> NodeStats {
        self.stats.get(node).copied().unwrap_or_default()
    }

    /// Queues a packet from `node` on `via`, as if the node's own logic had sent it. It is
    /// delivered on the next tick.
    ///
    /// # Errors
    ///
    /// Fails if the node or link does not exist, the node is not on the link, the event is
    /// too large, or the queue is full.
    pub fn send(
        &mut self,
        node: NodeId,
        via: LinkId,
        to: PacketRoute,
        event: Event,
    ) -> Result<(), SendError> {
        let out = outgoing(
            &self.network,
            &self.limits,
            node,
            via,
            None,
            Arc::new(to),
            Arc::new(event),
        )?;
        SendQueue {
            items: &mut self.pending,
            limits: &self.limits,
            reserved: self.reserved,
            refused: &mut self.health.refused_sends,
        }
        .push(out)
    }

    /// Everything that has happened since the last call, oldest first.
    pub fn drain_trace(&mut self) -> Vec<TraceEvent> {
        self.trace.drain()
    }

    /// How many trace events were thrown away because nobody drained the buffer in time.
    #[must_use]
    pub fn trace_discarded(&self) -> u64 {
        self.trace.discarded()
    }

    /// Advances the simulation by one tick. Returns [`TickOutcome::FuseTripped`] if a
    /// [`Limits`] bound was reached; the host should then pause and look at
    /// [`fuse_report`](Self::fuse_report).
    pub fn tick(&mut self) -> TickOutcome {
        self.tick_count += 1;
        let tick = self.tick_count;
        self.fuse = None;

        if std::mem::take(&mut self.topology_changed) {
            self.monitor
                .check_topology(&mut self.trace, tick, &self.network, &self.logics);
        }

        let nodes: Vec<NodeId> = self.logics.keys().collect();
        for node in nodes {
            self.run_logic(tick, node, None, |logic, ctx| logic.on_tick(ctx));
        }

        // Deliver what was queued before this point; anything sent while delivering waits for
        // the next tick.
        let mut batch = std::mem::take(&mut self.pending).into_iter();
        let (mut transmissions, mut deliveries) = (0_u64, 0_u64);
        let mut tripped = None;
        while let Some(out) = batch.next() {
            self.reserved = batch.len();
            let audience = self
                .network
                .link(out.via)
                .map_or(0, |l| l.subscribers().len() as u64 + 1);
            if transmissions >= self.limits.max_transmissions_per_tick {
                tripped = Some((FuseLimit::Transmissions, out));
                break;
            }
            if deliveries > 0 && deliveries + audience > self.limits.max_deliveries_per_tick {
                tripped = Some((FuseLimit::Deliveries, out));
                break;
            }
            if let Some(n) = self.transmit(tick, out) {
                transmissions += 1;
                deliveries += n;
            }
        }
        self.reserved = 0;
        let mut limit = tripped.as_ref().map(|(l, _)| *l);
        if let Some((_, out)) = tripped {
            // Keep the rest, in order, ahead of anything sent during this tick.
            let mut held: Vec<Outgoing> = std::iter::once(out).chain(batch).collect();
            held.append(&mut self.pending);
            self.pending = held;
        }
        if limit.is_none() && self.health.refused_sends > self.refused_seen {
            limit = Some(FuseLimit::Queue);
        }
        self.refused_seen = self.health.refused_sends;

        self.health.transmissions = transmissions;
        self.health.deliveries = deliveries;
        self.health.peak_transmissions = self.health.peak_transmissions.max(transmissions);
        if let Some(limit) = limit {
            self.health.fuse_trips += 1;
            self.fuse = Some(FuseReport {
                tick,
                limit,
                max: match limit {
                    FuseLimit::Transmissions => self.limits.max_transmissions_per_tick,
                    FuseLimit::Deliveries => self.limits.max_deliveries_per_tick,
                    FuseLimit::Queue => self.limits.max_pending as u64,
                },
                held_back: self.pending.len(),
                top_senders: self.monitor.top_senders(5),
                top_links: self.monitor.top_links(5),
                recent_alerts: Vec::new(),
            });
        }
        self.monitor.end_tick(&mut self.trace, tick);
        if let Some(report) = &mut self.fuse {
            report.recent_alerts = self.monitor.recent_alerts(tick.saturating_sub(200));
            return TickOutcome::FuseTripped;
        }
        TickOutcome::Completed
    }

    /// Runs one logic callback for `node` with panic isolation. A panicking logic is removed
    /// and reported, and the rest of the world carries on. Returns how many packets it sent.
    fn run_logic(
        &mut self,
        tick: u64,
        node: NodeId,
        arrival: Option<(LinkId, AcceptRule)>,
        call: impl FnOnce(&mut dyn ControllerLogic, &mut Context<'_>),
    ) -> u64 {
        let Some(logic) = self.logics.get_mut(node) else {
            return 0;
        };
        let (queued_before, refused_before) = (self.pending.len(), self.health.refused_sends);
        let mut ctx = Context {
            network: &self.network,
            node,
            tick,
            arrived_on: arrival.map(|(l, _)| l),
            accepted_by: arrival.map(|(_, r)| r),
            queue: SendQueue {
                items: &mut self.pending,
                limits: &self.limits,
                reserved: self.reserved,
                refused: &mut self.health.refused_sends,
            },
            trace: &mut self.trace,
        };
        let result = catch_unwind(AssertUnwindSafe(|| call(logic.as_mut(), &mut ctx)));
        let sent = (self.pending.len().saturating_sub(queued_before)) as u64
            + (self.health.refused_sends - refused_before);
        if let Err(payload) = result {
            let kind = logic.kind();
            self.logics.remove(node);
            let why = payload
                .downcast_ref::<&str>()
                .map(ToString::to_string)
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            self.monitor.on_logic_panicked(
                &mut self.trace,
                tick,
                node,
                format!("`{kind}` logic panicked and was removed: {why}"),
            );
        }
        sent
    }

    /// Puts one packet on its link and hands a copy to every listener that accepts it. Returns
    /// how many copies were delivered, or `None` if nothing was transmitted.
    fn transmit(&mut self, tick: u64, out: Outgoing) -> Option<u64> {
        self.next_packet += 1;
        let id = PacketId(self.next_packet);
        if !self.network.can_use_link(out.sender, out.via) {
            self.health.sender_left_drops += 1;
            if self.trace_packets {
                self.trace.push(TraceEvent::Dropped {
                    tick,
                    packet: id,
                    at: Some(out.sender),
                    reason: DropReason::SenderLeftLink,
                });
            }
            return None;
        }
        let audience = listeners(&self.network, self.network.link(out.via)?, out.sender);
        let packet = Packet {
            id,
            from: out.from,
            to: out.to,
            event: out.event,
            ttl: out.ttl,
            trace: out.trace,
        };
        if let Some(e) = self.stats.entry(out.sender) {
            e.or_default().sent += 1;
        }
        self.monitor.on_transmit(out.sender, out.via);
        if self.trace_packets {
            self.trace.push(TraceEvent::Sent {
                tick,
                packet: packet.clone(),
                sender: out.sender,
                link: out.via,
            });
        }

        let mut delivered = 0;
        for listener in audience {
            let link = self.network.link(out.via)?;
            let wants = self.logics.get(listener).is_some_and(|l| l.wants(&packet));
            let Some(rule) = accepts(&self.network, listener, &packet, link)
                .or(wants.then_some(AcceptRule::Forced))
            else {
                continue;
            };
            delivered += 1;

            let mut copy = packet.clone();
            copy.ttl = copy.ttl.saturating_sub(1);
            copy.trace.push(listener);
            if copy.ttl == 0 {
                self.health.ttl_drops += 1;
                self.monitor.on_ttl_expired(&mut self.trace, tick, listener);
                if self.trace_packets {
                    self.trace.push(TraceEvent::Dropped {
                        tick,
                        packet: packet.id,
                        at: Some(listener),
                        reason: DropReason::TtlExpired,
                    });
                }
                continue;
            }

            if let Some(e) = self.stats.entry(listener) {
                e.or_default().received += 1;
            }
            self.monitor
                .on_delivered(&mut self.trace, tick, listener, &copy);
            if self.trace_packets {
                self.trace.push(TraceEvent::Delivered {
                    tick,
                    packet: packet.id,
                    receiver: listener,
                    link: out.via,
                    rule,
                });
            }
            let sent = self.run_logic(tick, listener, Some((out.via, rule)), |logic, ctx| {
                logic.on_received(&copy, ctx);
            });
            self.monitor
                .on_handled(&mut self.trace, tick, listener, sent);
        }
        if delivered == 0 && self.trace_packets {
            self.trace.push(TraceEvent::Dropped {
                tick,
                packet: packet.id,
                at: None,
                reason: DropReason::NoRecipient,
            });
        }
        Some(delivered)
    }
}

/// Everyone who hears traffic on `link`: its subscribers, then its owner (which hears its own
/// internal buses), minus the sender. The root is the world itself, not a device, so it never
/// listens.
fn listeners(network: &Network, link: &Link, sender: NodeId) -> Vec<NodeId> {
    let mut out: Vec<NodeId> = link.subscribers().to_vec();
    if let Some(owner) = link.owner()
        && !out.contains(&owner)
    {
        out.push(owner);
    }
    out.retain(|&n| n != sender && n != network.root());
    out
}

/// The accept rules from the design docs (`02-routing-and-delivery.md`).
fn accepts(
    network: &Network,
    listener: NodeId,
    packet: &Packet,
    link: &Link,
) -> Option<AcceptRule> {
    let node = network.node(listener)?;
    let name = node.name();
    let to = packet.to();
    if packet.from().node() == name {
        return None; // Never accept your own echo.
    }
    let on_this_link = |l: &str| l == link.name() || l == names::BROADCAST || l == names::ANY_LINK;
    if to.node() == name && on_this_link(to.link()) {
        return Some(AcceptRule::Addressed);
    }
    if to.node() == names::BROADCAST && on_this_link(to.link()) {
        return Some(AcceptRule::Broadcast);
    }
    if link.owner() != Some(listener) {
        return None;
    }
    let from_child = node.children().iter().any(|&c| {
        network
            .node(c)
            .is_some_and(|c| c.name() == packet.from().node())
    });
    if to.node() == names::PARENT && from_child {
        return Some(AcceptRule::ChildToParent);
    }
    // Gateway: the next hop is off this bus. (The design docs test *any* hop, but then a node
    // would intercept replies addressed to its own children just because they continue
    // further inside those children.)
    if !on_this_link(to.link()) {
        return Some(AcceptRule::Gateway);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::events;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn new_world_starts_at_tick_zero() {
        assert_eq!(World::new().tick_count(), 0);
    }

    /// wifi: laptop, pc (gateway) ── pc/ipc: fileman (responder), scanner
    struct Office {
        world: World,
        wifi: LinkId,
        ipc: LinkId,
        laptop: NodeId,
        pc: NodeId,
        fileman: NodeId,
        scanner: NodeId,
    }

    fn office() -> Result<Office, Box<dyn std::error::Error>> {
        let mut world = World::new();
        let net = world.network_mut();
        let root = net.root();
        let wifi = net.create_link("wifi")?;
        let ipc = net.create_link("ipc")?;
        let laptop = net.create_node("laptop", "computer")?;
        let pc = net.create_node("pc", "computer")?;
        let fileman = net.create_node("fileman", "app")?;
        let scanner = net.create_node("scanner", "app")?;
        net.connect(root, laptop, Some(wifi))?;
        net.connect(root, pc, Some(wifi))?;
        net.connect(pc, fileman, Some(ipc))?;
        net.connect(pc, scanner, Some(ipc))?;
        world.set_logic_kind(laptop, "responder")?;
        world.set_logic_kind(pc, "gateway")?;
        world.set_logic_kind(fileman, "responder")?;
        Ok(Office {
            world,
            wifi,
            ipc,
            laptop,
            pc,
            fileman,
            scanner,
        })
    }

    fn delivered(trace: &[TraceEvent]) -> Vec<(NodeId, AcceptRule)> {
        trace
            .iter()
            .filter_map(|e| match e {
                TraceEvent::Delivered { receiver, rule, .. } => Some((*receiver, *rule)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn sends_wait_for_the_next_tick() -> TestResult {
        let mut o = office()?;
        o.world
            .send(o.laptop, o.wifi, "pc@wifi".parse()?, Event::new("hello"))?;
        assert_eq!(o.world.stats(o.pc).received, 0);
        assert_eq!(o.world.tick(), TickOutcome::Completed);
        assert_eq!(o.world.stats(o.pc).received, 1);
        Ok(())
    }

    #[test]
    fn ping_is_answered_with_pong() -> TestResult {
        let mut o = office()?;
        o.world.send(
            o.laptop,
            o.wifi,
            "pc@wifi".parse()?,
            Event::new(events::PING),
        )?;
        assert_eq!(o.world.tick(), TickOutcome::Completed);
        assert_eq!(o.world.tick(), TickOutcome::Completed);
        let trace = o.world.drain_trace();
        assert_eq!(
            delivered(&trace),
            vec![
                (o.pc, AcceptRule::Addressed),
                (o.laptop, AcceptRule::Addressed)
            ]
        );
        Ok(())
    }

    #[test]
    fn broadcast_reaches_everyone_on_the_link_but_the_sender() -> TestResult {
        let mut o = office()?;
        o.world
            .send(o.fileman, o.ipc, "*@ipc".parse()?, Event::new("hi"))?;
        assert_eq!(o.world.tick(), TickOutcome::Completed);
        let got = delivered(&o.world.drain_trace());
        assert_eq!(
            got,
            vec![
                (o.scanner, AcceptRule::Broadcast),
                (o.pc, AcceptRule::Broadcast)
            ]
        );
        Ok(())
    }

    #[test]
    fn multi_hop_ping_reaches_the_app_and_the_pong_comes_back() -> TestResult {
        let mut o = office()?;
        let to = "pc@wifi/fileman@ipc".parse()?;
        o.world
            .send(o.laptop, o.wifi, to, Event::new(events::PING))?;
        for _ in 0..4 {
            assert_eq!(o.world.tick(), TickOutcome::Completed);
        }
        let got = delivered(&o.world.drain_trace());
        assert_eq!(
            got,
            vec![
                (o.pc, AcceptRule::Addressed),      // pc pops its hop...
                (o.fileman, AcceptRule::Addressed), // ...fileman gets the ping, pongs
                (o.pc, AcceptRule::Gateway),        // pong leaves via the gateway...
                (o.laptop, AcceptRule::Addressed),  // ...and reaches the laptop
            ]
        );
        Ok(())
    }

    #[test]
    fn scanner_pings_through_the_gateway_and_gets_replies_back() -> TestResult {
        let mut o = office()?;
        o.world.set_logic_kind(o.scanner, "scanner")?;
        for _ in 0..60 {
            assert_eq!(o.world.tick(), TickOutcome::Completed);
        }
        // The laptop answered at least one scan, and the pong made it back to the scanner.
        assert!(o.world.stats(o.laptop).received >= 1);
        let trace = o.world.drain_trace();
        let back = trace.iter().any(|e| {
            matches!(e, TraceEvent::Delivered { receiver, rule: AcceptRule::Addressed, .. }
                if *receiver == o.scanner)
        });
        assert!(back, "no pong reached the scanner");
        Ok(())
    }

    #[test]
    fn owner_does_not_intercept_multi_hop_packets_for_its_children() -> TestResult {
        // A reply to a nested app travels to its computer first: the computer's parent (which
        // owns the link) must not grab it as a gateway just because later hops go elsewhere.
        let mut o = office()?;
        let house = o.world.network_mut().create_node("house", "building")?;
        let net = o.world.network_mut();
        let lan = net.create_link("lan")?;
        let root = net.root();
        net.disconnect(root, o.pc)?;
        net.disconnect(root, o.laptop)?;
        net.connect(root, house, None)?;
        net.connect(house, o.pc, Some(lan))?;
        net.connect(house, o.laptop, Some(lan))?;
        o.world.set_logic_kind(house, "gateway")?;
        o.world.send(
            o.laptop,
            lan,
            "pc@lan/fileman@ipc".parse()?,
            Event::new("x"),
        )?;
        assert_eq!(o.world.tick(), TickOutcome::Completed);
        assert_eq!(
            delivered(&o.world.drain_trace()),
            vec![(o.pc, AcceptRule::Addressed)]
        );
        Ok(())
    }

    #[test]
    fn child_can_address_its_parent_structurally() -> TestResult {
        let mut o = office()?;
        o.world
            .send(o.fileman, o.ipc, "^@ipc".parse()?, Event::new("up"))?;
        assert_eq!(o.world.tick(), TickOutcome::Completed);
        assert_eq!(
            delivered(&o.world.drain_trace()),
            vec![(o.pc, AcceptRule::ChildToParent)]
        );
        Ok(())
    }

    #[test]
    fn unheard_packets_are_reported() -> TestResult {
        let mut o = office()?;
        o.world
            .send(o.laptop, o.wifi, "nobody@wifi".parse()?, Event::new("x"))?;
        assert_eq!(o.world.tick(), TickOutcome::Completed);
        let dropped = o.world.drain_trace().into_iter().any(|e| {
            matches!(
                e,
                TraceEvent::Dropped {
                    reason: DropReason::NoRecipient,
                    ..
                }
            )
        });
        assert!(dropped);
        Ok(())
    }

    /// Two gateways bouncing a packet between them forever must be stopped by the TTL.
    #[derive(Debug)]
    struct Bouncer;
    impl ControllerLogic for Bouncer {
        fn kind(&self) -> &'static str {
            "bouncer"
        }
        fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>) {
            let _ = ctx.forward(
                packet,
                ctx.arrived_on().unwrap_or_default(),
                packet.to().clone(),
                packet.from().clone(),
            );
        }
    }

    #[test]
    fn loops_are_stopped_by_ttl() -> TestResult {
        let mut o = office()?;
        o.world.set_logic(o.laptop, Box::new(Bouncer))?;
        o.world.set_logic(o.pc, Box::new(Bouncer))?;
        o.world
            .send(o.laptop, o.wifi, "pc@wifi".parse()?, Event::new("loop"))?;
        for _ in 0..40 {
            assert_eq!(o.world.tick(), TickOutcome::Completed);
        }
        let trace = o.world.drain_trace();
        let expired = trace.iter().any(|e| {
            matches!(
                e,
                TraceEvent::Dropped {
                    reason: DropReason::TtlExpired,
                    ..
                }
            )
        });
        assert!(expired);
        let sends = trace
            .iter()
            .filter(|e| matches!(e, TraceEvent::Sent { .. }))
            .count();
        assert!(sends <= usize::from(Packet::DEFAULT_TTL) + 1);
        Ok(())
    }

    #[test]
    fn host_cannot_send_on_a_link_the_node_is_not_on() -> TestResult {
        let mut o = office()?;
        let err = o
            .world
            .send(o.laptop, o.ipc, "pc@ipc".parse()?, Event::new("x"));
        assert_eq!(err, Err(SendError::NotOnLink(o.ipc)));
        Ok(())
    }

    #[test]
    fn logic_kinds_are_validated() -> TestResult {
        let mut o = office()?;
        assert_eq!(
            o.world.set_logic_kind(o.pc, "teleporter"),
            Err(LogicError::UnknownKind("teleporter".into()))
        );
        o.world.set_logic_kind(o.pc, "none")?;
        assert_eq!(o.world.logic_kind(o.pc), None);
        Ok(())
    }

    // ---- Defences ------------------------------------------------------------------------

    use crate::{Check, Severity};

    /// A world with one shared link and `n` named nodes on it, each with `logic`.
    fn on_one_link(
        logics: &[(&str, &str)],
    ) -> Result<(World, LinkId, Vec<NodeId>), Box<dyn std::error::Error>> {
        let mut world = World::new();
        let net = world.network_mut();
        let root = net.root();
        let link = net.create_link("bus")?;
        let mut nodes = Vec::new();
        for (name, _) in logics {
            let n = net.create_node(*name, "device")?;
            net.connect(root, n, Some(link))?;
            nodes.push(n);
        }
        for (&n, (_, logic)) in nodes.iter().zip(logics) {
            world.set_logic_kind(n, logic)?;
        }
        Ok((world, link, nodes))
    }

    fn alerts(trace: &[TraceEvent]) -> Vec<(Check, Severity)> {
        trace
            .iter()
            .filter_map(|e| match e {
                TraceEvent::Alert(a) => Some((a.check, a.severity)),
                _ => None,
            })
            .collect()
    }

    /// Runs up to `ticks`, stopping at the first tripped fuse (as a host would). Returns the
    /// trace and whether the fuse tripped.
    fn run(world: &mut World, ticks: u32) -> (Vec<TraceEvent>, bool) {
        let mut trace = Vec::new();
        let mut tripped = false;
        for _ in 0..ticks {
            let outcome = world.tick();
            trace.extend(world.drain_trace());
            if outcome == TickOutcome::FuseTripped {
                tripped = true;
                break;
            }
        }
        (trace, tripped)
    }

    /// Two links joined by `bridges` bridges, with a sender on the first link.
    fn bridged(bridges: usize) -> Result<(World, NodeId, LinkId), Box<dyn std::error::Error>> {
        let mut world = World::new();
        let net = world.network_mut();
        let root = net.root();
        let (a, b) = (net.create_link("a")?, net.create_link("b")?);
        let sender = net.create_node("sender", "device")?;
        net.connect(root, sender, Some(a))?;
        let mut ids = Vec::new();
        for i in 0..bridges {
            let bridge = net.create_node(format!("bridge-{i}"), "hub")?;
            net.connect(root, bridge, Some(a))?;
            net.subscribe(bridge, b)?;
            ids.push(bridge);
        }
        for id in ids {
            world.set_logic_kind(id, "bridge")?;
        }
        Ok((world, sender, a))
    }

    #[test]
    fn relay_cycle_is_reported_before_any_traffic() -> TestResult {
        let (mut world, _, _) = bridged(2)?;
        let (trace, _) = run(&mut world, 1);
        assert!(alerts(&trace).contains(&(Check::RelayCycle, Severity::Error)));
        assert_eq!(world.health().transmissions, 0);
        Ok(())
    }

    #[test]
    fn two_bridge_loop_is_ended_by_ttl() -> TestResult {
        let (mut world, sender, a) = bridged(2)?;
        world.send(sender, a, "*@a".parse()?, Event::new("hello"))?;
        let (trace, tripped) = run(&mut world, 40);
        assert!(!tripped);
        assert!(alerts(&trace).contains(&(Check::TtlExpired, Severity::Error)));
        assert_eq!(world.health().transmissions, 0, "the loop died out");
        Ok(())
    }

    #[test]
    fn broadcast_storm_trips_the_fuse_within_limits() -> TestResult {
        let (mut world, sender, a) = bridged(4)?;
        world.send(sender, a, "*@a".parse()?, Event::new("hello"))?;
        let (_, tripped) = run(&mut world, 40);
        assert!(tripped, "an exponential storm must trip the fuse");
        let health = world.health();
        assert!(health.peak_transmissions <= world.limits().max_transmissions_per_tick);
        assert!(health.pending <= world.limits().max_pending);
        // Even if the host ignores the fuse and keeps ticking, the queue stays bounded.
        for _ in 0..20 {
            let _ = world.tick();
            assert!(world.health().pending <= world.limits().max_pending);
        }
        let report = world.fuse_report().ok_or("no fuse report")?;
        assert!(!report.top_senders.is_empty());
        assert!(
            report
                .recent_alerts
                .iter()
                .any(|a| a.check == Check::RelayCycle),
            "the report points at the relay loop"
        );
        Ok(())
    }

    #[test]
    fn echo_pair_is_caught_by_replay_not_ttl() -> TestResult {
        let (mut world, bus, nodes) = on_one_link(&[("a", "echo"), ("b", "echo")])?;
        world.send(nodes[0], bus, "b@bus".parse()?, Event::new("ping"))?;
        let (trace, tripped) = run(&mut world, 60);
        let found = alerts(&trace);
        assert!(!tripped);
        assert!(found.iter().any(|&(c, _)| c == Check::Replay));
        assert!(!found.iter().any(|&(c, _)| c == Check::TtlExpired));
        Ok(())
    }

    #[test]
    fn amplifier_is_reported() -> TestResult {
        let (mut world, bus, nodes) = on_one_link(&[
            ("amp", "amplifier"),
            ("r1", "responder"),
            ("r2", "responder"),
        ])?;
        world.send(nodes[1], bus, "amp@bus".parse()?, Event::new("ping"))?;
        let (trace, _) = run(&mut world, 5);
        assert!(alerts(&trace).contains(&(Check::Amplification, Severity::Error)));
        Ok(())
    }

    #[test]
    fn panicking_logic_is_removed_and_the_world_keeps_going() -> TestResult {
        let (mut world, bus, nodes) = on_one_link(&[("boom", "crasher"), ("ok", "responder")])?;
        world.send(nodes[1], bus, "boom@bus".parse()?, Event::new("ping"))?;
        world.send(nodes[0], bus, "ok@bus".parse()?, Event::new("ping"))?;
        let (trace, _) = run(&mut world, 3);
        let panics = alerts(&trace)
            .iter()
            .filter(|a| **a == (Check::LogicPanicked, Severity::Error))
            .count();
        assert_eq!(panics, 1);
        assert_eq!(world.logic_kind(nodes[0]), None);
        // It still sits on the link: it received the ping it crashed on, then the pong.
        assert_eq!(world.stats(nodes[0]).received, 2);
        assert_eq!(world.stats(nodes[1]).received, 1);
        Ok(())
    }

    #[test]
    fn a_full_queue_refuses_sends_and_trips_the_fuse() -> TestResult {
        let (mut world, bus, nodes) = on_one_link(&[("a", "none"), ("b", "none")])?;
        world.set_limits(Limits {
            max_pending: 10,
            ..Limits::default()
        });
        for _ in 0..10 {
            world.send(nodes[0], bus, "b@bus".parse()?, Event::new("x"))?;
        }
        assert_eq!(
            world.send(nodes[0], bus, "b@bus".parse()?, Event::new("x")),
            Err(SendError::QueueFull)
        );
        assert_eq!(world.tick(), TickOutcome::FuseTripped);
        assert_eq!(world.fuse_report().map(|r| r.limit), Some(FuseLimit::Queue));
        Ok(())
    }

    #[test]
    fn oversized_payloads_are_refused() -> TestResult {
        let (mut world, bus, nodes) = on_one_link(&[("a", "none"), ("b", "none")])?;
        let big = vec![0_u8; world.limits().max_payload_bytes + 1];
        assert_eq!(
            world.send(nodes[0], bus, "b@bus".parse()?, Event::with_data("x", big)),
            Err(SendError::PayloadTooLarge)
        );
        Ok(())
    }

    #[test]
    fn packets_from_a_node_that_left_the_link_are_dropped() -> TestResult {
        let (mut world, bus, nodes) = on_one_link(&[("a", "none"), ("b", "none")])?;
        world.send(nodes[0], bus, "b@bus".parse()?, Event::new("x"))?;
        let root = world.network().root();
        world.network_mut().disconnect(root, nodes[0])?;
        assert_eq!(world.tick(), TickOutcome::Completed);
        assert_eq!(world.stats(nodes[1]).received, 0);
        assert_eq!(world.health().sender_left_drops, 1);
        Ok(())
    }

    #[test]
    fn normal_traffic_raises_no_alerts() -> TestResult {
        let mut o = office()?;
        o.world.set_logic_kind(o.scanner, "scanner")?;
        let (trace, tripped) = run(&mut o.world, 300);
        assert!(!tripped);
        assert_eq!(alerts(&trace), Vec::new());
        Ok(())
    }

    #[test]
    fn gateways_alone_never_count_as_a_relay_loop() -> TestResult {
        // Two buildings on the same two world links: a cycle of gateways, but gateways follow
        // routes, so it cannot loop. One building also plugs into its own LAN.
        let mut world = World::new();
        let net = world.network_mut();
        let root = net.root();
        let (fibre, street) = (net.create_link("fibre")?, net.create_link("street")?);
        let mut buildings = Vec::new();
        for i in 0..2 {
            let b = net.create_node(format!("building-{i}"), "building")?;
            net.connect(root, b, Some(fibre))?;
            net.subscribe(b, street)?;
            let lan = net.create_link("lan")?;
            net.add_internal_link(b, lan)?;
            buildings.push((b, lan));
        }
        net.subscribe(buildings[0].0, buildings[0].1)?;
        for (b, _) in buildings {
            world.set_logic_kind(b, "gateway")?;
        }
        let (trace, _) = run(&mut world, 1);
        assert_eq!(alerts(&trace), Vec::new());
        Ok(())
    }

    #[test]
    fn a_bridge_completing_a_loop_through_a_gateway_is_caught() -> TestResult {
        let mut world = World::new();
        let net = world.network_mut();
        let root = net.root();
        let wifi = net.create_link("wifi")?;
        let pc = net.create_node("pc", "computer")?;
        net.connect(root, pc, Some(wifi))?;
        let ipc = net.create_link("ipc")?;
        let patch = net.create_node("patch", "hub")?;
        net.connect(pc, patch, Some(ipc))?;
        net.subscribe(patch, wifi)?;
        world.set_logic_kind(pc, "gateway")?;
        world.set_logic_kind(patch, "bridge")?;
        let (trace, _) = run(&mut world, 1);
        assert!(alerts(&trace).contains(&(Check::RelayCycle, Severity::Error)));
        Ok(())
    }
}
