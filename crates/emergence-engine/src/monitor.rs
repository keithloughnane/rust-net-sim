//! Watches traffic for the failure shapes that have crashed hosts before: loops, replay storms,
//! amplification and floods (see the diagnostics design docs). The monitor only observes and
//! reports. The hard limits that stop a storm are the fuse ([`Limits`](crate::Limits)), which
//! reports through [`TickOutcome`](crate::TickOutcome) instead.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};

use slotmap::SecondaryMap;

use crate::trace::{TraceEvent, TraceLog};
use crate::{ControllerLogic, LinkId, Network, NodeId, Packet, Relaying};

/// Which check raised an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Check {
    /// Too many packets per tick across the whole network.
    NetworkRate,
    /// Too many packets per tick on one link.
    LinkRate,
    /// Too many packets per tick from one node.
    NodeRate,
    /// A packet ran out of hops: a loop has happened.
    TtlExpired,
    /// Relaying nodes connect links in a cycle: a loop will happen as soon as something is
    /// broadcast into it. Found from the topology alone, before any traffic.
    RelayCycle,
    /// A node keeps receiving the same packet.
    Replay,
    /// One incoming packet made a node send many.
    Amplification,
    /// A node's logic panicked. Its logic was removed so the rest of the world keeps running.
    LogicPanicked,
}

impl Check {
    /// Every check, for listing.
    pub const ALL: [Self; 8] = [
        Self::NetworkRate,
        Self::LinkRate,
        Self::NodeRate,
        Self::TtlExpired,
        Self::RelayCycle,
        Self::Replay,
        Self::Amplification,
        Self::LogicPanicked,
    ];

    /// A stable machine-readable name, such as `"relay_cycle"`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::NetworkRate => "network_rate",
            Self::LinkRate => "link_rate",
            Self::NodeRate => "node_rate",
            Self::TtlExpired => "ttl_expired",
            Self::RelayCycle => "relay_cycle",
            Self::Replay => "replay",
            Self::Amplification => "amplification",
            Self::LogicPanicked => "logic_panicked",
        }
    }
}

/// How bad an alert is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    /// Getting close to a limit.
    Warning,
    /// Past a limit, or something that is always a bug.
    Error,
}

/// What an alert is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subject {
    /// The network as a whole.
    Network,
    /// One node.
    Node(NodeId),
    /// One link.
    Link(LinkId),
}

/// Something a check noticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    /// Tick it was raised on.
    pub tick: u64,
    /// The check that raised it.
    pub check: Check,
    /// How bad it is.
    pub severity: Severity,
    /// What it is about.
    pub subject: Subject,
    /// The measured value, for threshold checks.
    pub value: u64,
    /// The threshold that was crossed, for threshold checks.
    pub limit: u64,
    /// A human-readable explanation.
    pub message: String,
}

/// Warning and error levels for a threshold check. A level of 0 disables it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Threshold {
    /// Value at which a warning is raised.
    pub warn: u64,
    /// Value at which an error is raised.
    pub error: u64,
}

impl Threshold {
    fn severity(self, value: u64) -> Option<(Severity, u64)> {
        if self.error > 0 && value >= self.error {
            Some((Severity::Error, self.error))
        } else if self.warn > 0 && value >= self.warn {
            Some((Severity::Warning, self.warn))
        } else {
            None
        }
    }
}

/// Monitor settings. The defaults suit the sandbox; see the diagnostics docs for which checks a
/// shipped game should keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorConfig {
    /// Packets per tick across the network.
    pub network_rate: Threshold,
    /// Packets per tick on one link.
    pub link_rate: Threshold,
    /// Packets per tick sent by one node.
    pub node_rate: Threshold,
    /// Identical packets received by one node within [`replay_window`](Self::replay_window).
    pub replay: Threshold,
    /// Ticks the replay check looks back over.
    pub replay_window: u64,
    /// Packets one node sent while handling a single incoming packet.
    pub amplification: Threshold,
    /// Ticks before the same alert (check and subject) can be raised again at the same severity.
    pub cooldown: u64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            network_rate: Threshold {
                warn: 2_000,
                error: 4_000,
            },
            link_rate: Threshold {
                warn: 500,
                error: 2_000,
            },
            node_rate: Threshold {
                warn: 50,
                error: 200,
            },
            replay: Threshold { warn: 8, error: 24 },
            replay_window: 32,
            amplification: Threshold {
                warn: 10,
                error: 40,
            },
            cooldown: 100,
        }
    }
}

/// Most distinct packets the replay check remembers before it starts over, so a storm of
/// unique packets cannot use unbounded memory.
const REPLAY_MEMORY: usize = 50_000;

