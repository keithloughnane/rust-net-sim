//! Health counters, fuse reports and alerts as the native library returns them. Mirrors
//! `crates/emergence-ffi/src/health.rs`, keeping only the fields the sandbox uses.

use serde::Deserialize;

use crate::native::{LinkId, NodeId};

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Health {
    pub(crate) tick: u64,
    pub(crate) transmissions: u64,
    pub(crate) peak_transmissions: u64,
    pub(crate) pending: u64,
    pub(crate) refused_sends: u64,
    pub(crate) ttl_drops: u64,
    pub(crate) sender_left_drops: u64,
    pub(crate) alerts: u64,
    pub(crate) fuse_trips: u64,
    pub(crate) trace_len: u64,
    pub(crate) trace_discarded: u64,
    pub(crate) limits: Limits,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(clippy::struct_field_names)] // Same names as the library's.
pub(crate) struct Limits {
    pub(crate) max_transmissions_per_tick: u64,
    pub(crate) max_deliveries_per_tick: u64,
    pub(crate) max_pending: u64,
    pub(crate) max_payload_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Subject {
    Node(NodeId),
    Link(LinkId),
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Alert {
    pub(crate) tick: u64,
    /// Check name, such as `relay_cycle`.
    pub(crate) check: String,
    pub(crate) severity: Severity,
    /// `None` means the whole network.
    pub(crate) subject: Option<Subject>,
    pub(crate) message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FuseReport {
    pub(crate) tick: u64,
    /// Which limit: `transmissions`, `deliveries` or `queue`.
    pub(crate) limit: String,
    pub(crate) max: u64,
    pub(crate) held_back: u64,
    pub(crate) top_senders: Vec<(NodeId, u64)>,
    pub(crate) top_links: Vec<(LinkId, u64)>,
    pub(crate) recent_alerts: Vec<Alert>,
}

impl FuseReport {
    /// What the limit means, for people.
    pub(crate) fn limit_text(&self) -> String {
        match self.limit.as_str() {
            "transmissions" => format!("{} packets sent in one tick", self.max),
            "deliveries" => format!("{} packet deliveries in one tick", self.max),
            "queue" => format!("{} packets waiting (new sends were refused)", self.max),
            other => format!("{other} limit of {}", self.max),
        }
    }
}
