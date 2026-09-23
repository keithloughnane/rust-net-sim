//! The trace the native library returns as JSON. Mirrors `crates/emergence-ffi/src/trace.rs`,
//! keeping only the fields the sandbox uses; the rest are ignored.

use serde::Deserialize;

use crate::native::{LinkId, NodeId};

/// Trace format version this module understands.
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TraceEntry {
    Sent {
        tick: u64,
        packet: u64,
        sender: NodeId,
        link: LinkId,
        to: String,
        kind: String,
        data: Option<String>,
    },
    Delivered {
        packet: u64,
        receiver: NodeId,
        rule: String,
    },
    Dropped {
        packet: u64,
        at: Option<NodeId>,
        reason: String,
    },
    Note {
        tick: u64,
        node: NodeId,
        text: String,
    },
    /// Anything newer than this sandbox understands.
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct Trace {
    format: u32,
    events: Vec<TraceEntry>,
}

pub(crate) fn parse(json: &[u8]) -> Result<Vec<TraceEntry>, serde_json::Error> {
    let trace: Trace = serde_json::from_slice(json)?;
    if trace.format != FORMAT {
        return Err(serde::de::Error::custom(format!(
            "trace format {} is not supported (expected {FORMAT})",
            trace.format
        )));
    }
    Ok(trace.events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_entry_type_and_tolerates_new_ones() -> Result<(), serde_json::Error> {
        let json = br#"{"format":1,"discarded":0,"events":[
            {"type":"sent","tick":1,"packet":7,"sender":2,"link":3,"from":"a@w","to":"*@w","kind":"ping","data":"t1","data_len":2,"ttl":16},
            {"type":"delivered","tick":1,"packet":7,"receiver":4,"link":3,"rule":"broadcast"},
            {"type":"dropped","tick":1,"packet":7,"at":null,"reason":"no_recipient"},
            {"type":"note","tick":1,"node":2,"text":"hi"},
            {"type":"teleported","tick":1}]}"#;
        let events = parse(json)?;
        assert_eq!(events.len(), 5);
        assert!(matches!(events[4], TraceEntry::Other));
        Ok(())
    }
}
