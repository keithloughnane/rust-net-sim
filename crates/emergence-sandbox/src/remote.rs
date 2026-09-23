//! Runs control-port commands (see [`crate::control`]) against the live sandbox, so what a
//! script does shows up in the window, and the results go back to the script.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use eframe::egui;
use serde_json::{Value, json};

use crate::control::{Request, Response};
use crate::graph_view::Item;
use crate::health::{FuseReport, Subject};
use crate::native::NodeId;
use crate::snapshot::Snapshot;
use crate::stress::{self, TestReport};
use crate::{SandboxApp, Source, scenarios};

/// Replies that wait for something to finish.
#[derive(Debug, Default)]
pub(crate) struct Deferred {
    /// Clients waiting for the running tests to finish.
    tests: Vec<Sender<Response>>,
    /// A client waiting for a screenshot to be saved.
    screenshot: Option<(PathBuf, Sender<Response>)>,
}

impl SandboxApp {
    /// Handles every waiting control request, and completes deferred ones that are ready.
    pub(crate) fn handle_control(&mut self, ctx: &egui::Context) {
        let pending: Vec<_> = self
            .control
            .as_ref()
            .map(|c| c.requests.try_iter().collect())
            .unwrap_or_default();
        for p in pending {
            if let Some(response) = self.execute(ctx, p.request, &p.reply) {
                let _ = p.reply.send(response);
            }
        }

        if !self.deferred.tests.is_empty() && !self.tests.is_running() {
            let response = self.results(false);
            for reply in self.deferred.tests.drain(..) {
                let _ = reply.send(response.clone());
            }
        }

        if self.deferred.screenshot.is_some() {
            let image = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            // Only take the waiting reply once the image has arrived (it comes a frame later).
            if let Some(image) = image
                && let Some((path, reply)) = self.deferred.screenshot.take()
            {
                let [w, h] = image.size;
                let saved = image::save_buffer(
                    &path,
                    image.as_raw(),
                    u32::try_from(w).unwrap_or(0),
                    u32::try_from(h).unwrap_or(0),
                    image::ExtendedColorType::Rgba8,
                );
                let _ = reply.send(match saved {
                    Ok(()) => Response::ok(
                        format!("saved {w}×{h} screenshot to {}", path.display()),
                        json!({ "path": path, "width": w, "height": h }),
                    ),
                    Err(e) => Response::error(format!("could not save screenshot: {e}")),
                });
            }
        }
    }

