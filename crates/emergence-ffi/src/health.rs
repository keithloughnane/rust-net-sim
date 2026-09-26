//! JSON forms of the world's health counters, the fuse report and monitor alerts.
//!
//! Part of the ABI, versioned with the trace format (`trace::FORMAT`).

use emergence_engine::{Alert, FuseReport, Health, Severity, Subject, World};
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct HealthJson {
    tick: u64,
    transmissions: u64,
    deliveries: u64,
    peak_transmissions: u64,
    pending: usize,
    refused_sends: u64,
    ttl_drops: u64,
    sender_left_drops: u64,
    alerts: u64,
    fuse_trips: u64,
    trace_len: usize,
    trace_discarded: u64,
    host_pending: usize,
    host_discarded: u64,
    direct_pushes: u64,
    limits: LimitsJson,
}

#[derive(Serialize)]
#[allow(clippy::struct_field_names)] // Same names as `emergence_engine::Limits`.
struct LimitsJson {
    max_transmissions_per_tick: u64,
    max_deliveries_per_tick: u64,
    max_pending: usize,
    max_payload_bytes: usize,
}

pub(crate) fn health(world: &World) -> HealthJson {
    let Health {
        tick,
        transmissions,
        deliveries,
        peak_transmissions,
        pending,
        refused_sends,
        ttl_drops,
        sender_left_drops,
        alerts,
        fuse_trips,
        trace_len,
        trace_discarded,
        host_pending,
        host_discarded,
        direct_pushes,
        ..
    } = world.health();
    let limits = world.limits();
    HealthJson {
        tick,
        transmissions,
        deliveries,
        peak_transmissions,
        pending,
        refused_sends,
        ttl_drops,
        sender_left_drops,
        alerts,
        fuse_trips,
        trace_len,
        trace_discarded,
        host_pending,
        host_discarded,
        direct_pushes,
        limits: LimitsJson {
            max_transmissions_per_tick: limits.max_transmissions_per_tick,
            max_deliveries_per_tick: limits.max_deliveries_per_tick,
            max_pending: limits.max_pending,
            max_payload_bytes: limits.max_payload_bytes,
        },
    }
}

#[derive(Serialize)]
pub(crate) struct AlertJson {
    tick: u64,
    check: &'static str,
    severity: &'static str,
    /// `{"node": id}`, `{"link": id}`, or `null` for the whole network.
    subject: Option<SubjectJson>,
    value: u64,
    limit: u64,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum SubjectJson {
    Node(u64),
    Link(u64),
}

pub(crate) fn alert(a: &Alert) -> AlertJson {
    AlertJson {
        tick: a.tick,
        check: a.check.name(),
        severity: match a.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        },
        subject: match a.subject {
            Subject::Node(n) => Some(SubjectJson::Node(n.to_raw())),
            Subject::Link(l) => Some(SubjectJson::Link(l.to_raw())),
            Subject::Network => None,
        },
        value: a.value,
        limit: a.limit,
        message: a.message.clone(),
    }
}

#[derive(Serialize)]
pub(crate) struct FuseJson {
    tick: u64,
    limit: &'static str,
    max: u64,
    held_back: usize,
    /// `[node id, packets sent this tick]`, busiest first.
    top_senders: Vec<(u64, u64)>,
    /// `[link id, packets carried this tick]`, busiest first.
    top_links: Vec<(u64, u64)>,
    /// The latest alert from each check, oldest (usually the root cause) first.
    recent_alerts: Vec<AlertJson>,
}

pub(crate) fn fuse_report(r: &FuseReport) -> FuseJson {
    FuseJson {
        tick: r.tick,
        limit: r.limit.name(),
        max: r.max,
        held_back: r.held_back,
        top_senders: r
            .top_senders
            .iter()
            .map(|&(n, c)| (n.to_raw(), c))
            .collect(),
        top_links: r.top_links.iter().map(|&(l, c)| (l.to_raw(), c)).collect(),
        recent_alerts: r.recent_alerts.iter().map(alert).collect(),
    }
}
