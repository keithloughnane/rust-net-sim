//! Interactive test harness for the Emergence Engine.
//!
//! The sandbox talks to the engine only through the compiled native library and its C ABI,
//! the same way Unity or Unreal will. Run it with `cargo sandbox`, which builds that library
//! first.

mod activity;
mod capture;
mod graph_view;
mod native;
mod scenarios;
mod snapshot;
mod style;
mod trace;

use std::sync::Arc;

use activity::Activity;
use eframe::egui::{self, RichText, collapsing_header::CollapsingState};
use graph_view::{GraphView, Item, ViewAction};
use native::{LinkId, NativeLibrary, NativeWorld, NodeId};
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
            Ok(Box::new(SandboxApp::new(capture::Options::from_env())))
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
}

impl SandboxApp {
    fn new(options: capture::Options) -> Self {
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
            capture: options.screenshot.map(capture::Capture::new),
            clock: Clock::default(),
            activity: Activity::default(),
            log_only_selected: false,
            send_form: SendForm::default(),
            sim_error: None,
        };
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
        let Ok(library) = &self.library else { return };
        let scenario = scenarios::ALL[self.scenario];
        self.loaded = NativeWorld::new(library.clone())
            .and_then(|mut world| {
                scenario.build(&mut world)?;
                let snapshot = world.snapshot()?;
                Ok(Loaded { world, snapshot })
            })
            .map_err(|e| format!("Building “{}” failed: {e}", scenario.name));
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
            ui.heading("Emergence Sandbox");
            ui.separator();
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
            if ui.button("⟲ Rebuild").clicked() || before != self.scenario {
                self.build();
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
            }
            if let Some(e) = &self.sim_error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Ok(library) = &self.library {
                    ui.weak(format!("libemergence v{}", library.version()))
                        .on_hover_text(library.path().display().to_string());
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
        let Ok(loaded) = &mut self.loaded else { return };
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
        let result = (0..steps)
            .try_for_each(|_| loaded.world.tick())
            .and_then(|()| {
                let trace = loaded.world.drain_trace()?;
                loaded.snapshot = loaded.world.snapshot()?;
                Ok(trace)
            });
        match result {
            Ok(trace) => {
                let now = ctx.input(|i| i.time);
                self.activity.ingest(&loaded.snapshot, trace, now, duration);
            }
            Err(e) => {
                self.sim_error = Some(e.to_string());
                clock.playing = false;
            }
        }
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
        let logic_kinds: Vec<String> = self
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
                            for kind in &logic_kinds {
                                ui.selectable_value(&mut chosen, kind.clone(), kind);
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
        match self
            .graph
            .show(ui, snap, scope, self.selection, &self.activity, now)
        {
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
        egui::TopBottomPanel::bottom("traffic")
            .resizable(true)
            .default_height(180.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                self.traffic_log(ui);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .fill(ui.visuals().panel_fill)
                    .show(ui, |ui| self.breadcrumbs(ui));
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

fn tree_node(
    ui: &mut egui::Ui,
    snap: &Snapshot,
    id: NodeId,
    selection: Option<Item>,
    reveal: bool,
    clicked: &mut Option<Item>,
) {
    let Some(node) = snap.node(id) else { return };
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
                tree_node(ui, snap, child, selection, reveal, clicked);
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