    /// Runs one request. Returns `None` if the reply is deferred.
    #[allow(clippy::too_many_lines)] // One arm per command.
    fn execute(
        &mut self,
        ctx: &egui::Context,
        request: Request,
        reply: &Sender<Response>,
    ) -> Option<Response> {
        let response = match request {
            Request::Status => self.status(),
            Request::Scenarios => {
                let names: Vec<&str> = scenarios::ALL.iter().map(|s| s.name).collect();
                let mut text = String::new();
                for s in scenarios::ALL {
                    let _ = writeln!(text, "{:<16} {}", s.name, s.description);
                }
                Response::ok(text.trim_end(), json!(names))
            }
            Request::Tests => self.test_list(),
            Request::Load { name } => {
                match scenarios::ALL.iter().position(|s| matches(s.name, &name)) {
                    Some(i) => {
                        self.scenario = i;
                        self.watching = None;
                        self.build();
                        self.status()
                    }
                    None => {
                        Response::error(format!("no scenario matches `{name}` (try `scenarios`)"))
                    }
                }
            }
            Request::Watch { name } => match find_test(&name) {
                Ok(i) => {
                    self.watching = Some(i);
                    self.build();
                    self.status()
                }
                Err(e) => e,
            },
            Request::Rebuild => {
                self.build();
                self.status()
            }
            Request::Play => {
                self.clock.playing = true;
                self.fuse = None;
                Response::text(format!(
                    "playing at {} ticks/s",
                    self.clock.ticks_per_second
                ))
            }
            Request::Pause => {
                self.clock.playing = false;
                Response::text(format!("paused at tick {}", self.health.tick))
            }
            Request::Speed { ticks_per_second } => {
                self.clock.ticks_per_second = ticks_per_second.clamp(0.5, 60.0);
                Response::text(format!("speed {} ticks/s", self.clock.ticks_per_second))
            }
            Request::Step { ticks } => self.step(ctx, ticks),
            Request::Run { names, wait } => {
                let tests = if names.is_empty() {
                    Ok((0..stress::ALL.len()).collect())
                } else {
                    names
                        .iter()
                        .map(|n| find_test(n))
                        .collect::<Result<Vec<_>, _>>()
                };
                match (tests, &self.library) {
                    (Err(e), _) => e,
                    (_, Err(e)) => Response::error(e.clone()),
                    (Ok(_), _) if self.tests.is_running() => {
                        Response::error("tests are already running")
                    }
                    (Ok(tests), Ok(library)) => {
                        let count = tests.len();
                        self.tests.run(&library.clone(), tests);
                        self.bottom_tab = crate::BottomTab::Tests;
                        if wait {
                            self.deferred.tests.push(reply.clone());
                            return None;
                        }
                        Response::text(format!("started {count} tests"))
                    }
                }
            }
            Request::Results { failed_only } => self.results(failed_only),
            Request::Open { node } => self.open(node.as_deref()),
            Request::Up => {
                let parent = self.loaded.as_ref().ok().and_then(|l| {
                    self.scope
                        .and_then(|s| l.snapshot.node(s))
                        .and_then(|n| n.parent)
                });
                match parent {
                    Some(p) => {
                        self.scope = Some(p);
                        self.status()
                    }
                    None => Response::error("already at the top"),
                }
            }
            Request::Select { node } => match self.find(&node) {
                Ok(id) => {
                    self.select(Some(Item::Node(id)), Source::Panel);
                    self.node_info(id)
                }
                Err(e) => e,
            },
            Request::Node { name } => match self.find(&name) {
                Ok(id) => self.node_info(id),
                Err(e) => e,
            },
            Request::Send {
                from,
                via,
                to,
                kind,
                data,
            } => match self.find(&from) {
                Ok(node) => {
                    let result = self.loaded.as_mut().map_err(|e| e.clone()).and_then(|l| {
                        l.world
                            .send(node, &via, &to, &kind, data.as_bytes())
                            .and_then(|()| l.world.snapshot().map(|s| l.snapshot = s))
                            .map_err(|e| e.to_string())
                    });
                    match result {
                        Ok(()) => Response::text(format!(
                            "{from} ─{kind}→ {to} via {via}: queued for the next tick"
                        )),
                        Err(e) => Response::error(e),
                    }
                }
                Err(e) => e,
            },
            Request::Logic { node, kind } => match self.find(&node) {
                Ok(id) => {
                    let result = self.loaded.as_mut().map_err(|e| e.clone()).and_then(|l| {
                        l.world
                            .set_logic(id, &kind)
                            .and_then(|()| l.world.snapshot().map(|s| l.snapshot = s))
                            .map_err(|e| e.to_string())
                    });
                    match result {
                        Ok(()) => Response::text(format!("{node} now runs {kind}")),
                        Err(e) => Response::error(e),
                    }
                }
                Err(e) => e,
            },
            Request::Limits {
                transmissions,
                deliveries,
                queue,
                payload,
            } => {
                let result = self.loaded.as_mut().map_err(|e| e.clone()).and_then(|l| {
                    l.world
                        .set_limits(
                            transmissions.unwrap_or(0),
                            deliveries.unwrap_or(0),
                            queue.unwrap_or(0),
                            payload.unwrap_or(0),
                        )
                        .and_then(|()| l.world.health())
                        .map_err(|e| e.to_string())
                });
                match result {
                    Ok(h) => {
                        let l = &h.limits;
                        let text = format!(
                            "fuse limits: {} sent/tick · {} delivered/tick · {} queued · {} payload bytes",
                            l.max_transmissions_per_tick,
                            l.max_deliveries_per_tick,
                            l.max_pending,
                            l.max_payload_bytes
                        );
                        self.health = h;
                        Response::text(text)
                    }
                    Err(e) => Response::error(e),
                }
            }
            Request::Alerts { last } => self.alerts(last),
            Request::Log { last } => {
                let lines: Vec<String> = self
                    .activity
                    .log
                    .iter()
                    .rev()
                    .take(last)
                    .rev()
                    .map(|l| format!("t{:<6}{}", l.tick, l.text))
                    .collect();
                Response::ok(lines.join("\n"), json!(lines))
            }
            Request::Fuse => match &self.fuse {
                Some(report) => {
                    let text = self.fuse_text(report);
                    Response::ok(
                        text,
                        json!({ "tripped": true, "tick": report.tick, "limit": report.limit }),
                    )
                }
                None => Response::ok("the fuse has not tripped", json!({ "tripped": false })),
            },
            Request::Screenshot { path } => {
                if self.deferred.screenshot.is_some() {
                    Response::error("a screenshot is already being taken")
                } else {
                    self.deferred.screenshot = Some((PathBuf::from(path), reply.clone()));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::UserData::default(),
                    ));
                    ctx.request_repaint();
                    return None;
                }
            }
            Request::Quit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                Response::text("closing the sandbox")
            }
        };
        ctx.request_repaint();
        Some(response)
    }

    fn step(&mut self, ctx: &egui::Context, ticks: u32) -> Response {
        if self.loaded.is_err() {
            return Response::error("nothing is loaded");
        }
        self.clock.playing = false;
        let (alerts_before, packets_before) = (self.health.alerts, self.activity.packets);
        let now = ctx.input(|i| i.time);
        let (ran, tripped) = self.run_ticks(ticks.max(1), 0.6, now);
        let h = &self.health;
        let mut text = format!(
            "ran {ran} tick{} → tick {} · {} packets · {} new alerts · load {}/tick",
            if ran == 1 { "" } else { "s" },
            h.tick,
            self.activity.packets - packets_before,
            h.alerts - alerts_before,
            h.transmissions,
        );
        if tripped && let Some(report) = &self.fuse {
            let _ = write!(text, "\n{}", self.fuse_text(report));
        }
        Response::ok(
            text,
            json!({
                "ran": ran,
                "tick": h.tick,
                "fuse_tripped": tripped,
                "new_alerts": h.alerts - alerts_before,
                "transmissions": h.transmissions,
                "pending": h.pending,
            }),
        )
    }

    fn status(&self) -> Response {
        let Ok(loaded) = &self.loaded else {
            return Response::error(self.loaded.as_ref().err().cloned().unwrap_or_default());
        };
        let snap = &loaded.snapshot;
        let h = &self.health;
        let view = match self.watching {
            Some(i) => format!("test “{}”", stress::ALL[i].name),
            None => format!("scenario “{}”", scenarios::ALL[self.scenario].name),
        };
        let at = self.scope.map_or_else(
            || "?".into(),
            |s| {
                snap.path_to(s)
                    .iter()
                    .map(|&n| name_of(snap, n))
                    .collect::<Vec<_>>()
                    .join(" › ")
            },
        );
        let selected = match self.selection {
            Some(Item::Node(n)) => name_of(snap, n).to_owned(),
            Some(Item::Link(l)) => format!("link {}", snap.link_name(l)),
            None => "nothing".into(),
        };
        let (passed, failed, total) = self.tests.counts();
        let text = format!(
            "{view} · tick {} · {}\n\
             load {}/tick (peak {}) · queued {} · alerts {} · fuse {}\n\
             {} nodes · {} links · viewing {at} · selected {selected}\n\
             tests: {passed} passed, {failed} failed of {total}{}",
            h.tick,
            if self.clock.playing {
                format!("playing at {} ticks/s", self.clock.ticks_per_second)
            } else {
                "paused".into()
            },
            h.transmissions,
            h.peak_transmissions,
            h.pending,
            h.alerts,
            if self.fuse.is_some() { "TRIPPED" } else { "ok" },
            snap.node_count(),
            snap.link_count(),
            if self.tests.is_running() {
                " (running)"
            } else {
                ""
            },
        );
        Response::ok(
            text,
            json!({
                "view": view,
                "tick": h.tick,
                "playing": self.clock.playing,
                "ticks_per_second": self.clock.ticks_per_second,
                "transmissions": h.transmissions,
                "peak_transmissions": h.peak_transmissions,
                "pending": h.pending,
                "alerts": h.alerts,
                "fuse_tripped": self.fuse.is_some(),
                "nodes": snap.node_count(),
                "links": snap.link_count(),
                "viewing": at,
                "selected": selected,
                "tests": { "passed": passed, "failed": failed, "total": total, "running": self.tests.is_running() },
            }),
        )
    }

    fn test_list(&self) -> Response {
        let mut text = String::new();
        let mut data = Vec::new();
        let mut group = "";
        for (i, t) in stress::ALL.iter().enumerate() {
            if t.group != group {
                group = t.group;
                let _ = writeln!(text, "{group}");
            }
            let status = self.tests.status_name(i);
            let _ = writeln!(text, "  {} {}", icon(status), t.name);
            data.push(json!({ "group": t.group, "name": t.name, "status": status, "description": t.description }));
        }
        Response::ok(text.trim_end(), Value::Array(data))
    }

    fn results(&self, failed_only: bool) -> Response {
        let mut text = String::new();
        let mut data = Vec::new();
        for (i, t) in stress::ALL.iter().enumerate() {
            let status = self.tests.status_name(i);
            let report = self.tests.report(i);
            if failed_only && status != "failed" {
                continue;
            }
            let _ = write!(text, "{} {}", icon(status), t.name);
            if let Some(r) = report {
                let _ = write!(
                    text,
                    "  ({} ticks, {:.0?}, peak {}/tick{})",
                    r.ticks,
                    r.elapsed,
                    r.peak_transmissions,
                    if r.fuse.is_some() {
                        ", fuse tripped"
                    } else {
                        ""
                    }
                );
            }
            text.push('\n');
            if let Some(r) = report.filter(|r| !r.passed) {
                for line in r.lines.iter().filter(|l| !l.passed) {
                    let _ = writeln!(text, "    ✘ {} — {}", line.label, line.detail);
                }
                if let Some(e) = &r.error {
                    let _ = writeln!(text, "    ✘ {e}");
                }
            }
            data.push(report_json(t.name, status, report));
        }
        let (passed, failed, total) = self.tests.counts();
        let _ = write!(text, "{passed} passed, {failed} failed of {total}");
        Response {
            ok: failed == 0,
            text,
            data: json!({ "passed": passed, "failed": failed, "total": total, "tests": data }),
        }
    }

    fn open(&mut self, node: Option<&str>) -> Response {
        let target = match node {
            None | Some("world") => self.loaded.as_ref().ok().map(|l| l.snapshot.root()),
            Some(name) => match self.find(name) {
                Ok(id) => Some(id),
                Err(e) => return e,
            },
        };
        let Some(target) = target else {
            return Response::error("nothing is loaded");
        };
        self.scope = Some(target);
        self.status()
    }

    fn find(&self, name: &str) -> Result<NodeId, Response> {
        let loaded = self
            .loaded
            .as_ref()
            .map_err(|e| Response::error(e.clone()))?;
        let snap = &loaded.snapshot;
        if name == "world" {
            return Ok(snap.root());
        }
        snap.find_node(name)
            .ok_or_else(|| Response::error(format!("no node named `{name}`")))
    }

    fn node_info(&self, id: NodeId) -> Response {
        let Ok(loaded) = &self.loaded else {
            return Response::error("nothing is loaded");
        };
        let snap = &loaded.snapshot;
        let Some(n) = snap.node(id) else {
            return Response::error("unknown node");
        };
        let links = |ids: &[crate::native::LinkId]| -> Vec<String> {
            ids.iter().map(|&l| snap.link_name(l).to_owned()).collect()
        };
        let children: Vec<&str> = n.children.iter().map(|&c| name_of(snap, c)).collect();
        let text = format!(
            "{} ({}{})\n  parent: {}\n  on links: {}\n  owns links: {}\n  children: {}\n  sent {} · received {}",
            name_of(snap, id),
            n.kind,
            n.logic
                .as_ref()
                .map_or(String::new(), |l| format!(" · {l}")),
            n.parent.map_or("none", |p| name_of(snap, p)),
            links(&n.subscriptions).join(", "),
            links(&n.internal_links).join(", "),
            if children.is_empty() {
                "none".into()
            } else {
                children.join(", ")
            },
            n.sent,
            n.received,
        );
        Response::ok(
            text,
            json!({
                "name": n.name, "kind": n.kind, "logic": n.logic,
                "parent": n.parent.map(|p| name_of(snap, p)),
                "subscriptions": links(&n.subscriptions),
                "internal_links": links(&n.internal_links),
                "children": children,
                "sent": n.sent, "received": n.received,
            }),
        )
    }

    fn alerts(&self, last: usize) -> Response {
        let Ok(loaded) = &self.loaded else {
            return Response::error("nothing is loaded");
        };
        let snap = &loaded.snapshot;
        let mut lines = Vec::new();
        let mut data = Vec::new();
        for a in self.activity.alerts.iter().rev().take(last).rev() {
            let about = subject_name(snap, a.subject);
            lines.push(format!(
                "t{:<6}{:?} {} · {about}: {}",
                a.tick, a.severity, a.check, a.message
            ));
            data.push(json!({ "tick": a.tick, "check": a.check, "severity": format!("{:?}", a.severity).to_lowercase(), "subject": about, "message": a.message }));
        }
        if lines.is_empty() {
            return Response::ok("no alerts", json!([]));
        }
        Response::ok(lines.join("\n"), Value::Array(data))
    }

    fn fuse_text(&self, report: &FuseReport) -> String {
        let Ok(loaded) = &self.loaded else {
            return String::new();
        };
        let snap = &loaded.snapshot;
        let senders: Vec<String> = report
            .top_senders
            .iter()
            .map(|&(n, c)| format!("{} ({c})", name_of(snap, n)))
            .collect();
        let links: Vec<String> = report
            .top_links
            .iter()
            .map(|&(l, c)| format!("{} ({c})", snap.link_name(l)))
            .collect();
        let mut text = format!(
            "FUSE TRIPPED at tick {}: {} ({} packets held back). World paused.\n  busiest senders: {}\n  busiest links: {}",
            report.tick,
            report.limit_text(),
            report.held_back,
            senders.join(", "),
            links.join(", "),
        );
        if !report.recent_alerts.is_empty() {
            text.push_str("\n  likely causes, earliest first:");
            for a in &report.recent_alerts {
                let _ = write!(text, "\n    t{} {}: {}", a.tick, a.check, a.message);
            }
        }
        text
    }
}

