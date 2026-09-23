//! Turns the trace into things to show: packets in flight on the canvas, and a traffic log.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;

use eframe::egui::Color32;

use crate::health::{Alert, Severity};
use crate::native::{LinkId, NodeId};
use crate::snapshot::Snapshot;
use crate::style;
use crate::trace::TraceEntry;

/// How long a node keeps glowing after a packet arrives, in seconds.
pub(crate) const GLOW_SECONDS: f64 = 0.8;
const LOG_CAPACITY: usize = 2_000;
const ALERT_CAPACITY: usize = 1_000;
/// Most packets animated per batch. A storm can produce tens of thousands per tick; drawing
/// them all would freeze the UI the sandbox is supposed to keep responsive.
const MAX_FLIGHTS_PER_BATCH: usize = 300;
/// Most per-packet log lines per batch; the rest are summarised in one line.
const MAX_LOG_LINES_PER_BATCH: usize = 60;

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
    pub(crate) alerts: VecDeque<Alert>,
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
                TraceEntry::Alert(alert) => self.ingest_alert(snap, alert),
                TraceEntry::Other => {}
            }
        }

        let total = batch.len();
        let tick = batch.last().map_or(0, |(_, info)| info.tick);
        for (i, (flight, info)) in batch.into_iter().enumerate() {
            self.packets += 1;
            if i < MAX_LOG_LINES_PER_BATCH {
                let line = describe(snap, &flight, &info);
                self.push_log(line);
            }
            if i < MAX_FLIGHTS_PER_BATCH {
                self.flights.push(flight);
            }
        }
        if total > MAX_LOG_LINES_PER_BATCH {
            self.push_log(LogLine {
                tick,
                text: format!(
                    "… and {} more packets (only the first {MAX_FLIGHTS_PER_BATCH} are animated)",
                    total - MAX_LOG_LINES_PER_BATCH
                ),
                color: style::TEXT_WEAK,
                nodes: Vec::new(),
            });
        }
    }

    fn ingest_alert(&mut self, snap: &Snapshot, alert: Alert) {
        let (nodes, about) = match alert.subject {
            Some(crate::health::Subject::Node(n)) => (vec![n], snap.node_name(n).to_owned()),
            Some(crate::health::Subject::Link(l)) => {
                (Vec::new(), format!("link {}", snap.link_name(l)))
            }
            None => (Vec::new(), "network".to_owned()),
        };
        self.push_log(LogLine {
            tick: alert.tick,
            text: format!("⚠ {} · {about}: {}", alert.check, alert.message),
            color: style::severity_color(alert.severity),
            nodes,
        });
        if self.alerts.len() == ALERT_CAPACITY {
            self.alerts.pop_front();
        }
        self.alerts.push_back(alert);
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

impl Activity {
    /// The most severe recent alert about each node or link, for badges on the canvas.
    pub(crate) fn marks(&self, since_tick: u64) -> HashMap<crate::health::Subject, Severity> {
        let mut marks = HashMap::new();
        for a in self.alerts.iter().filter(|a| a.tick >= since_tick) {
            if let Some(subject) = a.subject {
                let e = marks.entry(subject).or_insert(a.severity);
                *e = (*e).max(a.severity);
            }
        }
        marks
    }
}
