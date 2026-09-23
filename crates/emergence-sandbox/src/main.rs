//! Interactive test harness for the Emergence Engine.
//!
//! The sandbox talks to the engine only through the compiled native library and its C ABI,
//! the same way Unity or Unreal will. Run it with `cargo sandbox`, which builds that library
//! first.

mod activity;
mod capture;
mod control;
mod graph_view;
mod health;
mod native;
mod remote;
mod scenarios;
mod snapshot;
mod stress;
mod style;
mod tests_panel;
mod trace;

use std::sync::Arc;

use activity::Activity;
use eframe::egui::{self, RichText, collapsing_header::CollapsingState};
use graph_view::{GraphView, Item, ViewAction};
use native::{LinkId, NativeLibrary, NativeWorld, NodeId, TickResult};
use snapshot::Snapshot;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Emergence Sandbox")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Emergence Sandbox",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            Ok(Box::new(SandboxApp::new(
                &capture::Options::from_env(),
                &cc.egui_ctx,
            )))
        }),
    )
}

/// A built network: the native world that owns it and the snapshot the UI draws from.
#[derive(Debug)]
struct Loaded {
    world: NativeWorld,
    snapshot: Snapshot,
}

/// Drives `World::tick` from the UI: play/pause, single steps, and a speed.
#[derive(Debug)]
struct Clock {
    playing: bool,
    ticks_per_second: f32,
    /// Fractional ticks owed from previous frames.
    owed: f32,
    step_requested: bool,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            playing: true,
            ticks_per_second: 4.0,
            owed: 0.0,
            step_requested: false,
        }
    }
}

/// The inspector's "send an event" form.
#[derive(Debug, Default)]
struct SendForm {
    node: Option<NodeId>,
    via: String,
    to: String,
    kind: String,
    data: String,
    result: Option<Result<String, String>>,
}

/// Something the inspector asked for, applied after drawing (the world is borrowed while
/// drawing).
#[derive(Debug)]
enum Command {
    SetLogic(NodeId, String),
    Send {
        node: NodeId,
        via: String,
        to: String,
        kind: String,
        data: String,
    },
}

/// Where a selection came from, which decides whether the canvas or the tree follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Canvas,
    Panel,
}

#[derive(Debug)]
struct SandboxApp {
    library: Result<Arc<NativeLibrary>, String>,
    scenario: usize,
    loaded: Result<Loaded, String>,
    scope: Option<NodeId>,
    selection: Option<Item>,
    /// Set when the tree should expand to, and scroll to, the selection on the next frame.
    reveal_in_tree: bool,
    /// The scope the tree was last expanded to, so opening a level also opens it in the tree.
    tree_scope: Option<NodeId>,
    graph: GraphView,
    capture: Option<capture::Capture>,
    clock: Clock,
    activity: Activity,
    log_only_selected: bool,
    send_form: SendForm,
    /// The last error from ticking or a command, shown in the toolbar.
    sim_error: Option<String>,
    health: health::Health,
    /// Set when the fuse trips; shown as a banner until dismissed.
    fuse: Option<health::FuseReport>,
    bottom_tab: BottomTab,
    tests: tests_panel::TestsPanel,
    /// The stress test loaded into the viewer, instead of a scenario.
    watching: Option<usize>,
    /// The fuse-limits editor, while open.
    limits_edit: Option<health::Limits>,
    /// Lets `emergence-ctl` drive this window.
    control: Option<control::ControlServer>,
    /// Why the control port is not available, if it is not.
    control_problem: Option<String>,
    deferred: remote::Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BottomTab {
    Traffic,
    Alerts,
    Tests,
}

impl SandboxApp {
    fn new(options: &capture::Options, ctx: &egui::Context) -> Self {
        let library =
            NativeLibrary::load(&NativeLibrary::default_path()).map_err(|e| e.to_string());
        let mut app = Self {
            library,
            scenario: 0,
            loaded: Err("not built yet".into()),
            scope: None,
            selection: None,
            reveal_in_tree: false,
            tree_scope: None,
            graph: GraphView::default(),
            capture: options
                .screenshot
                .clone()
                .map(|p| capture::Capture::new(p, options.frames)),
            clock: Clock::default(),
            activity: Activity::default(),
            log_only_selected: false,
            send_form: SendForm::default(),
            sim_error: None,
            health: health::Health::default(),
            fuse: None,
            bottom_tab: BottomTab::Traffic,
            tests: tests_panel::TestsPanel::default(),
            watching: None,
            limits_edit: None,
            control: None,
            control_problem: None,
            deferred: remote::Deferred::default(),
        };
        let port = std::env::var("EMERGENCE_CONTROL_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(control::DEFAULT_PORT);
        if port == 0 {
            app.control_problem = Some("turned off (EMERGENCE_CONTROL_PORT=0)".into());
        } else {
            match control::ControlServer::start(port, ctx.clone()) {
                Ok(server) => app.control = Some(server),
                Err(e) => app.control_problem = Some(format!("port {port} unavailable: {e}")),
            }
        }
        if let Some(name) = &options.watch {
            app.watching = stress::ALL
                .iter()
                .position(|t| t.name.eq_ignore_ascii_case(name));
        }
        if let Some(tab) = &options.tab {
            app.bottom_tab = match tab.as_str() {
                "alerts" => BottomTab::Alerts,
                "tests" => BottomTab::Tests,
                _ => BottomTab::Traffic,
            };
        }
        if options.run_tests
            && let Ok(library) = &app.library
        {
            let all = (0..stress::ALL.len()).collect();
            app.tests.run(library, all);
        }
        if let Some(speed) = options.speed {
            app.clock.ticks_per_second = speed;
        }
        if let Some(wanted) = &options.scenario {
            app.scenario = scenarios::ALL
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(wanted))
                .unwrap_or(0);
        }
        app.build();
        if options.play {
            app.clock.playing = true;
        }
        if let (Some(name), Ok(loaded)) = (&options.open, &app.loaded) {
            app.scope = loaded.snapshot.find_node(name).or(app.scope);
        }
        app
    }

