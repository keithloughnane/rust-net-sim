//! JSON form of the packets delivered to host nodes (`emergence_world_drain_host_deliveries_json`).

use std::fmt::Write as _;

use emergence_engine::{HostDelivery, PacketRoute};
use serde::Serialize;

/// Everything collected in one drain.
#[derive(Serialize)]
pub(crate) struct Deliveries<'a> {
    deliveries: Vec<Delivery<'a>>,
}

#[derive(Serialize)]
struct Delivery<'a> {
    receiver: u64,
    link: u64,
    from: Vec<(&'a str, &'a str)>,
    to: Vec<(&'a str, &'a str)>,
    kind: &'a str,
    data: String,
    ttl: u8,
}

impl<'a> Deliveries<'a> {
    pub(crate) fn of(deliveries: &'a [HostDelivery]) -> Self {
        Self {
            deliveries: deliveries
                .iter()
                .map(|d| Delivery {
                    receiver: d.receiver.to_raw(),
                    link: d.link.to_raw(),
                    from: hops(d.packet.from()),
                    to: hops(d.packet.to()),
                    kind: &d.packet.event().kind,
                    data: hex(&d.packet.event().data),
                    ttl: d.packet.ttl(),
                })
                .collect(),
        }
    }
}

fn hops(route: &PacketRoute) -> Vec<(&str, &str)> {
    route
        .hops()
        .iter()
        .map(|h| (h.node.as_str(), h.link.as_str()))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}