/// The monitor's state. Owned by [`World`](crate::World).
#[derive(Debug, Default)]
pub(crate) struct Monitor {
    pub(crate) config: MonitorConfig,
    link_sends: HashMap<LinkId, u64>,
    node_sends: HashMap<NodeId, u64>,
    network_sends: u64,
    replay: HashMap<(NodeId, u64), VecDeque<u64>>,
    fired: HashMap<(Check, Subject), (u64, Severity)>,
    /// The latest alert from each check, for explaining a tripped fuse.
    latest: BTreeMap<Check, Alert>,
    pub(crate) alerts_raised: u64,
}

impl Monitor {
    /// Records an alert unless the same one was raised recently at the same or higher severity.
    pub(crate) fn raise(&mut self, trace: &mut TraceLog, alert: Alert) {
        let key = (alert.check, alert.subject);
        if let Some(&(tick, severity)) = self.fired.get(&key)
            && alert.severity <= severity
            && alert.tick.saturating_sub(tick) < self.config.cooldown
        {
            return;
        }
        self.fired.insert(key, (alert.tick, alert.severity));
        self.alerts_raised += 1;
        let keep = self
            .latest
            .get(&alert.check)
            .is_none_or(|old| alert.severity >= old.severity || alert.tick > old.tick + 100);
        if keep {
            self.latest.insert(alert.check, alert.clone());
        }
        trace.push(TraceEvent::Alert(alert));
    }

    /// The latest alert from each check raised since `since`, oldest first, so the earliest
    /// (usually the root cause) leads.
    pub(crate) fn recent_alerts(&self, since: u64) -> Vec<Alert> {
        let mut out: Vec<Alert> = self
            .latest
            .values()
            .filter(|a| a.tick >= since)
            .cloned()
            .collect();
        out.sort_by_key(|a| (a.tick, a.check));
        out
    }

    /// The nodes that sent the most this tick, busiest first.
    pub(crate) fn top_senders(&self, n: usize) -> Vec<(NodeId, u64)> {
        top(&self.node_sends, n)
    }

    /// The links that carried the most this tick, busiest first.
    pub(crate) fn top_links(&self, n: usize) -> Vec<(LinkId, u64)> {
        top(&self.link_sends, n)
    }

    pub(crate) fn on_logic_panicked(
        &mut self,
        trace: &mut TraceLog,
        tick: u64,
        node: NodeId,
        message: String,
    ) {
        self.raise(
            trace,
            Alert {
                tick,
                check: Check::LogicPanicked,
                severity: Severity::Error,
                subject: Subject::Node(node),
                value: 0,
                limit: 0,
                message,
            },
        );
    }

    pub(crate) fn on_transmit(&mut self, sender: NodeId, link: LinkId) {
        self.network_sends += 1;
        *self.link_sends.entry(link).or_default() += 1;
        *self.node_sends.entry(sender).or_default() += 1;
    }

    pub(crate) fn on_delivered(
        &mut self,
        trace: &mut TraceLog,
        tick: u64,
        receiver: NodeId,
        packet: &Packet,
    ) {
        let t = self.config.replay;
        if t.warn == 0 && t.error == 0 {
            return;
        }
        let mut h = DefaultHasher::new();
        packet.from().hash(&mut h);
        packet.to().hash(&mut h);
        packet.event().hash(&mut h);
        let key = (receiver, h.finish());
        if self.replay.len() >= REPLAY_MEMORY && !self.replay.contains_key(&key) {
            self.replay.clear();
        }
        let window = self.config.replay_window;
        let seen = self.replay.entry(key).or_default();
        seen.push_back(tick);
        while seen
            .front()
            .is_some_and(|&t0| tick.saturating_sub(t0) >= window)
        {
            seen.pop_front();
        }
        let count = seen.len() as u64;
        if let Some((severity, limit)) = t.severity(count) {
            self.raise(
                trace,
                Alert {
                    tick,
                    check: Check::Replay,
                    severity,
                    subject: Subject::Node(receiver),
                    value: count,
                    limit,
                    message: format!(
                        "received the same {} packet {count} times in {window} ticks",
                        packet.event().kind
                    ),
                },
            );
        }
    }

    pub(crate) fn on_handled(&mut self, trace: &mut TraceLog, tick: u64, node: NodeId, sends: u64) {
        if let Some((severity, limit)) = self.config.amplification.severity(sends) {
            self.raise(
                trace,
                Alert {
                    tick,
                    check: Check::Amplification,
                    severity,
                    subject: Subject::Node(node),
                    value: sends,
                    limit,
                    message: format!("one incoming packet made it send {sends}"),
                },
            );
        }
    }

    pub(crate) fn on_ttl_expired(&mut self, trace: &mut TraceLog, tick: u64, node: NodeId) {
        self.raise(
            trace,
            Alert {
                tick,
                check: Check::TtlExpired,
                severity: Severity::Error,
                subject: Subject::Node(node),
                value: 0,
                limit: 0,
                message: "a packet ran out of hops here: there is a loop".into(),
            },
        );
    }

