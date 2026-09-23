//! JSON form of the trace (what happened during recent ticks), for debugging tools and UIs.
//!
//! This is part of the ABI: bump [`FORMAT`] on any breaking change to the shape below.

use emergence_engine::{AcceptRule, DropReason, TraceEvent};
use serde::Serialize;

/// Version of the trace format.
pub(crate) const FORMAT: u32 = 2;

#[derive(Serialize)]
pub(crate) struct Trace {
    format: u32,
    /// Events lost because the buffer filled before it was drained.
    discarded: u64,
    events: Vec<Entry>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Entry {
    Sent {
        tick: u64,
        packet: u64,
        sender: u64,
        link: u64,
        from: String,
        to: String,
        kind: String,
        /// The payload as text, if it is valid UTF-8.
        data: Option<String>,
        data_len: usize,
        ttl: u8,
    },
    Delivered {
        tick: u64,
        packet: u64,
        receiver: u64,
        link: u64,
        rule: &'static str,
    },
    Dropped {
        tick: u64,
        packet: u64,
        at: Option<u64>,
        reason: &'static str,
    },
    Note {
        tick: u64,
        node: u64,
        text: String,
    },
    Alert(crate::health::AlertJson),
    /// An event kind this ABI version cannot describe.
    Other,
}

fn rule_name(rule: AcceptRule) -> &'static str {
    match rule {
        AcceptRule::Addressed => "addressed",
        AcceptRule::Broadcast => "broadcast",
        AcceptRule::ChildToParent => "child_to_parent",
        AcceptRule::Gateway => "gateway",
        AcceptRule::Forced => "forced",
        _ => "other",
    }
}

fn reason_name(reason: DropReason) -> &'static str {
    match reason {
        DropReason::TtlExpired => "ttl_expired",
        DropReason::NoRecipient => "no_recipient",
        DropReason::SenderLeftLink => "sender_left_link",
        _ => "other",
    }
}

impl Trace {
    pub(crate) fn of(events: Vec<TraceEvent>, discarded: u64) -> Self {
        let events = events
            .into_iter()
            .map(|e| match e {
                TraceEvent::Sent {
                    tick,
                    packet,
                    sender,
                    link,
                } => Entry::Sent {
                    tick,
                    packet: packet.id().to_raw(),
                    sender: sender.to_raw(),
                    link: link.to_raw(),
                    from: packet.from().to_string(),
                    to: packet.to().to_string(),
                    kind: packet.event().kind.clone(),
                    data: String::from_utf8(packet.event().data.clone()).ok(),
                    data_len: packet.event().data.len(),
                    ttl: packet.ttl(),
                },
                TraceEvent::Delivered {
                    tick,
                    packet,
                    receiver,
                    link,
                    rule,
                } => Entry::Delivered {
                    tick,
                    packet: packet.to_raw(),
                    receiver: receiver.to_raw(),
                    link: link.to_raw(),
                    rule: rule_name(rule),
                },
                TraceEvent::Dropped {
                    tick,
                    packet,
                    at,
                    reason,
                } => Entry::Dropped {
                    tick,
                    packet: packet.to_raw(),
                    at: at.map(emergence_engine::NodeId::to_raw),
                    reason: reason_name(reason),
                },
                TraceEvent::Alert(a) => Entry::Alert(crate::health::alert(&a)),
                TraceEvent::Note { tick, node, text } => Entry::Note {
                    tick,
                    node: node.to_raw(),
                    text,
                },
                _ => Entry::Other,
            })
            .collect();
        Self {
            format: FORMAT,
            discarded,
            events,
        }
    }
}