    /// (Re)builds the current scenario in a fresh native world.
    fn build(&mut self) {
        self.selection = None;
        self.graph.reset();
        self.activity.clear();
        self.clock.owed = 0.0;
        self.sim_error = None;
        self.fuse = None;
        let Ok(library) = &self.library else { return };
        let (name, watching) = match self.watching {
            Some(i) => (stress::ALL[i].name, Some(stress::ALL[i])),
            None => (scenarios::ALL[self.scenario].name, None),
        };
        let scenario = scenarios::ALL[self.scenario];
        self.loaded = NativeWorld::new(library.clone())
            .and_then(|mut world| {
                match watching {
                    Some(test) => test.setup(&mut world)?,
                    None => scenario.build(&mut world)?,
                }
                let snapshot = world.snapshot()?;
                self.health = world.health()?;
                Ok(Loaded { world, snapshot })
            })
            .map_err(|e| format!("Building “{name}” failed: {e}"));
        if watching.is_some() {
            // Tests are for stepping through: start paused.
            self.clock.playing = false;
        }
        self.scope = self.loaded.as_ref().ok().map(|l| l.snapshot.root());
    }

    fn select(&mut self, item: Option<Item>, source: Source) {
        self.selection = item;
        let (Some(item), Ok(loaded)) = (item, &self.loaded) else {
            return;
        };
        let snap = &loaded.snapshot;
        self.reveal_in_tree = true;
        if source == Source::Panel {
            // Show the level that contains the item, so it is visible on the canvas.
            self.scope = Some(container_of(snap, item));
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let before = self.scenario;
            egui::ComboBox::from_id_salt("scenario")
                .selected_text(scenarios::ALL[self.scenario].name)
                .show_ui(ui, |ui| {
                    for (i, s) in scenarios::ALL.iter().enumerate() {
                        ui.selectable_value(&mut self.scenario, i, s.name)
                            .on_hover_text(s.description);
                    }
                })
                .response
                .on_hover_text(scenarios::ALL[self.scenario].description);
            if before != self.scenario {
                self.watching = None;
            }
            if ui.button("⟲ Rebuild").clicked() || before != self.scenario {
                self.build();
            }
            if let Some(i) = self.watching {
                ui.colored_label(style::WARN, format!("Watching test: {}", stress::ALL[i].name))
                    .on_hover_text(stress::ALL[i].description);
            }
            ui.separator();

            let clock = &mut self.clock;
            let label = if clock.playing {
                "⏸ Pause"
            } else {
                "▶ Play"
            };
            if ui.button(label).on_hover_text("Space").clicked() {
                clock.playing = !clock.playing;
            }
            if ui
                .add_enabled(!clock.playing, egui::Button::new("Step"))
                .on_hover_text("Run one tick")
                .clicked()
            {
                clock.step_requested = true;
            }
            ui.add(
                egui::Slider::new(&mut clock.ticks_per_second, 0.5..=60.0)
                    .logarithmic(true)
                    .suffix(" ticks/s")
                    .max_decimals(1),
            );
            if let Ok(loaded) = &self.loaded {
                ui.monospace(format!("tick {}", loaded.snapshot.tick()));
                let h = &self.health;
                let load = h.transmissions;
                let max = h.limits.max_transmissions_per_tick.max(1);
                let color = if load * 2 >= max {
                    style::DROP
                } else if load * 10 >= max {
                    style::WARN
                } else {
                    style::TEXT_WEAK
                };
                ui.colored_label(color, format!("load {load}/tick"))
                    .on_hover_text(format!(
                        "Packets sent last tick. The fuse trips at {max} sent or {} delivered per tick.\nQueued: {} / {}  (refused so far: {})\nPeak: {} per tick\nTTL drops: {}  ·  unplugged drops: {}\nFuse trips: {}\nTrace buffer: {} (lost {})",
                        h.limits.max_deliveries_per_tick,
                        h.pending, h.limits.max_pending, h.refused_sends,
                        h.peak_transmissions, h.ttl_drops, h.sender_left_drops, h.fuse_trips,
                        h.trace_len, h.trace_discarded
                    ));
                if ui.button("Limits…").on_hover_text("Tune the fuse's hard limits").clicked() {
                    self.limits_edit = Some(self.health.limits.clone());
                }
            }
            if let Some(e) = &self.sim_error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Ok(library) = &self.library {
                    ui.weak(format!("libemergence v{}", library.version()))
                        .on_hover_text(library.path().display().to_string());
                }
                match (&self.control, &self.control_problem) {
                    (Some(c), _) => {
                        ui.weak(format!("ctl :{}", c.port)).on_hover_text(
                            "Drive this window from a terminal, e.g.\n  cargo ctl status\n  cargo ctl step 10\n  cargo ctl run",
                        );
                    }
                    (None, Some(problem)) => {
                        ui.weak("ctl off").on_hover_text(problem);
                    }
                    (None, None) => {}
                }
                if let Ok(loaded) = &self.loaded {
                    ui.separator();
                    ui.label(format!(
                        "{} nodes · {} links",
                        loaded.snapshot.node_count(),
                        loaded.snapshot.link_count()
                    ));
                }
            });
        });
    }

    /// Runs however many ticks are due, then refreshes the snapshot and the activity view.
    fn advance(&mut self, ctx: &egui::Context) {
        if self.loaded.is_err() {
            return;
        }
        let clock = &mut self.clock;
        let mut steps = u32::from(std::mem::take(&mut clock.step_requested));
        if clock.playing {
            clock.owed += ctx.input(|i| i.stable_dt).min(0.25) * clock.ticks_per_second;
            let due = clock.owed.floor();
            clock.owed -= due;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Small, positive.
            {
                steps += (due as u32).min(30);
            }
            ctx.request_repaint();
        }
        if steps == 0 {
            return;
        }
        // Animate each batch over about one tick interval, so packets visibly hop.
        let duration = if clock.playing {
            (0.9 / f64::from(clock.ticks_per_second)).clamp(0.12, 1.0)
        } else {
            0.6
        };
        let now = ctx.input(|i| i.time);
        self.run_ticks(steps, duration, now);
    }

    /// Runs up to `steps` ticks, stopping early if the fuse trips, then refreshes the snapshot,
    /// health and activity. Returns how many ticks ran and whether the fuse tripped. On a trip
    /// the world is paused: the library reported a hard limit, and stopping is the UI's call.
    fn run_ticks(&mut self, steps: u32, duration: f64, now: f64) -> (u32, bool) {
        let Ok(loaded) = &mut self.loaded else {
            return (0, false);
        };
        let mut ran = 0;
        let mut tripped = false;
        let result = (|| {
            for _ in 0..steps {
                ran += 1;
                if loaded.world.tick()? == TickResult::FuseTripped {
                    tripped = true;
                    break;
                }
            }
            let trace = loaded.world.drain_trace()?;
            loaded.snapshot = loaded.world.snapshot()?;
            self.health = loaded.world.health()?;
            if tripped {
                self.fuse = loaded.world.fuse_report()?;
            }
            Ok::<_, native::NativeError>(trace)
        })();
        match result {
            Ok(trace) => {
                self.activity.ingest(&loaded.snapshot, trace, now, duration);
                if tripped {
                    self.clock.playing = false;
                }
            }
            Err(e) => {
                self.sim_error = Some(e.to_string());
                self.clock.playing = false;
            }
        }
        (ran, tripped)
    }

    fn run_command(&mut self, command: &Command) {
        let Ok(loaded) = &mut self.loaded else { return };
        let result = match command {
            Command::SetLogic(node, kind) => loaded.world.set_logic(*node, kind),
            Command::Send {
                node,
                via,
                to,
                kind,
                data,
            } => loaded.world.send(*node, via, to, kind, data.as_bytes()),
        };
        let result = result.and_then(|()| {
            loaded.snapshot = loaded.world.snapshot()?;
            Ok(())
        });
        if let Command::Send { to, kind, .. } = command {
            let when = if self.clock.playing {
                "on the next tick"
            } else {
                "when you step"
            };
            self.send_form.result = Some(
                result
                    .as_ref()
                    .map(|()| format!("{kind} → {to} queued, delivered {when}"))
                    .map_err(ToString::to_string),
            );
        } else if let Err(e) = result {
            self.sim_error = Some(e.to_string());
        }
    }

    fn hierarchy(&mut self, ui: &mut egui::Ui) {
        let Ok(loaded) = &self.loaded else { return };
        if self.tree_scope != self.scope
            && let Some(scope) = self.scope
        {
            expand_tree_to(ui.ctx(), &loaded.snapshot, scope);
            self.tree_scope = self.scope;
        }
        let reveal = self.reveal_in_tree;
        if reveal && let Some(item) = self.selection {
            expand_tree_to(
                ui.ctx(),
                &loaded.snapshot,
                container_of(&loaded.snapshot, item),
            );
        }
        let mut clicked = None;
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            tree_node(
                ui,
                &loaded.snapshot,
                loaded.snapshot.root(),
                self.selection,
                reveal,
                &mut clicked,
                0,
            );
        });
        self.reveal_in_tree = false;
        if clicked.is_some() {
            self.select(clicked, Source::Panel);
        }
    }

    #[allow(clippy::too_many_lines)] // A flat list of panel sections.
    fn inspector(&mut self, ui: &mut egui::Ui) {
        let Ok(loaded) = &self.loaded else { return };
        let snap = &loaded.snapshot;
        let logic_kinds: Vec<native::LogicKind> = self
            .library
            .as_ref()
            .map(|l| l.logic_kinds().to_vec())
            .unwrap_or_default();
        let mut clicked = None;
        let mut enter = None;
        let mut command = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| match self.selection {
                None => {
                    ui.weak("Click a node or link to inspect it.");
                }
                Some(Item::Node(id)) => {
                    let Some(node) = snap.node(id) else { return };
                    ui.heading(&node.name);
                    ui.colored_label(style::kind_color(&node.kind), &node.kind);
                    ui.add_space(6.0);
                    egui::Grid::new("node-props").num_columns(2).show(ui, |ui| {
                        ui.weak("ID");
                        ui.monospace(format!("{:#x}", id.raw()));
                        ui.end_row();
                        ui.weak("Parent");
                        match node.parent {
                            Some(p) => node_button(ui, snap, p, &mut clicked),
                            None => {
                                ui.weak("none");
                            }
                        }
                        ui.end_row();
                        ui.weak("Nested");
                        ui.label(format!("{} nodes", snap.descendant_count(id)));
                        ui.end_row();
                    });
                    if !node.children.is_empty() && ui.button("Open ⏵").clicked() {
                        enter = Some(id);
                    }
                    ui.add_space(8.0);
                    ui.strong("Logic");
                    let current = node.logic.clone().unwrap_or_else(|| "none".into());
                    let mut chosen = current.clone();
                    egui::ComboBox::from_id_salt("logic")
                        .selected_text(&chosen)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut chosen, "none".into(), "none");
                            for kind in logic_kinds.iter().filter(|k| !k.faulty) {
                                ui.selectable_value(&mut chosen, kind.name.clone(), &kind.name);
                            }
                            ui.separator();
                            ui.weak("Faulty, for stress testing");
                            for kind in logic_kinds.iter().filter(|k| k.faulty) {
                                let text =
                                    RichText::new(format!("⚠ {}", kind.name)).color(style::DROP);
                                ui.selectable_value(&mut chosen, kind.name.clone(), text);
                            }
                        });
                    if chosen != current {
                        command = Some(Command::SetLogic(id, chosen));
                    }
                    ui.label(format!(
                        "↑ {} sent   ↓ {} received",
                        node.sent, node.received
                    ));

                    let usable: Vec<String> = node
                        .subscriptions
                        .iter()
                        .chain(&node.internal_links)
                        .map(|&l| snap.link_name(l).to_owned())
                        .collect();
                    if !usable.is_empty()
                        && let Some(send) = send_ui(ui, &mut self.send_form, id, &usable)
                    {
                        command = Some(send);
                    }
                    section(ui, "Attached to", node.subscriptions.len(), |ui| {
                        for &l in &node.subscriptions {
                            link_button(ui, snap, l, &mut clicked);
                        }
                    });
                    section(ui, "Internal links", node.internal_links.len(), |ui| {
                        for &l in &node.internal_links {
                            link_button(ui, snap, l, &mut clicked);
                        }
                    });
                    section(ui, "Children", node.children.len(), |ui| {
                        for &c in &node.children {
                            node_button(ui, snap, c, &mut clicked);
                        }
                    });
                }
                Some(Item::Link(id)) => {
                    let Some(link) = snap.link(id) else { return };
                    ui.heading(RichText::new(&link.name).color(style::link_color(&link.name)));
                    ui.weak("link");
                    ui.add_space(6.0);
                    egui::Grid::new("link-props").num_columns(2).show(ui, |ui| {
                        ui.weak("ID");
                        ui.monospace(format!("{:#x}", id.raw()));
                        ui.end_row();
                        ui.weak("Internal to");
                        match link.owner {
                            Some(o) => node_button(ui, snap, o, &mut clicked),
                            None => {
                                ui.weak("nothing (floating)");
                            }
                        }
                        ui.end_row();
                    });
                    section(ui, "Subscribers", link.subscribers.len(), |ui| {
                        for &n in &link.subscribers {
                            node_button(ui, snap, n, &mut clicked);
                        }
                    });
                }
            });
        if let Some(node) = enter {
            self.scope = Some(node);
        }
        if clicked.is_some() {
            self.select(clicked, Source::Panel);
        }
        if let Some(command) = command {
            self.run_command(&command);
        }
    }

    fn traffic_log(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Traffic");
            ui.weak(format!(
                "{} packets · {} dropped",
                self.activity.packets, self.activity.drops
            ));
            ui.checkbox(&mut self.log_only_selected, "Only the selected node");
            if ui.button("Clear").clicked() {
                self.activity.log.clear();
            }
        });
        let only = match (self.log_only_selected, self.selection) {
            (true, Some(Item::Node(node))) => Some(node),
            _ => None,
        };
        let lines: Vec<&activity::LogLine> = self
            .activity
            .log
            .iter()
            .filter(|l| only.is_none_or(|n| l.nodes.contains(&n)))
            .collect();
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_rows(ui, row_height, lines.len(), |ui, range| {
                for line in &lines[range] {
                    let text = RichText::new(format!("t{:<6}{}", line.tick, line.text))
                        .monospace()
                        .color(line.color);
                    // One line per row keeps scrolling exact; hover shows the full text.
                    ui.add(egui::Label::new(text).truncate());
                }
            });
    }

    /// Lets you try different fuse limits against the running network.
    fn limits_window(&mut self, ctx: &egui::Context) {
        let Some(edit) = &mut self.limits_edit else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        egui::Window::new("Fuse limits")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.weak("The last line of defence. When a tick reaches one of these, the library\nholds the rest back and the sandbox pauses.");
                egui::Grid::new("limits").num_columns(2).show(ui, |ui| {
                    ui.label("Packets sent per tick");
                    ui.add(egui::DragValue::new(&mut edit.max_transmissions_per_tick).range(1..=10_000_000));
                    ui.end_row();
                    ui.label("Deliveries per tick");
                    ui.add(egui::DragValue::new(&mut edit.max_deliveries_per_tick).range(1..=100_000_000));
                    ui.end_row();
                    ui.label("Queued packets");
                    ui.add(egui::DragValue::new(&mut edit.max_pending).range(1..=10_000_000));
                    ui.end_row();
                    ui.label("Payload bytes");
                    ui.add(egui::DragValue::new(&mut edit.max_payload_bytes).range(1..=16_777_216));
                    ui.end_row();
                });
                apply = ui.button("Apply to this world").clicked();
            });
        if apply && let (Some(l), Ok(loaded)) = (&self.limits_edit, &mut self.loaded) {
            let result = loaded
                .world
                .set_limits(
                    l.max_transmissions_per_tick,
                    l.max_deliveries_per_tick,
                    l.max_pending,
                    l.max_payload_bytes,
                )
                .and_then(|()| loaded.world.health());
            match result {
                Ok(h) => self.health = h,
                Err(e) => self.sim_error = Some(e.to_string()),
            }
        }
        if !open || apply {
            self.limits_edit = None;
        }
    }

    fn alerts_list(&mut self, ui: &mut egui::Ui) {
        let Ok(loaded) = &self.loaded else { return };
        let snap = &loaded.snapshot;
        if self.activity.alerts.is_empty() {
            ui.weak("No alerts. The monitor watches for loops, replays, amplification and floods.");
            return;
        }
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for a in &self.activity.alerts {
                    ui.horizontal(|ui| {
                        ui.monospace(format!("t{:<6}", a.tick));
                        ui.colored_label(
                            style::severity_color(a.severity),
                            format!("⚠ {}", a.check),
                        );
                        match a.subject {
                            Some(health::Subject::Node(n)) => {
                                node_button(ui, snap, n, &mut clicked);
                            }
                            Some(health::Subject::Link(l)) => {
                                link_button(ui, snap, l, &mut clicked);
                            }
                            None => {
                                ui.weak("network");
                            }
                        }
                        ui.label(&a.message);
                    });
                }
            });
        if clicked.is_some() {
            self.select(clicked, Source::Panel);
        }
    }

    /// The last line of defence, made visible: the fuse tripped, the world is paused, and here
    /// is the library's best explanation of why.
    fn fuse_banner(&mut self, ui: &mut egui::Ui) {
        let (Some(report), Ok(loaded)) = (&self.fuse, &self.loaded) else {
            return;
        };
        let snap = &loaded.snapshot;
        let mut dismiss = false;
        let mut resume = false;
        let mut clicked = None;
        egui::Frame::NONE
            .fill(style::DROP.gamma_multiply(0.18))
            .stroke(egui::Stroke::new(1.5_f32, style::DROP))
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("⛔ Last line of defence: the fuse tripped. World paused.")
                            .color(style::DROP)
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        dismiss = ui.button("Dismiss").clicked();
                        resume = ui
                            .button("Resume anyway")
                            .on_hover_text("Keep ticking. The fuse will keep holding traffic back each tick it is exceeded.")
                            .clicked();
                    });
                });
                ui.label(format!(
                    "Tick {}: hit the limit of {}. {} packets held back.",
                    report.tick,
                    report.limit_text(),
                    report.held_back
                ));
                ui.horizontal_wrapped(|ui| {
                    ui.weak("Busiest senders:");
                    for &(n, count) in &report.top_senders {
                        node_button(ui, snap, n, &mut clicked);
                        ui.weak(format!("({count})"));
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.weak("Busiest links:");
                    for &(l, count) in &report.top_links {
                        link_button(ui, snap, l, &mut clicked);
                        ui.weak(format!("({count})"));
                    }
                });
                if report.recent_alerts.is_empty() {
                    ui.weak("No monitor alerts point at a cause.");
                } else {
                    ui.weak("Likely causes (earliest first):");
                    for a in &report.recent_alerts {
                        ui.horizontal(|ui| {
                            ui.colored_label(style::severity_color(a.severity), format!("⚠ {}", a.check));
                            ui.label(format!("tick {}: {}", a.tick, a.message));
                        });
                    }
                }
            });
        if clicked.is_some() {
            self.select(clicked, Source::Panel);
        }
        if resume {
            self.clock.playing = true;
            self.fuse = None;
        } else if dismiss {
            self.fuse = None;
        }
    }

    fn breadcrumbs(&mut self, ui: &mut egui::Ui) {
        let (Ok(loaded), Some(scope)) = (&self.loaded, self.scope) else {
            return;
        };
        let snap = &loaded.snapshot;
        let mut go = None;
        ui.horizontal_wrapped(|ui| {
            let path = snap.path_to(scope);
            for (i, &id) in path.iter().enumerate() {
                if i > 0 {
                    ui.weak("›");
                }
                let name = if id == snap.root() {
                    "world"
                } else {
                    snap.node_name(id)
                };
                if id == scope {
                    ui.strong(name);
                } else if ui.link(name).clicked() {
                    go = Some(id);
                }
            }
            if let Some(node) = snap.node(scope)
                && !node.subscriptions.is_empty()
            {
                ui.separator();
                ui.weak("connects out via");
                for &l in &node.subscriptions {
                    let name = snap.link_name(l);
                    ui.colored_label(style::link_color(name), name);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Re-layout").clicked() {
                    self.graph.relayout(scope);
                }
                if ui.button("Fit").clicked() {
                    self.graph.fit(scope);
                }
            });
        });
        if let Some(id) = go {
            self.scope = Some(id);
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (Ok(loaded), Some(scope)) = (&self.loaded, self.scope) else {
            return;
        };
        let snap = &loaded.snapshot;
        let now = ui.input(|i| i.time);
        self.activity.expire(now);
        let marks = self.activity.marks(snap.tick().saturating_sub(100));
        let overlay = graph_view::Overlay {
            activity: &self.activity,
            marks: &marks,
            now,
        };
        match self.graph.show(ui, snap, scope, self.selection, overlay) {
            Some(ViewAction::Select(item)) => self.select(item, Source::Canvas),
            Some(ViewAction::Enter(node)) => self.scope = Some(node),
            Some(ViewAction::Up) => {
                if let Some(parent) = snap.node(scope).and_then(|n| n.parent) {
                    self.scope = Some(parent);
                }
            }
            None => {}
        }
    }
}

