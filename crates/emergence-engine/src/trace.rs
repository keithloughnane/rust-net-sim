use std::collections::VecDeque;

use crate::{AcceptRule, Alert, LinkId, NodeId, Packet, PacketId};

/// Why a packet copy went nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DropReason {
    /// Its hop budget ran out: usually a routing loop.
    TtlExpired,
    /// Nobody on the link accepted it.
    NoRecipient,
    /// The sender left the link after queueing it, so it was never transmitted.
    SenderLeftLink,
}

/// Something that happened during a tick, for debugging tools and visualisers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TraceEvent {
    /// `sender` put `packet` on `link`.
    Sent {
        /// Tick it happened on.
        tick: u64,
        /// The packet as transmitted.
        packet: Packet,
        /// The node that transmitted it.
        sender: NodeId,
        /// The link it went onto.
        link: LinkId,
    },
    /// `receiver` accepted a copy of a packet.
    Delivered {
        /// Tick it happened on.
        tick: u64,
        /// The transmission this copy came from.
        packet: PacketId,
        /// The node that accepted it.
        receiver: NodeId,
        /// The link it arrived on.
        link: LinkId,
        /// Why it was accepted.
        rule: AcceptRule,
    },
    /// A packet, or one copy of it, was discarded.
    Dropped {
        /// Tick it happened on.
        tick: u64,
        /// The transmission involved.
        packet: PacketId,
        /// The node that dropped it, if one did.
        at: Option<NodeId>,
        /// Why.
        reason: DropReason,
    },
    /// A monitor check noticed something. Always recorded, even when packet tracing is off.
    Alert(Alert),
    /// A note written by a node's logic.
    Note {
        /// Tick it happened on.
        tick: u64,
        /// The node whose logic wrote it.
        node: NodeId,
        /// The note.
        text: String,
    },
}

/// Bounded buffer of trace events, so a host that never drains it cannot run out of memory.
#[derive(Debug, Default)]
pub(crate) struct TraceLog {
    events: VecDeque<TraceEvent>,
    discarded: u64,
}

impl TraceLog {
    const CAPACITY: usize = 100_000;

    pub(crate) fn push(&mut self, event: TraceEvent) {
        if self.events.len() == Self::CAPACITY {
            self.events.pop_front();
            self.discarded += 1;
        }
        self.events.push_back(event);
    }

    pub(crate) fn drain(&mut self) -> Vec<TraceEvent> {
        self.events.drain(..).collect()
    }

    pub(crate) fn discarded(&self) -> u64 {
        self.discarded
    }

    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }
}
