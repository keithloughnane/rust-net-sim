//! Turns the trace into things to show: packets in flight on the canvas, and a traffic log.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;

use eframe::egui::Color32;

use crate::native::{LinkId, NodeId};
use crate::snapshot::Snapshot;
use crate::style;
use crate::trace::TraceEntry;

/// How long a node keeps glowing after a packet arrives, in seconds.
pub(crate) const GLOW_SECONDS: f64 = 0.8;
const LOG_CAPACITY: usize = 2_000;

/// One transmission: a sender puts a packet on a link and some listeners accept it.
#[derive(Debug, Clone)]
pub(crate) struct Flight {
    pub(crate) sender: NodeId,
    pub(crate) link: LinkId,
    pub(crate) receivers: Vec<NodeId>,
    pub(crate) kind: String,
    /// When the animation starts, in UI seconds.
    pub(crate) start: f64,
    /// How long it takes to travel sender → link → receivers.
    pub(crate) duration: f64,
}

impl Flight {
    pub(crate) fn arrival(&self) -> f64 {
        self.start + self.duration
    }
}

/// A line in the traffic log.
#[derive(Debug, Clone)]
pub(crate) struct LogLine {
    pub(crate) tick: u64,
    pub(crate) text: String,
    pub(crate) color: Color32,
    /// Nodes involved, for filtering by selection.
    pub(crate) nodes: Vec<NodeId>,
}

/// Recent traffic, kept for drawing.
#[derive(Debug, Default)]
pub(crate) struct Activity {
    pub(crate) flights: Vec<Flight>,
    pub(crate) log: VecDeque<LogLine>,
    pub(crate) packets: u64,
    pub(crate) drops: u64,
}

impl Activity {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Adds one tick's trace. `start`/`duration` time the animations for this batch.
    pub(crate) fn ingest(
        &mut self,
        snap: &Snapshot,
        events: Vec<TraceEntry>,
        start: f64,
        duration: f64,
    ) {
        // Packets are identified by ID within the batch; delivery lines follow their send.
        let mut by_packet: HashMap<u64, usize> = HashMap::new();
        let mut batch: Vec<(Flight, SentInfo)> = Vec::new();

        for event in events {
            match event {
                TraceEntry::Sent {
                    tick,
                    packet,
                    sender,
                    link,
                    to,
                    kind,
                    data,
                    ..
                } => {
                    by_packet.insert(packet, batch.len());
                    batch.push((
                        Flight {
                            sender,
                            link,
                            receivers: Vec::new(),
                            kind,
                            start,
                            duration,
                        },
                        SentInfo {
                            tick,
                            to,
                            data,
                            rules: Vec::new(),
                            drop: None,
                        },
                    ));
                }
                TraceEntry::Delivered {
                    packet,
                    receiver,
                    rule,
                    ..
                } => {
                    if let Some(&i) = by_packet.get(&packet) {
                        batch[i].0.receivers.push(receiver);
                        batch[i].1.rules.push(rule);
                    }
                }
                TraceEntry::Dropped {
                    packet, at, reason, ..
                } => {
                    self.drops += 1;
                    if let Some(&i) = by_packet.get(&packet) {
                        let where_ =
                            at.map_or(String::new(), |n| format!(" at {}", snap.node_name(n)));
                        batch[i].1.drop = Some(format!("{}{where_}", reason.replace('_', " ")));
                    }
                }
                TraceEntry::Note { tick, node, text } => self.push_log(LogLine {
                    tick,
                    text: format!("{}: {text}", snap.node_name(node)),
                    color: style::TEXT_WEAK,
                    nodes: vec![node],
                }),
                TraceEntry::Other => {}
            }
        }

        for (flight, info) in batch {
            self.packets += 1;
            let line = describe(snap, &flight, &info);
            self.push_log(line);
            self.flights.push(flight);
        }
    }

    /// Forgets animations that have finished glowing.
    pub(crate) fn expire(&mut self, now: f64) {
        self.flights.retain(|f| now < f.arrival() + GLOW_SECONDS);
    }

    fn push_log(&mut self, line: LogLine) {
        if self.log.len() == LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }
}

struct SentInfo {
    tick: u64,
    to: String,
    data: Option<String>,
    rules: Vec<String>,
    drop: Option<String>,
}

fn describe(snap: &Snapshot, flight: &Flight, info: &SentInfo) -> LogLine {
    let data = info
        .data
        .as_deref()
        .filter(|d| !d.is_empty())
        .map_or(String::new(), |d| format!(" \"{d}\""));
    let mut text = format!(
        "{} ─{}{data}→ {}  via {}",
        snap.node_name(flight.sender),
        flight.kind,
        info.to,
        snap.link_name(flight.link),
    );
    if flight.receivers.is_empty() {
        text.push_str("  ⇒ nobody");
    } else {
        let heard: Vec<String> = flight
            .receivers
            .iter()
            .zip(&info.rules)
            .map(|(&r, rule)| match rule.as_str() {
                "addressed" | "broadcast" => snap.node_name(r).to_owned(),
                other => format!("{} ({})", snap.node_name(r), other.replace('_', " ")),
            })
            .collect();
        let _ = write!(text, "  ⇒ {}", heard.join(", "));
    }
    if let Some(drop) = &info.drop {
        let _ = write!(text, "  ✕ {drop}");
    }
    let mut nodes = vec![flight.sender];
    nodes.extend(&flight.receivers);
    LogLine {
        tick: info.tick,
        text,
        color: if info.drop.is_some() {
            style::DROP
        } else {
            style::event_color(&flight.kind)
        },
        nodes,
    }
}