impl eframe::App for SandboxApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(capture) = &mut self.capture {
            capture.update(ctx);
        }
        if !ctx.wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.clock.playing = !self.clock.playing;
        }
        self.tests.poll(ctx);
        self.handle_control(ctx);
        self.advance(ctx);
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });

        let problem = match (&self.library, &self.loaded) {
            (Err(e), _) => Some(format!(
                "{e}\n\nBuild the native library first: run the sandbox with `cargo sandbox`."
            )),
            (Ok(_), Err(e)) => Some(e.clone()),
            _ => None,
        };
        if let Some(problem) = problem {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.colored_label(ui.visuals().error_fg_color, problem);
            });
            return;
        }

        egui::SidePanel::left("hierarchy")
            .default_width(260.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.strong("Hierarchy");
                ui.separator();
                self.hierarchy(ui);
            });
        egui::SidePanel::right("inspector")
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.strong("Inspector");
                ui.separator();
                self.inspector(ui);
            });
        egui::TopBottomPanel::bottom("bottom")
            .resizable(true)
            .default_height(220.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.bottom_tab, BottomTab::Traffic, "Traffic");
                    let alerts = self.activity.alerts.len();
                    ui.selectable_value(
                        &mut self.bottom_tab,
                        BottomTab::Alerts,
                        format!("Alerts ({alerts})"),
                    );
                    let (passed, failed, total) = self.tests.counts();
                    let label = if self.tests.is_running() {
                        format!("Tests (running… {passed}/{total})")
                    } else if failed > 0 {
                        format!("Tests ({failed} failed)")
                    } else {
                        format!("Tests ({passed}/{total})")
                    };
                    ui.selectable_value(&mut self.bottom_tab, BottomTab::Tests, label);
                });
                ui.separator();
                match self.bottom_tab {
                    BottomTab::Traffic => self.traffic_log(ui),
                    BottomTab::Alerts => self.alerts_list(ui),
                    BottomTab::Tests => {
                        let library = self.library.as_ref().ok().cloned();
                        if let Some(tests_panel::TestAction::Watch(i)) =
                            self.tests.ui(ui, library.as_ref())
                        {
                            self.watching = Some(i);
                            self.build();
                        }
                    }
                }
            });
        self.limits_window(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .fill(ui.visuals().panel_fill)
                    .show(ui, |ui| self.breadcrumbs(ui));
                self.fuse_banner(ui);
                self.canvas(ui);
            });
    }
}

