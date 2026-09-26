use std::fmt;
use std::sync::Arc;

use crate::route::{Hop, names};
use crate::trace::{TraceEvent, TraceLog};
use crate::{ControlNode, Event, Limits, Link, LinkId, Network, NodeId, Packet, PacketRoute};

/// Behaviour attached to a [`ControlNode`]: what it does when packets arrive and as time passes.
///
/// Implementations never touch links, queues or scheduling. Everything they send through the
/// [`Context`] is queued and delivered on the next [`World::tick`](crate::World::tick), which
/// keeps delivery deterministic and stops a reply from cascading inside the same tick.
pub trait ControllerLogic: Send + fmt::Debug {
    /// Identifies the logic, such as `"responder"`. Used by hosts, the UI and save files.
    fn kind(&self) -> &'static str;

    /// Called once when the logic is attached to a node.
    fn on_start(&mut self, _ctx: &mut Context<'_>) {}

    /// Called once per tick, before that tick's packets are delivered.
    fn on_tick(&mut self, _ctx: &mut Context<'_>) {}

    /// Called for every packet this node accepts. [`Context::accepted_by`] says why it was
    /// accepted and [`Context::arrived_on`] which link it came in on.
    fn on_received(&mut self, packet: &Packet, ctx: &mut Context<'_>);

    /// Opt in to packets the standard accept rules would ignore. Use sparingly.
    fn wants(&self, _packet: &Packet) -> bool {
        false
    }

    /// How this logic passes traffic between the links its node is on. The monitor uses it to
    /// find loops in the topology before any traffic flows.
    fn relaying(&self) -> Relaying {
        Relaying::None
    }
}

/// How a logic passes traffic between links, for loop detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Relaying {
    /// Never passes traffic on.
    None,
    /// Passes traffic between its internal and external links only where a packet's route says
    /// to (a gateway). Route depth bounds this, so routed nodes alone cannot make an endless
    /// loop, but they can complete one that a flooding relay starts.
    Routed,
    /// Passes everything it hears to its other links (a hub or bridge). Any cycle through one of
    /// these loops.
    Flooding,
}

/// Why a node accepted a packet (see the routing rules in the design docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcceptRule {
    /// Addressed to this node by name.
    Addressed,
    /// Addressed to everyone (`*`).
    Broadcast,
    /// Sent by one of this node's children to `^` on a link this node owns.
    ChildToParent,
    /// Sent on a link this node owns, headed for a link it does not: this node is the gateway.
    Gateway,
    /// Accepted because the node's logic asked for it ([`ControllerLogic::wants`]).
    Forced,
}

/// Why a send was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// The sending node does not exist.
    UnknownNode(NodeId),
    /// The link does not exist.
    UnknownLink(LinkId),
    /// The node is neither subscribed to the link nor its owner.
    NotOnLink(LinkId),
    /// [`Context::reply`] was called outside [`ControllerLogic::on_received`].
    NothingToReplyTo,
    /// The route would be too deep.
    Route(crate::RouteError),
    /// The payload or event kind is bigger than [`Limits`] allow.
    PayloadTooLarge,
    /// The send queue is full ([`Limits::max_pending`]). The world is overloaded; the packet was
    /// not sent.
    QueueFull,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "unknown node {id:?}"),
            Self::UnknownLink(id) => write!(f, "unknown link {id:?}"),
            Self::NotOnLink(id) => write!(f, "the node is not on link {id:?}"),
            Self::NothingToReplyTo => f.write_str("there is no received packet to reply to"),
            Self::Route(e) => write!(f, "bad route: {e}"),
            Self::PayloadTooLarge => f.write_str("the payload or event kind is too large"),
            Self::QueueFull => f.write_str("the send queue is full"),
        }
    }
}

impl std::error::Error for SendError {}

/// A packet waiting for the next tick.
#[derive(Debug, Clone)]
pub(crate) struct Outgoing {
    pub(crate) sender: NodeId,
    pub(crate) via: LinkId,
    pub(crate) from: Arc<PacketRoute>,
    pub(crate) to: Arc<PacketRoute>,
    pub(crate) event: Arc<Event>,
    pub(crate) ttl: u8,
    pub(crate) trace: Vec<NodeId>,
}

/// Longest event kind allowed, in bytes.
pub(crate) const MAX_KIND_LEN: usize = 64;

/// Builds an [`Outgoing`] from `sender` on `via`, checking the sender may use the link and the
/// event fits the limits.
pub(crate) fn outgoing(
    network: &Network,
    limits: &Limits,
    sender: NodeId,
    via: LinkId,
    from: Option<Arc<PacketRoute>>,
    to: Arc<PacketRoute>,
    event: Arc<Event>,
) -> Result<Outgoing, SendError> {
    let node = network.node(sender).ok_or(SendError::UnknownNode(sender))?;
    let link = network.link(via).ok_or(SendError::UnknownLink(via))?;
    if !network.can_use_link(sender, via) {
        return Err(SendError::NotOnLink(via));
    }
    if event.data.len() > limits.max_payload_bytes || event.kind.len() > MAX_KIND_LEN {
        return Err(SendError::PayloadTooLarge);
    }
    Ok(Outgoing {
        sender,
        via,
        from: from.unwrap_or_else(|| Arc::new(PacketRoute::new(node.name(), link.name()))),
        to,
        event,
        ttl: Packet::DEFAULT_TTL,
        trace: Vec::new(),
    })
}

