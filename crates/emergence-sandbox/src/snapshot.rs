//! The network snapshot the native library returns as JSON, parsed and indexed for the UI.
//!
//! Mirrors `crates/emergence-ffi/src/snapshot.rs` the way a game engine's bindings would.

use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;

use crate::native::{LinkId, NodeId};

/// Snapshot format version this module understands.
const FORMAT: u32 = 2;

#[derive(Debug, Deserialize)]
pub(crate) struct NodeEntry {
    pub(crate) id: NodeId,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) parent: Option<NodeId>,
    pub(crate) children: Vec<NodeId>,
    pub(crate) internal_links: Vec<LinkId>,
    pub(crate) subscriptions: Vec<LinkId>,
    pub(crate) logic: Option<String>,
    pub(crate) sent: u64,
    pub(crate) received: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LinkEntry {
    pub(crate) id: LinkId,
    pub(crate) name: String,
    pub(crate) owner: Option<NodeId>,
    pub(crate) subscribers: Vec<NodeId>,
}

#[derive(Debug, Deserialize)]
struct Raw {
    format: u32,
    tick: u64,
    root: NodeId,
    nodes: Vec<NodeEntry>,
    links: Vec<LinkEntry>,
}

#[derive(Debug)]
pub(crate) enum SnapshotError {
    Json(serde_json::Error),
    UnsupportedFormat(u32),
    MissingRoot,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "invalid JSON: {e}"),
            Self::UnsupportedFormat(v) => {
                write!(
                    f,
                    "snapshot format {v} is not supported (expected {FORMAT})"
                )
            }
            Self::MissingRoot => f.write_str("the root node is missing"),
        }
    }
}

/// A read-only copy of the whole network, with lookups by ID.
#[derive(Debug)]
pub(crate) struct Snapshot {
    tick: u64,
    root: NodeId,
    nodes: Vec<NodeEntry>,
    links: Vec<LinkEntry>,
    node_index: HashMap<NodeId, usize>,
    link_index: HashMap<LinkId, usize>,
}

impl Snapshot {
    pub(crate) fn from_json(json: &[u8]) -> Result<Self, SnapshotError> {
        let raw: Raw = serde_json::from_slice(json).map_err(SnapshotError::Json)?;
        if raw.format != FORMAT {
            return Err(SnapshotError::UnsupportedFormat(raw.format));
        }
        let node_index = raw
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id, i))
            .collect();
        let link_index = raw
            .links
            .iter()
            .enumerate()
            .map(|(i, l)| (l.id, i))
            .collect();
        let snapshot = Self {
            tick: raw.tick,
            root: raw.root,
            nodes: raw.nodes,
            links: raw.links,
            node_index,
            link_index,
        };
        if snapshot.node(snapshot.root).is_none() {
            return Err(SnapshotError::MissingRoot);
        }
        Ok(snapshot)
    }

    pub(crate) fn tick(&self) -> u64 {
        self.tick
    }

    /// The child of `scope` that is `node` or contains it, if `node` is below `scope`.
    pub(crate) fn child_of_scope_containing(&self, scope: NodeId, node: NodeId) -> Option<NodeId> {
        let path = self.path_to(node);
        let i = path.iter().position(|&n| n == scope)?;
        path.get(i + 1).copied()
    }

    pub(crate) fn root(&self) -> NodeId {
        self.root
    }

    pub(crate) fn node(&self, id: NodeId) -> Option<&NodeEntry> {
        self.node_index.get(&id).map(|&i| &self.nodes[i])
    }

    pub(crate) fn link(&self, id: LinkId) -> Option<&LinkEntry> {
        self.link_index.get(&id).map(|&i| &self.links[i])
    }

    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub(crate) fn link_count(&self) -> usize {
        self.links.len()
    }

    pub(crate) fn node_name(&self, id: NodeId) -> &str {
        self.node(id).map_or("?", |n| n.name.as_str())
    }

    pub(crate) fn link_name(&self, id: LinkId) -> &str {
        self.link(id).map_or("?", |l| l.name.as_str())
    }

    /// How many nodes have this name.
    pub(crate) fn nodes_named(&self, name: &str) -> usize {
        self.nodes.iter().filter(|n| n.name == name).count()
    }

    /// The first link (in creation order) with this name.
    pub(crate) fn find_link(&self, name: &str) -> Option<LinkId> {
        self.links.iter().find(|l| l.name == name).map(|l| l.id)
    }

    /// The first node (in creation order) with this name.
    pub(crate) fn find_node(&self, name: &str) -> Option<NodeId> {
        self.nodes.iter().find(|n| n.name == name).map(|n| n.id)
    }

    /// Root first, down to and including `id`.
    pub(crate) fn path_to(&self, id: NodeId) -> Vec<NodeId> {
        let mut path: Vec<NodeId> =
            std::iter::successors(Some(id), |&n| self.node(n).and_then(|n| n.parent))
                .take(self.nodes.len())
                .collect();
        path.reverse();
        path
    }

    /// Number of nodes nested anywhere below `id`. Iterative, so a player-built chain thousands
    /// of levels deep cannot overflow the stack.
    pub(crate) fn descendant_count(&self, id: NodeId) -> usize {
        let mut count = 0;
        let mut stack: Vec<NodeId> = self
            .node(id)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        while let Some(n) = stack.pop() {
            count += 1;
            if let Some(node) = self.node(n) {
                stack.extend(&node.children);
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_indexes_a_snapshot() -> Result<(), SnapshotError> {
        let json = br#"{"format":2,"tick":0,"root":1,
            "nodes":[{"id":1,"name":".","kind":"root","parent":null,"children":[2],"internal_links":[9],"subscriptions":[],"logic":null,"sent":0,"received":0},
                     {"id":2,"name":"pc","kind":"computer","parent":1,"children":[],"internal_links":[],"subscriptions":[9],"logic":"gateway","sent":1,"received":2}],
            "links":[{"id":9,"name":"wifi","owner":1,"subscribers":[2]}]}"#;
        let snap = Snapshot::from_json(json)?;
        let pc = snap.node(snap.root()).map(|r| r.children[0]);
        assert_eq!(pc.map(|pc| snap.node_name(pc)), Some("pc"));
        assert_eq!(pc.map(|pc| snap.path_to(pc).len()), Some(2));
        assert_eq!(snap.descendant_count(snap.root()), 1);
        Ok(())
    }

    #[test]
    fn rejects_unknown_format() {
        let json = br#"{"format":99,"tick":0,"root":1,"nodes":[],"links":[]}"#;
        assert!(matches!(
            Snapshot::from_json(json),
            Err(SnapshotError::UnsupportedFormat(99))
        ));
    }
}