// ---- Panel widgets -----------------------------------------------------------------------

fn tree_state_id(node: NodeId) -> egui::Id {
    egui::Id::new(("tree", node.raw()))
}

/// The node whose level shows `item`: a node's parent, or a link's owner.
fn container_of(snap: &Snapshot, item: Item) -> NodeId {
    match item {
        Item::Node(node) => snap.node(node).and_then(|n| n.parent),
        Item::Link(link) => snap.link(link).and_then(|l| l.owner),
    }
    .unwrap_or(snap.root())
}

/// Expands the tree rows from the root down to and including `node`.
fn expand_tree_to(ctx: &egui::Context, snap: &Snapshot, node: NodeId) {
    for id in snap.path_to(node) {
        let mut state = CollapsingState::load_with_default_open(ctx, tree_state_id(id), false);
        state.set_open(true);
        state.store(ctx);
    }
}

/// Deepest level the hierarchy tree draws. Players can nest thousands deep; drawing every level
/// recursively would overflow the stack, and nobody reads a tree that deep anyway.
const MAX_TREE_DEPTH: usize = 40;

fn tree_node(
    ui: &mut egui::Ui,
    snap: &Snapshot,
    id: NodeId,
    selection: Option<Item>,
    reveal: bool,
    clicked: &mut Option<Item>,
    depth: usize,
) {
    let Some(node) = snap.node(id) else { return };
    if depth >= MAX_TREE_DEPTH {
        let hidden = 1 + snap.descendant_count(id);
        if ui
            .link(format!(
                "⋯ {hidden} more nested nodes (open {} on the canvas)",
                node.name
            ))
            .clicked()
        {
            *clicked = Some(Item::Node(id));
        }
        return;
    }
    let selected = selection == Some(Item::Node(id));
    let name = if id == snap.root() {
        "world"
    } else {
        node.name.as_str()
    };
    let row = |ui: &mut egui::Ui, clicked: &mut Option<Item>| {
        let response = ui.selectable_label(selected, name);
        ui.colored_label(
            style::kind_color(&node.kind).gamma_multiply(0.8),
            &node.kind,
        );
        if response.clicked() {
            *clicked = Some(Item::Node(id));
        }
        if selected && reveal {
            response.scroll_to_me(Some(egui::Align::Center));
        }
    };

    if node.children.is_empty() && node.internal_links.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().indent);
            row(ui, clicked);
        });
        return;
    }
    let open_by_default = id == snap.root();
    CollapsingState::load_with_default_open(ui.ctx(), tree_state_id(id), open_by_default)
        .show_header(ui, |ui| row(ui, clicked))
        .body(|ui| {
            for &link in &node.internal_links {
                let link_name = snap.link_name(link);
                let selected = selection == Some(Item::Link(link));
                ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().indent);
                    let text = RichText::new(format!("━ {link_name}"))
                        .color(style::link_color(link_name))
                        .monospace();
                    if ui.selectable_label(selected, text).clicked() {
                        *clicked = Some(Item::Link(link));
                    }
                });
            }
            for &child in &node.children {
                tree_node(ui, snap, child, selection, reveal, clicked, depth + 1);
            }
        });
}

