//! Stress tests: networks built to fail the way player-built networks might (loops, storms,
//! replays, floods, bad names, edits mid-flight), run headless through the native library.
//!
//! Each test builds its network (`setup`, which is also what "Watch" loads into the viewer),
//! then runs and checks it (`verify`). Every check becomes a line in the UI's checklist. Tests
//! run on a background thread; the engine's fuse is what keeps a runaway test from taking the
//! sandbox down, which is itself part of what is being tested.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use crate::health::{Alert, FuseReport, Health, Severity};
use crate::native::{LinkId, NativeError, NativeLibrary, NativeWorld, NodeId, TickResult};
use crate::scenarios::{self, BuildResult, Builder};
use crate::snapshot::Snapshot;
use crate::trace::TraceEntry;

type ProbeResult = Result<(), NativeError>;

/// Longest a single test may run before it is stopped and failed.
const TIME_BUDGET: Duration = Duration::from_secs(30);

// Status codes from `bindings/c/emergence.h`.
const ALREADY_HAS_PARENT: u32 = 6;
const WOULD_CREATE_CYCLE: u32 = 7;
const IS_ROOT: u32 = 8;
const INVALID_ROUTE: u32 = 12;
const INVALID_NAME: u32 = 14;
const NAME_CONFLICT: u32 = 15;
const QUEUE_FULL: u32 = 16;
const PAYLOAD_TOO_LARGE: u32 = 17;

/// One stress test.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StressTest {
    pub(crate) group: &'static str,
    pub(crate) name: &'static str,
    /// What the test does and what should happen.
    pub(crate) description: &'static str,
    setup: fn(&mut Builder<'_>) -> BuildResult,
    verify: fn(&mut Probe<'_>) -> ProbeResult,
}

impl StressTest {
    /// Builds the test's network into `world`, including any packets it starts with.
    pub(crate) fn setup(&self, world: &mut NativeWorld) -> BuildResult {
        (self.setup)(&mut Builder::new(world)?)
    }

    /// Runs the test in a fresh world and reports the outcome.
    pub(crate) fn run(&self, library: Arc<NativeLibrary>, cancel: &AtomicBool) -> TestReport {
        let started = Instant::now();
        let result = NativeWorld::new(library).and_then(|mut world| {
            world.set_trace_packets(false)?;
            self.setup(&mut world)
                .map_err(|e| NativeError::Sandbox(format!("setup failed: {e}")))?;
            Ok(world)
        });
        let mut world = match result {
            Ok(world) => world,
            Err(e) => return TestReport::error(e.to_string(), started.elapsed()),
        };
        let health = world.health().unwrap_or_default();
        let mut probe = Probe {
            world,
            lines: Vec::new(),
            alerts: Vec::new(),
            notes: Vec::new(),
            fuse: None,
            ticks: 0,
            health,
            cancel,
            deadline: started + TIME_BUDGET,
            stopped: None,
        };
        let outcome = (self.verify)(&mut probe);
        probe.finish(outcome, started.elapsed())
    }
}

/// One checked expectation.
#[derive(Debug, Clone)]
pub(crate) struct CheckLine {
    pub(crate) label: String,
    pub(crate) passed: bool,
    pub(crate) detail: String,
}

/// How a test went.
#[derive(Debug, Clone)]
pub(crate) struct TestReport {
    pub(crate) passed: bool,
    pub(crate) lines: Vec<CheckLine>,
    /// Why the test could not finish, if it could not.
    pub(crate) error: Option<String>,
    pub(crate) ticks: u64,
    pub(crate) peak_transmissions: u64,
    pub(crate) alerts: usize,
    pub(crate) fuse: Option<String>,
    pub(crate) elapsed: Duration,
}

impl TestReport {
    fn error(error: String, elapsed: Duration) -> Self {
        Self {
            passed: false,
            lines: Vec::new(),
            error: Some(error),
            ticks: 0,
            peak_transmissions: 0,
            alerts: 0,
            fuse: None,
            elapsed,
        }
    }
}

/// A test's view of its world, with helpers for running it and checking what happened.
pub(crate) struct Probe<'c> {
    world: NativeWorld,
    lines: Vec<CheckLine>,
    alerts: Vec<Alert>,
    notes: Vec<String>,
    fuse: Option<FuseReport>,
    ticks: u64,
    health: Health,
    cancel: &'c AtomicBool,
    deadline: Instant,
    /// Why running stopped early (cancelled or out of time).
    stopped: Option<&'static str>,
}

