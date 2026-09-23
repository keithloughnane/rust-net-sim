use crate::{NodeId, PacketRoute};

/// The payload of a packet: a kind that says what happened, plus opaque bytes.
///
/// The engine never looks inside `data`. Hosts choose their own encoding (JSON, a binary
/// struct, nothing at all).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Event {
    /// What the event is, such as `"ping"`. Logic dispatches on this.
    pub kind: String,
    /// Event-specific payload.
    pub data: Vec<u8>,
}

impl Event {
    /// An event with no payload.
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            data: Vec::new(),
        }
    }

    /// An event with a payload.
    pub fn with_data(kind: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            kind: kind.into(),
            data: data.into(),
        }
    }
}

/// Identifies one transmission of a packet onto a link. Every copy a link hands to its
/// listeners shares the ID; a forwarded packet gets a new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PacketId(pub(crate) u64);

impl PacketId {
    /// The ID as a plain integer, for passing across an FFI boundary.
    #[must_use]
    pub fn to_raw(self) -> u64 {
        self.0
    }
}

/// A message travelling through the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub(crate) id: PacketId,
    pub(crate) from: PacketRoute,
    pub(crate) to: PacketRoute,
    pub(crate) event: Event,
    pub(crate) ttl: u8,
    pub(crate) trace: Vec<NodeId>,
}

impl Packet {
    /// Hop budget a fresh packet starts with.
    pub const DEFAULT_TTL: u8 = 16;

    /// This transmission's ID.
    #[must_use]
    pub fn id(&self) -> PacketId {
        self.id
    }

    /// The sender's address, for replying.
    #[must_use]
    pub fn from(&self) -> &PacketRoute {
        &self.from
    }

    /// The destination address.
    #[must_use]
    pub fn to(&self) -> &PacketRoute {
        &self.to
    }

    /// The payload.
    #[must_use]
    pub fn event(&self) -> &Event {
        &self.event
    }

    /// Hops left before the packet is dropped.
    #[must_use]
    pub fn ttl(&self) -> u8 {
        self.ttl
    }

    /// Every node that has accepted this packet (or its forwarded ancestors), oldest first.
    #[must_use]
    pub fn trace(&self) -> &[NodeId] {
        &self.trace
    }
}