fn section(ui: &mut egui::Ui, title: &str, count: usize, body: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    ui.strong(format!("{title} ({count})"));
    if count == 0 {
        ui.weak("none");
    } else {
        body(ui);
    }
}

fn node_button(ui: &mut egui::Ui, snap: &Snapshot, id: NodeId, clicked: &mut Option<Item>) {
    let name = if id == snap.root() {
        "world"
    } else {
        snap.node_name(id)
    };
    if ui.link(name).clicked() {
        *clicked = Some(Item::Node(id));
    }
}

fn link_button(ui: &mut egui::Ui, snap: &Snapshot, id: LinkId, clicked: &mut Option<Item>) {
    let name = snap.link_name(id);
    let text = RichText::new(format!("━ {name}"))
        .color(style::link_color(name))
        .monospace();
    if ui.link(text).clicked() {
        *clicked = Some(Item::Link(id));
    }
}

/// The inspector's send controls. Returns a command if the user sent something.
fn send_ui(
    ui: &mut egui::Ui,
    form: &mut SendForm,
    node: NodeId,
    usable: &[String],
) -> Option<Command> {
    if form.node != Some(node) {
        *form = SendForm {
            node: Some(node),
            via: usable[0].clone(),
            to: format!("*@{}", usable[0]),
            kind: "ping".into(),
            data: "hello".into(),
            result: None,
        };
    }
    let mut command = None;
    ui.add_space(8.0);
    ui.strong("Send");
    for link in usable {
        if ui.button(format!("Ping everyone on {link}")).clicked() {
            command = Some(Command::Send {
                node,
                via: link.clone(),
                to: format!("*@{link}"),
                kind: "ping".into(),
                data: "sandbox".into(),
            });
        }
    }
    ui.add_space(4.0);
    egui::Grid::new("send-form").num_columns(2).show(ui, |ui| {
        ui.weak("Via");
        egui::ComboBox::from_id_salt("send-via")
            .selected_text(&form.via)
            .show_ui(ui, |ui| {
                for link in usable {
                    ui.selectable_value(&mut form.via, link.clone(), link);
                }
            });
        ui.end_row();
        ui.weak("To");
        ui.text_edit_singleline(&mut form.to)
            .on_hover_text("node@link, several hops joined by /, * for everyone, ^ for the parent");
        ui.end_row();
        ui.weak("Event");
        ui.text_edit_singleline(&mut form.kind);
        ui.end_row();
        ui.weak("Data");
        ui.text_edit_singleline(&mut form.data);
        ui.end_row();
    });
    if ui.button("Send").clicked() {
        command = Some(Command::Send {
            node,
            via: form.via.clone(),
            to: form.to.clone(),
            kind: form.kind.clone(),
            data: form.data.clone(),
        });
    }
    match &form.result {
        Some(Ok(message)) => {
            ui.weak(message);
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        None => {}
    }
    command
}