impl Probe<'_> {
    fn finish(mut self, outcome: ProbeResult, elapsed: Duration) -> TestReport {
        let error = match (outcome, self.stopped) {
            (Err(e), _) => Some(format!("unexpected error: {e}")),
            (Ok(()), Some(why)) => Some(why.to_owned()),
            (Ok(()), None) => None,
        };
        if let Ok(h) = self.world.health() {
            self.health = h;
        }
        let passed =
            error.is_none() && !self.lines.is_empty() && self.lines.iter().all(|l| l.passed);
        TestReport {
            passed,
            lines: self.lines,
            error,
            ticks: self.ticks,
            peak_transmissions: self.health.peak_transmissions,
            alerts: self.alerts.len(),
            fuse: self
                .fuse
                .map(|f| format!("tick {}: {}", f.tick, f.limit_text())),
            elapsed,
        }
    }

    fn snapshot(&mut self) -> Result<Snapshot, NativeError> {
        self.world.snapshot()
    }

    fn node(&mut self, name: &str) -> Result<NodeId, NativeError> {
        self.snapshot()?
            .find_node(name)
            .ok_or_else(|| NativeError::Sandbox(format!("no node named {name}")))
    }

    fn link(&mut self, name: &str) -> Result<LinkId, NativeError> {
        self.snapshot()?
            .find_link(name)
            .ok_or_else(|| NativeError::Sandbox(format!("no link named {name}")))
    }

    fn received(&mut self, name: &str) -> Result<u64, NativeError> {
        let snap = self.snapshot()?;
        let id = snap
            .find_node(name)
            .ok_or_else(|| NativeError::Sandbox(format!("no node named {name}")))?;
        Ok(snap.node(id).map_or(0, |n| n.received))
    }

    fn send(&mut self, from: &str, via: &str, to: &str, kind: &str) -> ProbeResult {
        let node = self.node(from)?;
        self.world.send(node, via, to, kind, b"")
    }

    /// Runs up to `ticks`, stopping at the first tripped fuse, as the sandbox UI does.
    fn run(&mut self, ticks: u64) -> ProbeResult {
        self.run_inner(ticks, true, true)
    }

    fn run_inner(&mut self, ticks: u64, stop_at_fuse: bool, drain: bool) -> ProbeResult {
        for _ in 0..ticks {
            if self.cancel.load(Ordering::Relaxed) {
                self.stopped = Some("cancelled");
                break;
            }
            if Instant::now() > self.deadline {
                self.stopped = Some("ran out of time");
                break;
            }
            let result = self.world.tick()?;
            self.ticks += 1;
            if drain {
                for entry in self.world.drain_trace()? {
                    match entry {
                        TraceEntry::Alert(a) => self.alerts.push(a),
                        TraceEntry::Note { text, .. } => self.notes.push(text),
                        _ => {}
                    }
                }
            }
            if result == TickResult::FuseTripped {
                if self.fuse.is_none() {
                    self.fuse = self.world.fuse_report()?;
                }
                if stop_at_fuse {
                    break;
                }
            }
        }
        self.health = self.world.health()?;
        Ok(())
    }

    fn check(&mut self, label: impl Into<String>, passed: bool, detail: impl Into<String>) {
        self.lines.push(CheckLine {
            label: label.into(),
            passed,
            detail: detail.into(),
        });
    }

    fn first_alert(&self, check: &str, min: Severity) -> Option<&Alert> {
        self.alerts
            .iter()
            .find(|a| a.check == check && a.severity >= min)
    }

    fn expect_alert(&mut self, check: &str, min: Severity) {
        let found = self
            .first_alert(check, min)
            .map(|a| (a.tick, a.message.clone()));
        let label = format!("{check} alert ({})", severity_name(min));
        match found {
            Some((tick, message)) => self.check(label, true, format!("tick {tick}: {message}")),
            None => self.check(label, false, "never raised"),
        }
    }

    fn expect_alert_by(&mut self, check: &str, tick: u64) {
        let found = self.first_alert(check, Severity::Warning).map(|a| a.tick);
        self.check(
            format!("{check} alert by tick {tick}"),
            found.is_some_and(|t| t <= tick),
            found.map_or("never raised".into(), |t| format!("raised on tick {t}")),
        );
    }

    fn expect_no_alert(&mut self, check: &str) {
        let found = self.first_alert(check, Severity::Warning).map(|a| a.tick);
        self.check(
            format!("no {check} alert"),
            found.is_none(),
            found.map_or("none".into(), |t| format!("raised on tick {t}")),
        );
    }

    fn expect_no_alerts(&mut self) {
        let summary: Vec<String> = self
            .alerts
            .iter()
            .take(3)
            .map(|a| format!("{} (tick {})", a.check, a.tick))
            .collect();
        self.check(
            "no alerts (no false alarms)",
            self.alerts.is_empty(),
            if self.alerts.is_empty() {
                "none".to_owned()
            } else {
                format!("{} raised: {}", self.alerts.len(), summary.join(", "))
            },
        );
    }

    fn expect_fuse(&mut self, limit: &str) {
        let got = self.fuse.as_ref().map(|f| (f.limit.clone(), f.tick));
        self.check(
            format!("fuse trips ({limit})"),
            got.as_ref().is_some_and(|(l, _)| l == limit),
            got.map_or("never tripped".into(), |(l, t)| {
                format!("{l} limit on tick {t}")
            }),
        );
    }

    fn expect_fuse_any(&mut self) {
        let got = self.fuse.as_ref().map(|f| (f.limit.clone(), f.tick));
        self.check(
            "fuse trips",
            got.is_some(),
            got.map_or("never tripped".into(), |(l, t)| {
                format!("{l} limit on tick {t}")
            }),
        );
    }

    fn expect_no_fuse(&mut self) {
        let got = self.fuse.as_ref().map(|f| (f.limit.clone(), f.tick));
        self.check(
            "fuse does not trip",
            got.is_none(),
            got.map_or("never tripped".into(), |(l, t)| {
                format!("{l} limit on tick {t}")
            }),
        );
    }

    fn expect_report_names(&mut self, check: &str) {
        let named = self
            .fuse
            .as_ref()
            .is_some_and(|f| f.recent_alerts.iter().any(|a| a.check == check));
        self.check(
            format!("fuse report names the cause ({check})"),
            named,
            if named {
                "yes"
            } else {
                "missing from the report"
            },
        );
    }

    /// Nothing got past the fuse: per-tick sends and the queue stayed within their limits.
    fn expect_bounded(&mut self) {
        let h = &self.health;
        let ok = h.peak_transmissions <= h.limits.max_transmissions_per_tick
            && h.pending <= h.limits.max_pending;
        let detail = format!(
            "peak {} / {} packets per tick, {} / {} queued",
            h.peak_transmissions,
            h.limits.max_transmissions_per_tick,
            h.pending,
            h.limits.max_pending
        );
        self.check("stays within the hard limits", ok, detail);
    }

    fn expect_quiet(&mut self) {
        let t = self.health.transmissions;
        self.check(
            "traffic dies out",
            t == 0,
            format!("{t} packets on the last tick"),
        );
    }

    fn expect_status<T>(&mut self, label: &str, result: Result<T, NativeError>, code: u32) {
        let got = match &result {
            Ok(_) => "accepted".to_owned(),
            Err(e) => e.to_string(),
        };
        let passed = result.err().and_then(|e| e.status()) == Some(code);
        self.check(label, passed, got);
    }
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Warning => "warning or worse",
        Severity::Error => "error",
    }
}