    /// Rate checks for the tick that just ended; resets the per-tick counters.
    pub(crate) fn end_tick(&mut self, trace: &mut TraceLog, tick: u64) {
        let network = std::mem::take(&mut self.network_sends);
        if let Some((severity, limit)) = self.config.network_rate.severity(network) {
            self.raise(
                trace,
                Alert {
                    tick,
                    check: Check::NetworkRate,
                    severity,
                    subject: Subject::Network,
                    value: network,
                    limit,
                    message: format!("{network} packets this tick"),
                },
            );
        }
        let mut links: Vec<(LinkId, u64)> = self.link_sends.drain().collect();
        links.sort_unstable();
        for (link, n) in links {
            if let Some((severity, limit)) = self.config.link_rate.severity(n) {
                self.raise(
                    trace,
                    Alert {
                        tick,
                        check: Check::LinkRate,
                        severity,
                        subject: Subject::Link(link),
                        value: n,
                        limit,
                        message: format!("{n} packets on this link this tick"),
                    },
                );
            }
        }
        let mut nodes: Vec<(NodeId, u64)> = self.node_sends.drain().collect();
        nodes.sort_unstable();
        for (node, n) in nodes {
            if let Some((severity, limit)) = self.config.node_rate.severity(n) {
                self.raise(
                    trace,
                    Alert {
                        tick,
                        check: Check::NodeRate,
                        severity,
                        subject: Subject::Node(node),
                        value: n,
                        limit,
                        message: format!("sent {n} packets this tick"),
                    },
                );
            }
        }
    }

    /// Looks for relaying nodes that connect links in a cycle. A cycle only loops endlessly if
    /// a flooding relay (a bridge) is part of it: gateways follow routes, which are bounded. So
    /// routed connections are joined first without reporting, then every flooding connection
    /// that closes a cycle is reported.
    pub(crate) fn check_topology(
        &mut self,
        trace: &mut TraceLog,
        tick: u64,
        network: &Network,
        logics: &SecondaryMap<NodeId, Box<dyn ControllerLogic>>,
    ) {
        let mut sets: HashMap<LinkId, LinkId> = HashMap::new();
        for (node, logic) in logics {
            let Some(n) = network.node(node) else {
                continue;
            };
            if logic.relaying() == Relaying::Routed {
                // A gateway joins each of its internal links to each external one.
                for &inside in n.internal_links() {
                    for &outside in n.subscriptions() {
                        let (a, b) = (find_set(&mut sets, inside), find_set(&mut sets, outside));
                        sets.insert(a, b);
                    }
                }
            }
        }
        for (node, logic) in logics {
            let Some(n) = network.node(node) else {
                continue;
            };
            if logic.relaying() != Relaying::Flooding {
                continue;
            }
            let mut links: Vec<LinkId> = n
                .subscriptions()
                .iter()
                .chain(n.internal_links())
                .copied()
                .collect();
            links.dedup();
            links.sort_unstable();
            links.dedup();
            for pair in links.windows(2) {
                let (a, b) = (find_set(&mut sets, pair[0]), find_set(&mut sets, pair[1]));
                if a == b {
                    let name = |l: LinkId| network.link(l).map_or("?", crate::Link::name);
                    self.raise(
                        trace,
                        Alert {
                            tick,
                            check: Check::RelayCycle,
                            severity: Severity::Error,
                            subject: Subject::Node(node),
                            value: 0,
                            limit: 0,
                            message: format!(
                                "{} closes a relay loop between {} and {}",
                                n.name(),
                                name(pair[0]),
                                name(pair[1])
                            ),
                        },
                    );
                } else {
                    sets.insert(a, b);
                }
            }
        }
    }
}

/// Union-find lookup with path compression, iterative so deep chains cannot overflow the stack.
fn find_set(parent: &mut HashMap<LinkId, LinkId>, link: LinkId) -> LinkId {
    let mut root = link;
    while let Some(&p) = parent.get(&root) {
        if p == root {
            break;
        }
        root = p;
    }
    parent.entry(root).or_insert(root);
    let mut cur = link;
    while cur != root {
        let next = parent.get(&cur).copied().unwrap_or(root);
        parent.insert(cur, root);
        cur = next;
    }
    root
}

fn top<K: Copy + Ord + Hash>(counts: &HashMap<K, u64>, n: usize) -> Vec<(K, u64)> {
    let mut all: Vec<(K, u64)> = counts.iter().map(|(&k, &v)| (k, v)).collect();
    all.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    all.truncate(n);
    all
}

impl fmt::Display for Alert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{:?}] {}: {}",
            self.severity,
            self.check.name(),
            self.message
        )
    }
}
