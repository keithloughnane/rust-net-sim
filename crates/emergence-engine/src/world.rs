use std::fmt;

use slotmap::SecondaryMap;

use crate::logic::{Outgoing, outgoing};
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

/// A self-contained simulation: a [`Network`], the logic attached to its nodes, and the clock.
///
/// The host owns the clock. Nothing moves between calls to [`World::tick`]: each tick runs
/// every logic's `on_tick`, then delivers everything that was sent before the tick began.
/// Anything sent while handling a packet waits for the next tick, so packets advance one hop
/// per tick and a tick always finishes.
///
/// ```
/// use emergence_engine::{Event, PacketRoute, World};
///
/// let mut world = World::new();
/// let net = world.network_mut();
/// let wifi = net.create_link("wifi");
/// let (a, b) = (net.create_node("a", "device"), net.create_node("b", "device"));
/// let root = net.root();
/// net.connect(root, a, Some(wifi))?;
/// net.connect(root, b, Some(wifi))?;
/// world.set_logic_kind(b, "responder")?;
///
/// world.send(a, wifi, PacketRoute::new("b", "wifi"), Event::new("ping"))?;
/// world.tick(); // b receives the ping and queues a pong
/// world.tick(); // a receives the pong
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
}

impl World {
    /// Creates a world with an empty network, at tick zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        &mut self.network
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
            pending: &mut self.pending,
            trace: &mut self.trace,
        };
        logic.on_start(&mut ctx);
        self.logics.insert(node, logic);
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
    /// Fails if the node or link does not exist, or the node is not on the link.
    pub fn send(
        &mut self,
        node: NodeId,
        via: LinkId,
        to: PacketRoute,
        event: Event,
    ) -> Result<(), SendError> {
        let out = outgoing(&self.network, node, via, None, to, event)?;
        self.pending.push(out);
        Ok(())
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

    /// Advances the simulation by one tick.
    pub fn tick(&mut self) {
        self.tick_count += 1;
        let tick = self.tick_count;

        for (node, logic) in &mut self.logics {
            let mut ctx = Context {
                network: &self.network,
                node,
                tick,
                arrived_on: None,
                accepted_by: None,
                pending: &mut self.pending,
                trace: &mut self.trace,
            };
            logic.on_tick(&mut ctx);
        }

        // Deliver what was queued before this point; anything sent while delivering waits for
        // the next tick.
        let batch = std::mem::take(&mut self.pending);
        for out in batch {
            self.transmit(tick, out);
        }
    }

    /// Puts one packet on its link and hands a copy to every listener that accepts it.
    fn transmit(&mut self, tick: u64, out: Outgoing) {
        let Some(link) = self.network.link(out.via) else {
            return; // Links are never removed yet, so this cannot happen today.
        };
        self.next_packet += 1;
        let packet = Packet {
            id: PacketId(self.next_packet),
            from: out.from,
            to: out.to,
            event: out.event,
            ttl: out.ttl,
            trace: out.trace,
        };
        if let Some(e) = self.stats.entry(out.sender) {
            e.or_default().sent += 1;
        }
        self.trace.push(TraceEvent::Sent {
            tick,
            packet: packet.clone(),
            sender: out.sender,
            link: out.via,
        });

        let mut heard = false;
        for listener in listeners(&self.network, link, out.sender) {
            let wants = self.logics.get(listener).is_some_and(|l| l.wants(&packet));
            let Some(rule) = accepts(&self.network, listener, &packet, link)
                .or(wants.then_some(AcceptRule::Forced))
            else {
                continue;
            };
            heard = true;

            let mut copy = packet.clone();
            copy.ttl = copy.ttl.saturating_sub(1);
            copy.trace.push(listener);
            if copy.ttl == 0 {
                self.trace.push(TraceEvent::Dropped {
                    tick,
                    packet: packet.id,
                    at: Some(listener),
                    reason: DropReason::TtlExpired,
                });
                continue;
            }

            if let Some(e) = self.stats.entry(listener) {
                e.or_default().received += 1;
            }
            self.trace.push(TraceEvent::Delivered {
                tick,
                packet: packet.id,
                receiver: listener,
                link: out.via,
                rule,
            });
            if let Some(logic) = self.logics.get_mut(listener) {
                let mut ctx = Context {
                    network: &self.network,
                    node: listener,
                    tick,
                    arrived_on: Some(out.via),
                    accepted_by: Some(rule),
                    pending: &mut self.pending,
                    trace: &mut self.trace,
                };
                logic.on_received(&copy, &mut ctx);
            }
        }
        if !heard {
            self.trace.push(TraceEvent::Dropped {
                tick,
                packet: packet.id,
                at: None,
                reason: DropReason::NoRecipient,
            });
        }
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
        let wifi = net.create_link("wifi");
        let ipc = net.create_link("ipc");
        let laptop = net.create_node("laptop", "computer");
        let pc = net.create_node("pc", "computer");
        let fileman = net.create_node("fileman", "app");
        let scanner = net.create_node("scanner", "app");
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
        o.world.tick();
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
        o.world.tick();
        o.world.tick();
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
        o.world.tick();
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
            o.world.tick();
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
            o.world.tick();
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
        let house = o.world.network_mut().create_node("house", "building");
        let net = o.world.network_mut();
        let lan = net.create_link("lan");
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
        o.world.tick();
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
        o.world.tick();
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
        o.world.tick();
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
            o.world.tick();
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
}