// ---- Runner ---------------------------------------------------------------------------------

/// Progress from the background runner.
#[derive(Debug)]
pub(crate) enum RunnerEvent {
    Started(usize),
    Finished(usize, Box<TestReport>),
    Done,
}

/// Runs tests on a background thread so the UI stays responsive.
#[derive(Debug)]
pub(crate) struct Runner {
    pub(crate) events: Receiver<RunnerEvent>,
    cancel: Arc<AtomicBool>,
}

impl Runner {
    pub(crate) fn start(library: Arc<NativeLibrary>, tests: Vec<usize>) -> Self {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("stress-tests".into())
            .spawn(move || run_all(&library, &tests, &flag, &tx))
            .ok();
        Self { events: rx, cancel }
    }

    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn run_all(
    library: &Arc<NativeLibrary>,
    tests: &[usize],
    cancel: &AtomicBool,
    tx: &Sender<RunnerEvent>,
) {
    for &i in tests {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let _ = tx.send(RunnerEvent::Started(i));
        let report = catch_unwind(AssertUnwindSafe(|| ALL[i].run(Arc::clone(library), cancel)))
            .unwrap_or_else(|_| {
                TestReport::error("the test itself panicked".into(), Duration::ZERO)
            });
        let _ = tx.send(RunnerEvent::Finished(i, Box::new(report)));
    }
    let _ = tx.send(RunnerEvent::Done);
}

// ---- Shared topology ------------------------------------------------------------------------

/// Devices with `logic` on one world link.
fn crowd(b: &mut Builder<'_>, link: LinkId, prefix: &str, n: usize, logic: &str) -> BuildResult {
    for i in 0..n {
        b.device(b.root, &format!("{prefix}-{i}"), "device", &[link], logic)?;
    }
    Ok(())
}

/// Two links joined by `n` bridges, with a sender on the first, which broadcasts once.
fn bridged(b: &mut Builder<'_>, n: usize) -> BuildResult {
    let a = b.world_link("a")?;
    let other = b.world_link("b")?;
    let sender = b.device(b.root, "sender", "device", &[a], "none")?;
    for i in 0..n {
        b.device(b.root, &format!("bridge-{i}"), "hub", &[a, other], "bridge")?;
    }
    b.world.send(sender, "a", "*@a", "hello", b"")
}

#[allow(clippy::unnecessary_wraps)] // Must match the `setup` signature.
fn none(_: &mut Builder<'_>) -> BuildResult {
    Ok(())
}

fn scenario(name: &str) -> impl Fn(&mut Builder<'_>) -> BuildResult + '_ {
    move |b| {
        scenarios::ALL
            .iter()
            .find(|s| s.name == name)
            .map_or(Ok(()), |s| s.build(b.world))
    }
}

fn healthy_run(p: &mut Probe<'_>, ticks: u64) -> ProbeResult {
    p.run(ticks)?;
    p.expect_no_fuse();
    p.expect_no_alerts();
    let drops = p.health.ttl_drops;
    p.check(
        "no packets run out of hops",
        drops == 0,
        format!("{drops} TTL drops"),
    );
    Ok(())
}

/// A fingerprint of everything that happens in `ticks` ticks of a world.
fn fingerprint(world: &mut NativeWorld, ticks: u64) -> Result<u64, NativeError> {
    world.set_trace_packets(true)?;
    let mut h = DefaultHasher::new();
    for _ in 0..ticks {
        world.tick()?;
        for e in world.drain_trace()? {
            format!("{e:?}").hash(&mut h);
        }
    }
    Ok(h.finish())
}

// ---- The catalogue --------------------------------------------------------------------------

pub(crate) const ALL: &[StressTest] = &[
    // Healthy traffic: the checks must not cry wolf.
    StressTest {
        group: "Healthy traffic",
        name: "Office runs clean",
        description: "300 ticks of the Office scenario: beacons, scanners through gateways, NPCs. No alerts, no fuse, no TTL drops.",
        setup: |b| scenario("Office")(b),
        verify: |p| healthy_run(p, 300),
    },
    StressTest {
        group: "Healthy traffic",
        name: "City block runs clean",
        description: "200 ticks of 345 nodes in 8 buildings with scanners on every LAN.",
        setup: |b| scenario("City block")(b),
        verify: |p| healthy_run(p, 200),
    },
    StressTest {
        group: "Healthy traffic",
        name: "Mesh runs clean",
        description: "300 ticks of 30 devices on 10 shared links with beacons.",
        setup: |b| scenario("Mesh")(b),
        verify: |p| healthy_run(p, 300),
    },
    StressTest {
        group: "Healthy traffic",
        name: "Big legitimate broadcast",
        description: "A host pings a hall of 400 guests five times; 2,000 pongs come back. Busy, but not a bug.",
        setup: |b| {
            let hall = b.world_link("hall")?;
            b.device(b.root, "host", "device", &[hall], "none")?;
            crowd(b, hall, "guest", 400, "responder")
        },
        verify: |p| {
            for _ in 0..5 {
                p.send("host", "hall", "*@hall", "ping")?;
                p.run(20)?;
            }
            p.expect_no_fuse();
            p.expect_no_alerts();
            let got = p.received("host")?;
            p.check(
                "every guest answered every ping",
                got == 2_000,
                format!("{got} of 2000 pongs"),
            );
            Ok(())
        },
    },
    // Loops.
    StressTest {
        group: "Loops",
        name: "Relay loop: two bridges",
        description: "Two bridges join the same two links. The loop is flagged from the topology before any traffic; the broadcast then circles until its TTL runs out.",
        setup: |b| bridged(b, 2),
        verify: |p| {
            p.run(1)?;
            p.expect_alert_by("relay_cycle", 1);
            p.run(40)?;
            p.expect_alert("ttl_expired", Severity::Error);
            p.expect_no_fuse();
            p.expect_quiet();
            Ok(())
        },
    },
    StressTest {
        group: "Loops",
        name: "Broadcast storm: four bridges",
        description: "Four bridges on two links: every copy is copied three times per hop. Exponential growth must trip the fuse, stay within limits, and the report must name the loop.",
        setup: |b| bridged(b, 4),
        verify: |p| {
            p.run(40)?;
            p.expect_alert("relay_cycle", Severity::Error);
            p.expect_fuse("transmissions");
            p.expect_report_names("relay_cycle");
            p.expect_bounded();
            Ok(())
        },
    },
    StressTest {
        group: "Loops",
        name: "Loop across levels",
        description: "A player patches a cable from inside a computer to the Wi-Fi it is already on. The computer's gateway and the patch bridge form a loop between levels; it must be flagged and die out.",
        setup: |b| {
            let wifi = b.world_link("wifi")?;
            let laptop = b.device(b.root, "laptop", "computer", &[wifi], "responder")?;
            let pc = b.device(b.root, "pc", "computer", &[wifi], "gateway")?;
            let ipc = b.internal_link(pc, "ipc")?;
            b.device(pc, "patch", "hub", &[ipc, wifi], "bridge")?;
            b.world.send(laptop, "wifi", "*@wifi", "hello", b"")
        },
        verify: |p| {
            p.run(60)?;
            p.expect_alert("relay_cycle", Severity::Error);
            p.expect_no_fuse();
            p.expect_quiet();
            Ok(())
        },
    },
    StressTest {
        group: "Loops",
        name: "Echo ping-pong",
        description: "Two nodes that answer everything with a copy of it. Every reply is a new packet, so the TTL never runs out: only the replay check can see it.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            let a = b.device(b.root, "a", "device", &[bus], "echo")?;
            b.device(b.root, "b", "device", &[bus], "echo")?;
            b.world.send(a, "bus", "b@bus", "ping", b"")
        },
        verify: |p| {
            p.run(80)?;
            p.expect_alert("replay", Severity::Warning);
            p.expect_no_alert("ttl_expired");
            let t = p.health.transmissions;
            p.check(
                "still running (the TTL cannot stop it)",
                t > 0,
                format!("{t} packets on the last tick"),
            );
            Ok(())
        },
    },
    // Replay and amplification.
    StressTest {
        group: "Replay & amplification",
        name: "Stuck retry",
        description: "A node re-sends the same packet four times a tick forever. The receiver's replay check must reach error.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            b.device(b.root, "retry", "device", &[bus], "replayer")?;
            let server = b.device(b.root, "server", "device", &[bus], "responder")?;
            b.world.send(server, "bus", "retry@bus", "ping", b"")
        },
        verify: |p| {
            p.run(40)?;
            p.expect_alert("replay", Severity::Error);
            p.expect_no_fuse();
            Ok(())
        },
    },
    StressTest {
        group: "Replay & amplification",
        name: "Amplifier",
        description: "One ping makes a node send 50; each of the three responders' pongs is amplified again. Amplification must be flagged and the runaway stopped by the fuse.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            b.device(b.root, "amp", "device", &[bus], "amplifier")?;
            crowd(b, bus, "r", 3, "responder")?;
            let first = b
                .world
                .snapshot()?
                .find_node("r-0")
                .ok_or_else(|| NativeError::Sandbox("r-0".into()))?;
            b.world.send(first, "bus", "amp@bus", "ping", b"")
        },
        verify: |p| {
            p.run(20)?;
            p.expect_alert("amplification", Severity::Error);
            p.expect_fuse("transmissions");
            p.expect_bounded();
            Ok(())
        },
    },
    StressTest {
        group: "Replay & amplification",
        name: "Two amplifiers facing each other",
        description: "Each amplifies the other's output: 50× per hop. The fuse must trip within a few ticks.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            let a = b.device(b.root, "amp-1", "device", &[bus], "amplifier")?;
            b.device(b.root, "amp-2", "device", &[bus], "amplifier")?;
            b.world.send(a, "bus", "amp-2@bus", "x", b"")
        },
        verify: |p| {
            p.run(10)?;
            p.expect_alert("amplification", Severity::Error);
            p.expect_fuse_any();
            let t = p.ticks;
            p.check(
                "caught within 5 ticks",
                t <= 5,
                format!("stopped on tick {t}"),
            );
            p.expect_bounded();
            Ok(())
        },
    },
    // Floods and hard limits.
    StressTest {
        group: "Floods & limits",
        name: "Noisy node",
        description: "One node sends 1,000 broadcasts a tick. Heavy, but under the fuse: the rate checks must name it.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            b.device(b.root, "noisy", "device", &[bus], "flooder")?;
            crowd(b, bus, "listener", 3, "none")
        },
        verify: |p| {
            p.run(10)?;
            p.expect_alert("node_rate", Severity::Error);
            p.expect_alert("link_rate", Severity::Warning);
            p.expect_no_fuse();
            Ok(())
        },
    },
    StressTest {
        group: "Floods & limits",
        name: "Firehose",
        description: "Six flooders together exceed the per-tick packet limit. The fuse must trip on transmissions and the queue stay bounded even when the host keeps ticking.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            crowd(b, bus, "flooder", 6, "flooder")
        },
        verify: |p| {
            p.run(5)?;
            p.expect_fuse("transmissions");
            p.expect_alert("network_rate", Severity::Error);
            p.run_inner(20, false, true)?;
            p.expect_bounded();
            Ok(())
        },
    },
    StressTest {
        group: "Floods & limits",
        name: "Fan-out bomb",
        description: "60 broadcasts into a stadium of 1,200 listeners is 72,000 deliveries in one tick. The fuse must trip on deliveries.",
        setup: |b| {
            let stadium = b.world_link("stadium")?;
            let announcer = b.device(b.root, "announcer", "device", &[stadium], "none")?;
            crowd(b, stadium, "fan", 1_200, "none")?;
            for _ in 0..60 {
                b.world
                    .send(announcer, "stadium", "*@stadium", "cheer", b"")?;
            }
            Ok(())
        },
        verify: |p| {
            p.run(5)?;
            p.expect_fuse("deliveries");
            p.expect_bounded();
            Ok(())
        },
    },
    StressTest {
        group: "Floods & limits",
        name: "Host floods the queue",
        description: "The host queues 25,000 packets between two ticks. The first 20,000 are accepted, the rest refused with QueueFull, and the next tick trips the fuse.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            crowd(b, bus, "n", 2, "none")
        },
        verify: |p| {
            let from = p.node("n-0")?;
            let (mut ok, mut full) = (0, 0);
            for _ in 0..25_000 {
                match p.world.send(from, "bus", "n-1@bus", "x", b"") {
                    Ok(()) => ok += 1,
                    Err(e) if e.status() == Some(QUEUE_FULL) => full += 1,
                    Err(e) => return Err(e),
                }
            }
            p.check(
                "20,000 accepted, 5,000 refused",
                ok == 20_000 && full == 5_000,
                format!("{ok} accepted, {full} refused"),
            );
            p.run(1)?;
            p.expect_fuse_any();
            let refused = p.health.refused_sends;
            p.check(
                "refusals are counted",
                refused == 5_000,
                format!("{refused} refused"),
            );
            p.expect_bounded();
            Ok(())
        },
    },
    StressTest {
        group: "Floods & limits",
        name: "Huge payloads",
        description: "A payload over 64 KiB is refused. A maximum-size payload broadcast to 1,000 listeners is shared, not copied 1,000 times, so it is quick.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            b.device(b.root, "sender", "device", &[bus], "none")?;
            crowd(b, bus, "l", 1_000, "none")
        },
        verify: |p| {
            let from = p.node("sender")?;
            let limit = usize::try_from(p.health.limits.max_payload_bytes).unwrap_or(65_536);
            let result = p
                .world
                .send(from, "bus", "*@bus", "big", &vec![7; limit + 1]);
            p.expect_status("oversized payload refused", result, PAYLOAD_TOO_LARGE);
            p.world.send(from, "bus", "*@bus", "big", &vec![7; limit])?;
            let started = Instant::now();
            p.run(1)?;
            let took = started.elapsed();
            p.expect_no_fuse();
            p.check(
                "delivered to 1,000 listeners quickly",
                took < Duration::from_millis(500),
                format!("{took:.1?}"),
            );
            Ok(())
        },
    },
    StressTest {
        group: "Floods & limits",
        name: "Trace never drained",
        description: "A host that never reads the trace must not leak memory: the buffer stays capped and counts what it threw away.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            b.device(b.root, "chatty", "device", &[bus], "flooder")?;
            crowd(b, bus, "l", 2, "none")
        },
        verify: |p| {
            p.world.set_trace_packets(true)?;
            p.run_inner(150, false, false)?;
            let (len, lost) = (p.health.trace_len, p.health.trace_discarded);
            p.check(
                "trace buffer stays capped",
                len <= 100_000,
                format!("{len} events buffered"),
            );
            p.check(
                "overflow is counted",
                lost > 0,
                format!("{lost} events discarded"),
            );
            Ok(())
        },
    },
    // Things players will do while building.
    StressTest {
        group: "Player edits",
        name: "Invalid names",
        description: "Empty names, reserved tokens and route syntax are refused for nodes and links.",
        setup: none,
        verify: |p| {
            for bad in ["", "*", "^", "?", ".", "a@b", "a/b", " padded", "tab\there"] {
                let node = p.world.create_node(bad, "x");
                p.expect_status(&format!("node name {bad:?} refused"), node, INVALID_NAME);
                let link = p.world.create_link(bad);
                p.expect_status(&format!("link name {bad:?} refused"), link, INVALID_NAME);
            }
            Ok(())
        },
    },
    StressTest {
        group: "Player edits",
        name: "Name clashes",
        description: "Every computer can have its own `registry`, but two `pc-1`s on one Wi-Fi, or two `ipc` buses in one computer, are refused.",
        setup: |b| {
            let wifi = b.world_link("wifi")?;
            b.computer(b.root, "pc-1", &[wifi], &[])?;
            b.computer(b.root, "pc-2", &[wifi], &[])?;
            Ok(())
        },
        verify: |p| {
            let registries = p.snapshot()?.nodes_named("registry");
            p.check(
                "each computer has its own registry",
                registries == 2,
                format!("{registries} registries"),
            );
            let (root, wifi, pc1) = (p.world.root()?, p.link("wifi")?, p.node("pc-1")?);
            let dup = p.world.create_node("pc-1", "computer")?;
            let result = p.world.connect(root, dup, Some(wifi));
            p.expect_status(
                "second pc-1 on the same Wi-Fi refused",
                result,
                NAME_CONFLICT,
            );
            let bus = p.world.create_link("ipc")?;
            let result = p.world.add_internal_link(pc1, bus);
            p.expect_status("second ipc in one computer refused", result, NAME_CONFLICT);
            Ok(())
        },
    },
    StressTest {
        group: "Player edits",
        name: "Nesting mistakes",
        description: "Putting a node inside its own child, giving it two parents, or nesting the world are refused and change nothing.",
        setup: none,
        verify: |p| {
            let root = p.world.root()?;
            let a = p.world.create_node("box", "x")?;
            let b = p.world.create_node("crate", "x")?;
            let c = p.world.create_node("bag", "x")?;
            p.world.connect(a, b, None)?;
            let r = p.world.connect(b, a, None);
            p.expect_status("node inside its own child refused", r, WOULD_CREATE_CYCLE);
            let r = p.world.connect(a, a, None);
            p.expect_status("node inside itself refused", r, WOULD_CREATE_CYCLE);
            let r = p.world.connect(c, b, None);
            p.expect_status("second parent refused", r, ALREADY_HAS_PARENT);
            let r = p.world.connect(a, root, None);
            p.expect_status("world inside a node refused", r, IS_ROOT);
            let parent = p.snapshot()?.node(b).and_then(|n| n.parent);
            p.check(
                "nothing changed",
                parent == Some(a),
                "crate is still in box",
            );
            Ok(())
        },
    },
    StressTest {
        group: "Player edits",
        name: "Unplugged mid-flight",
        description: "A node queues 50 packets, then is unplugged before they go out. They are dropped, not delivered from a node that is no longer there.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            let sender = b.device(b.root, "sender", "device", &[bus], "none")?;
            b.device(b.root, "receiver", "device", &[bus], "none")?;
            for _ in 0..50 {
                b.world.send(sender, "bus", "receiver@bus", "x", b"")?;
            }
            Ok(())
        },
        verify: |p| {
            let (root, sender) = (p.world.root()?, p.node("sender")?);
            p.world.disconnect(root, sender)?;
            p.run(2)?;
            let dropped = p.health.sender_left_drops;
            p.check(
                "queued packets dropped",
                dropped == 50,
                format!("{dropped} dropped"),
            );
            let got = p.received("receiver")?;
            p.check("nothing delivered", got == 0, format!("{got} received"));
            Ok(())
        },
    },
    StressTest {
        group: "Player edits",
        name: "Gateway loses its uplink",
        description: "A computer is unplugged from the Wi-Fi mid-run, then an app inside it tries to reach the laptop. The gateway notes it has no way out; nothing breaks.",
        setup: |b| {
            let wifi = b.world_link("wifi")?;
            b.device(b.root, "laptop", "computer", &[wifi], "responder")?;
            b.computer(b.root, "pc", &[wifi], &["net-scan", "fileman"])?;
            Ok(())
        },
        verify: |p| {
            p.run(50)?;
            let (pc, wifi) = (p.node("pc")?, p.link("wifi")?);
            p.world.unsubscribe(pc, wifi)?;
            p.run(50)?;
            // An app still tries to reach the Wi-Fi through its now-unplugged computer.
            p.send("fileman", "ipc", "laptop@wifi", "ping")?;
            p.run(5)?;
            p.expect_no_fuse();
            p.expect_no_alerts();
            let noted = p.notes.iter().any(|n| n.contains("no way out"));
            p.check(
                "gateway reports it has no way out",
                noted,
                format!("{} notes", p.notes.len()),
            );
            Ok(())
        },
    },
    StressTest {
        group: "Player edits",
        name: "Logic removed mid-storm",
        description: "A three-bridge storm is building; the player switches the bridges off. Traffic must die down without the fuse tripping.",
        setup: |b| bridged(b, 3),
        verify: |p| {
            p.run(6)?;
            let t = p.health.transmissions;
            p.check(
                "storm is building",
                t >= 16,
                format!("{t} packets on tick 6"),
            );
            for i in 0..3 {
                let bridge = p.node(&format!("bridge-{i}"))?;
                p.world.set_logic(bridge, "none")?;
            }
            p.run(30)?;
            p.expect_no_fuse();
            p.expect_quiet();
            Ok(())
        },
    },
    StressTest {
        group: "Player edits",
        name: "Nested 2,000 deep",
        description: "Computers inside computers, 2,000 levels deep, with a scanner at the bottom. Building, ticking and snapshotting must not overflow the stack or stall.",
        setup: |b| {
            let mut parent = b.root;
            let mut bus = b.world_link("bus")?;
            for i in 0..2_000 {
                let node =
                    b.device(parent, &format!("level-{i}"), "computer", &[bus], "gateway")?;
                bus = b.internal_link(node, "bus")?;
                parent = node;
            }
            b.device(parent, "deep-scan", "app", &[bus], "scanner")?;
            Ok(())
        },
        verify: |p| {
            p.run(100)?;
            p.expect_no_fuse();
            let snap = p.snapshot()?;
            let depth = snap.descendant_count(snap.root());
            p.check(
                "all 2,001 nested nodes present",
                depth == 2_001,
                format!("{depth} nodes"),
            );
            let bottom = snap
                .find_node("deep-scan")
                .map_or(0, |n| snap.path_to(n).len());
            p.check(
                "path from the bottom resolves",
                bottom == 2_002,
                format!("path of {bottom}"),
            );
            Ok(())
        },
    },
    StressTest {
        group: "Player edits",
        name: "20,000 nodes",
        description: "200 links of 100 devices each, with a beacon per link. 100 ticks must finish well inside the time budget.",
        setup: |b| {
            for l in 0..200 {
                let link = b.world_link(&format!("net-{l}"))?;
                b.device(b.root, &format!("beacon-{l}"), "device", &[link], "beacon")?;
                for d in 0..99 {
                    b.device(
                        b.root,
                        &format!("d-{l}-{d}"),
                        "device",
                        &[link],
                        "responder",
                    )?;
                }
            }
            Ok(())
        },
        verify: |p| {
            let started = Instant::now();
            p.run(100)?;
            let took = started.elapsed();
            p.expect_no_fuse();
            p.check(
                "100 ticks in under 10 s",
                took < Duration::from_secs(10),
                format!("{took:.1?}"),
            );
            Ok(())
        },
    },
    StressTest {
        group: "Player edits",
        name: "Computer plugged into its own bus",
        description: "A computer subscribes to its own internal bus. It must hear each packet once, not twice, and not loop.",
        setup: |b| {
            let wifi = b.world_link("wifi")?;
            b.device(b.root, "laptop", "computer", &[wifi], "responder")?;
            let pc = b.device(b.root, "pc", "computer", &[wifi], "gateway")?;
            let ipc = b.internal_link(pc, "ipc")?;
            b.world.subscribe(pc, ipc)?;
            let app = b.device(pc, "app", "app", &[ipc], "responder")?;
            b.world.send(app, "ipc", "*@wifi", "ping", b"")
        },
        verify: |p| {
            p.run(6)?;
            let (laptop, app) = (p.received("laptop")?, p.received("app")?);
            p.check("laptop got one ping", laptop == 1, format!("{laptop}"));
            p.check("app got one pong", app == 1, format!("{app}"));
            p.expect_no_alerts();
            Ok(())
        },
    },
    StressTest {
        group: "Player edits",
        name: "Odd addresses",
        description: "Addressing the parent from the top level, a node that does not exist, a route that is too deep, and `*@*`.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            b.device(b.root, "x", "device", &[bus], "responder")?;
            b.device(b.root, "y", "device", &[bus], "responder")?;
            Ok(())
        },
        verify: |p| {
            p.send("x", "bus", "^@bus", "ping")?;
            p.send("x", "bus", "ghost@bus", "ping")?;
            p.run(2)?;
            let y = p.received("y")?;
            p.check(
                "parent-of-top-level and unknown node reach nobody",
                y == 0,
                format!("y received {y}"),
            );
            let deep = ["a@bus"; 9].join("/");
            let x = p.node("x")?;
            let result = p.world.send(x, "bus", &deep, "ping", b"");
            p.expect_status("9-hop route refused", result, INVALID_ROUTE);
            p.send("x", "bus", "*@*", "ping")?;
            p.run(2)?;
            let y = p.received("y")?;
            p.check(
                "*@* reaches everyone on the link",
                y == 1,
                format!("y received {y}"),
            );
            p.expect_no_alerts();
            Ok(())
        },
    },
    // Robustness.
    StressTest {
        group: "Robustness",
        name: "Crashing logic",
        description: "A node's logic panics on its first packet. It is removed and reported; the world keeps ticking and nothing crashes the host.",
        setup: |b| {
            let bus = b.world_link("bus")?;
            b.device(b.root, "bug", "device", &[bus], "crasher")?;
            let ok = b.device(b.root, "ok", "device", &[bus], "responder")?;
            b.world.send(ok, "bus", "bug@bus", "ping", b"")
        },
        verify: |p| {
            p.run(5)?;
            p.expect_alert("logic_panicked", Severity::Error);
            let t = p.ticks;
            p.check("world kept ticking", t == 5, format!("{t} ticks"));
            let snap = p.snapshot()?;
            let logic = snap
                .find_node("bug")
                .and_then(|n| snap.node(n))
                .and_then(|n| n.logic.clone());
            p.check(
                "the crashing logic was removed",
                logic.is_none(),
                format!("{logic:?}"),
            );
            Ok(())
        },
    },
    StressTest {
        group: "Robustness",
        name: "Deterministic",
        description: "The same network run twice produces exactly the same sequence of events, so any bug can be replayed.",
        setup: |b| scenario("Office")(b),
        verify: |p| {
            let mut twin = NativeWorld::new(p.world.library())?;
            scenario("Office")(&mut Builder::new(&mut twin)?)?;
            let (a, b) = (
                fingerprint(&mut p.world, 200)?,
                fingerprint(&mut twin, 200)?,
            );
            p.check("identical traces", a == b, format!("{a:016x} vs {b:016x}"));
            Ok(())
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole catalogue must pass against the real compiled library (`cargo build` first).
    #[test]
    fn every_stress_test_passes() -> Result<(), NativeError> {
        let library = NativeLibrary::load(&NativeLibrary::default_path())?;
        let cancel = AtomicBool::new(false);
        let mut failures = Vec::new();
        for test in ALL {
            let report = test.run(Arc::clone(&library), &cancel);
            if !report.passed {
                let failed: Vec<String> = report
                    .lines
                    .iter()
                    .filter(|l| !l.passed)
                    .map(|l| format!("{}: {}", l.label, l.detail))
                    .chain(report.error)
                    .collect();
                failures.push(format!("{}: {}", test.name, failed.join("; ")));
            }
        }
        assert!(
            failures.is_empty(),
            "failing stress tests:\n{}",
            failures.join("\n")
        );
        Ok(())
    }
}
