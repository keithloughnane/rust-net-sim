//! JSON snapshot of a network, for hosts that want to display or inspect the whole graph.
//!
//! This is part of the ABI: bump [`FORMAT`] on any breaking change to the shape below.
//! Handles are the same raw integers the C functions use; `null` means "none".

use emergence_engine::{LinkId, NodeId, World};
use serde::Serialize;

/// Version of the snapshot format.
pub(crate) const FORMAT: u32 = 2;

#[derive(Serialize)]
pub(crate) struct Snapshot<'a> {
    format: u32,
    tick: u64,
    root: u64,
    nodes: Vec<NodeEntry<'a>>,
    links: Vec<LinkEntry<'a>>,
}

#[derive(Serialize)]
struct NodeEntry<'a> {
    id: u64,
    name: &'a str,
    kind: &'a str,
    parent: Option<u64>,
    children: Vec<u64>,
    internal_links: Vec<u64>,
    subscriptions: Vec<u64>,
    /// Kind of attached logic, or `null`.
    logic: Option<&'static str>,
    sent: u64,
    received: u64,
}

#[derive(Serialize)]
struct LinkEntry<'a> {
    id: u64,
    name: &'a str,
    owner: Option<u64>,
    subscribers: Vec<u64>,
}

fn nodes(ids: &[NodeId]) -> Vec<u64> {
    ids.iter().map(|id| id.to_raw()).collect()
}

fn links(ids: &[LinkId]) -> Vec<u64> {
    ids.iter().map(|id| id.to_raw()).collect()
}

impl<'a> Snapshot<'a> {
    pub(crate) fn of(world: &'a World) -> Self {
        let network = world.network();
        Self {
            format: FORMAT,
            tick: world.tick_count(),
            root: network.root().to_raw(),
            nodes: network
                .nodes()
                .map(|(id, node)| NodeEntry {
                    id: id.to_raw(),
                    name: node.name(),
                    kind: node.kind(),
                    parent: node.parent().map(NodeId::to_raw),
                    children: nodes(node.children()),
                    internal_links: links(node.internal_links()),
                    subscriptions: links(node.subscriptions()),
                    logic: world.logic_kind(id),
                    sent: world.stats(id).sent,
                    received: world.stats(id).received,
                })
                .collect(),
            links: network
                .links()
                .map(|(id, link)| LinkEntry {
                    id: id.to_raw(),
                    name: link.name(),
                    owner: link.owner().map(NodeId::to_raw),
                    subscribers: nodes(link.subscribers()),
                })
                .collect(),
        }
    }
}