/// The queue of packets waiting for the next tick, with its size limit.
#[derive(Debug)]
pub(crate) struct SendQueue<'a> {
    pub(crate) items: &'a mut Vec<Outgoing>,
    pub(crate) limits: &'a Limits,
    /// Packets from the current batch not yet delivered, which count against the limit.
    pub(crate) reserved: usize,
    /// Sends refused because the queue was full.
    pub(crate) refused: &'a mut u64,
}

impl SendQueue<'_> {
    pub(crate) fn push(&mut self, out: Outgoing) -> Result<(), SendError> {
        if self.items.len() + self.reserved >= self.limits.max_pending {
            *self.refused += 1;
            return Err(SendError::QueueFull);
        }
        self.items.push(out);
        Ok(())
    }
}

/// What a [`ControllerLogic`] can see and do while it runs.
#[derive(Debug)]
pub struct Context<'a> {
    pub(crate) network: &'a Network,
    pub(crate) node: NodeId,
    pub(crate) tick: u64,
    pub(crate) arrived_on: Option<LinkId>,
    pub(crate) accepted_by: Option<AcceptRule>,
    pub(crate) queue: SendQueue<'a>,
    pub(crate) trace: &'a mut TraceLog,
}

impl Context<'_> {
    /// The node this logic belongs to.
    #[must_use]
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// This node's name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.network.node(self.node).map_or("", ControlNode::name)
    }

    /// The current tick.
    #[must_use]
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// Read-only view of the whole network.
    #[must_use]
    pub fn network(&self) -> &Network {
        self.network
    }

    /// The link the packet being handled arrived on. `None` outside `on_received`.
    #[must_use]
    pub fn arrived_on(&self) -> Option<LinkId> {
        self.arrived_on
    }

    /// Why the packet being handled was accepted. `None` outside `on_received`.
    #[must_use]
    pub fn accepted_by(&self) -> Option<AcceptRule> {
        self.accepted_by
    }

    /// A link this node subscribes to (from outside), by name. `?` means its first one.
    #[must_use]
    pub fn external_link_named(&self, name: &str) -> Option<LinkId> {
        let node = self.network.node(self.node)?;
        let subs = node.subscriptions();
        if name == names::ANY_LINK {
            return subs.first().copied();
        }
        subs.iter()
            .copied()
            .find(|&l| self.network.link(l).is_some_and(|l| l.name() == name))
    }

    /// One of this node's own internal links, by name.
    #[must_use]
    pub fn internal_link_named(&self, name: &str) -> Option<LinkId> {
        let node = self.network.node(self.node)?;
        node.internal_links()
            .iter()
            .copied()
            .find(|&l| self.network.link(l).is_some_and(|l| l.name() == name))
    }

    /// Sends `event` to `to` over `via`, from this node's own address on that link.
    ///
    /// # Errors
    ///
    /// Fails if this node can neither subscribe to nor own `via`.
    pub fn send(&mut self, via: LinkId, to: PacketRoute, event: Event) -> Result<(), SendError> {
        let out = outgoing(
            self.network,
            self.queue.limits,
            self.node,
            via,
            None,
            Arc::new(to),
            Arc::new(event),
        )?;
        self.queue.push(out)
    }

    /// Replies to `packet` with `event`, on the link it arrived on.
    ///
    /// # Errors
    ///
    /// Fails outside `on_received`.
    pub fn reply(&mut self, packet: &Packet, event: Event) -> Result<(), SendError> {
        let via = self.arrived_on.ok_or(SendError::NothingToReplyTo)?;
        let out = outgoing(
            self.network,
            self.queue.limits,
            self.node,
            via,
            None,
            Arc::clone(&packet.from),
            Arc::new(event),
        )?;
        self.queue.push(out)
    }

    /// Re-sends `packet`'s event with new addresses, keeping its remaining TTL and trace so loop
    /// protection carries through relays.
    ///
    /// # Errors
    ///
    /// Fails if this node can neither subscribe to nor own `via`.
    pub fn forward(
        &mut self,
        packet: &Packet,
        via: LinkId,
        from: PacketRoute,
        to: PacketRoute,
    ) -> Result<(), SendError> {
        let mut out = outgoing(
            self.network,
            self.queue.limits,
            self.node,
            via,
            Some(Arc::new(from)),
            Arc::new(to),
            Arc::clone(&packet.event),
        )?;
        out.ttl = packet.ttl();
        out.trace = packet.trace().to_vec();
        self.queue.push(out)
    }

    /// Re-sends `packet` unchanged (same addresses, event, TTL and trace) on another link, the
    /// way a hub or bridge passes traffic through.
    ///
    /// # Errors
    ///
    /// Fails if this node can neither subscribe to nor own `via`.
    pub fn relay(&mut self, packet: &Packet, via: LinkId) -> Result<(), SendError> {
        let mut out = outgoing(
            self.network,
            self.queue.limits,
            self.node,
            via,
            Some(Arc::clone(&packet.from)),
            Arc::clone(&packet.to),
            Arc::clone(&packet.event),
        )?;
        out.ttl = packet.ttl();
        out.trace = packet.trace().to_vec();
        self.queue.push(out)
    }

    /// `route` with this node's own address on `via` added in front, so replies come back
    /// through this node.
    ///
    /// # Errors
    ///
    /// Fails if the route would be too deep.
    pub fn via_me(&self, via: LinkId, route: &PacketRoute) -> Result<PacketRoute, SendError> {
        let link = self.network.link(via).map_or("", Link::name);
        route
            .prepended(Hop::new(self.name(), link))
            .map_err(SendError::Route)
    }

    /// Records a human-readable note in the trace, for debugging and the UI.
    pub fn note(&mut self, text: impl Into<String>) {
        self.trace.push(TraceEvent::Note {
            tick: self.tick,
            node: self.node,
            text: text.into(),
        });
    }
}