fn name_of(snap: &Snapshot, id: NodeId) -> &str {
    if id == snap.root() {
        "world"
    } else {
        snap.node_name(id)
    }
}

fn subject_name(snap: &Snapshot, subject: Option<Subject>) -> String {
    match subject {
        Some(Subject::Node(n)) => name_of(snap, n).to_owned(),
        Some(Subject::Link(l)) => format!("link {}", snap.link_name(l)),
        None => "network".into(),
    }
}

fn icon(status: &str) -> &'static str {
    match status {
        "passed" => "✔",
        "failed" => "✘",
        "running" => "◌",
        "queued" => "…",
        _ => "○",
    }
}

/// Case-insensitive match on a whole name or a word-prefix of it, so `office` finds "Office"
/// and `storm` finds "Broadcast storm: four bridges".
fn matches(name: &str, query: &str) -> bool {
    let (name, query) = (name.to_lowercase(), query.to_lowercase());
    name == query || name.contains(&query)
}

fn find_test(query: &str) -> Result<usize, Response> {
    if let Some(i) = stress::ALL
        .iter()
        .position(|t| t.name.eq_ignore_ascii_case(query))
    {
        return Ok(i);
    }
    let hits: Vec<usize> = (0..stress::ALL.len())
        .filter(|&i| matches(stress::ALL[i].name, query))
        .collect();
    match hits.as_slice() {
        [i] => Ok(*i),
        [] => Err(Response::error(format!(
            "no test matches `{query}` (try `tests`)"
        ))),
        many => Err(Response::error(format!(
            "`{query}` matches several tests: {}",
            many.iter()
                .map(|&i| stress::ALL[i].name)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn report_json(name: &str, status: &str, report: Option<&TestReport>) -> Value {
    let Some(r) = report else {
        return json!({ "name": name, "status": status });
    };
    json!({
        "name": name,
        "status": status,
        "ticks": r.ticks,
        "elapsed_ms": r.elapsed.as_millis(),
        "peak_transmissions": r.peak_transmissions,
        "alerts": r.alerts,
        "fuse": r.fuse,
        "error": r.error,
        "checks": r.lines.iter().map(|l| json!({ "label": l.label, "passed": l.passed, "detail": l.detail })).collect::<Vec<_>>(),
    })
}
