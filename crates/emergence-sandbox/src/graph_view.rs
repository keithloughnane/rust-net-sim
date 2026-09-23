//! The network canvas.
//!
//! Shows one level of the hierarchy at a time (the children of the current *scope* node), like
//! the game's `NetScan` view, but draws each link as its own "bus" object with a spoke to every
//! subscriber. Links are shared broadcast media, not point-to-point pipes, and this makes that
//! visible. Items are placed by a small force simulation that settles on screen; dragging an
//! item pins it.

use std::collections::{HashMap, HashSet};

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, vec2,
};

use crate::activity::{Activity, GLOW_SECONDS};
use crate::native::{LinkId, NodeId};
use crate::snapshot::Snapshot;
use crate::style;

/// Something that can be drawn, hovered and selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Item {
    Node(NodeId),
    Link(LinkId),
}

/// What the user asked for this frame.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ViewAction {
    Select(Option<Item>),
    Enter(NodeId),
    Up,
}

const NODE_SIZE: Vec2 = vec2(150.0, 54.0);
const LINK_HEIGHT: f32 = 26.0;
/// Ideal distance between connected items, in world units.
const SPRING_LENGTH: f32 = 130.0;
/// Items further apart than this do not repel, so separate clusters can sit close together.
const REPULSION_RANGE: f32 = SPRING_LENGTH * 3.0;
/// Pull towards the centre, per unit of distance.
const GRAVITY: f32 = 0.35;
const START_HEAT: f32 = 60.0;
const MIN_HEAT: f32 = 0.4;
const MIN_ZOOM: f32 = 0.08;
const MAX_ZOOM: f32 = 4.0;

/// The items and spokes visible in one scope.
struct Scene {
    items: Vec<Item>,
    /// Links shown because a child uses them, although the scope does not own them.
    outer_links: HashSet<LinkId>,
    edges: Vec<(NodeId, LinkId)>,
}

impl Scene {
    fn new(snap: &Snapshot, scope: NodeId) -> Self {
        let Some(scope_node) = snap.node(scope) else {
            return Self {
                items: Vec::new(),
                outer_links: HashSet::new(),
                edges: Vec::new(),
            };
        };
        let own: HashSet<LinkId> = scope_node.internal_links.iter().copied().collect();
        let mut items: Vec<Item> = scope_node
            .internal_links
            .iter()
            .map(|&l| Item::Link(l))
            .collect();
        let mut outer_links = HashSet::new();
        let mut edges = Vec::new();
        for &child in &scope_node.children {
            items.push(Item::Node(child));
            for &link in snap.node(child).map_or(&[][..], |c| &c.subscriptions) {
                edges.push((child, link));
                if !own.contains(&link) && outer_links.insert(link) {
                    items.push(Item::Link(link));
                }
            }
        }
        Self {
            items,
            outer_links,
            edges,
        }
    }

    fn has_edge(&self, item: Item, other: Item) -> bool {
        self.edges.iter().any(|&(n, l)| {
            (item == Item::Node(n) && other == Item::Link(l))
                || (item == Item::Link(l) && other == Item::Node(n))
        })
    }
}

/// Size of an item's box in world units.
fn item_size(snap: &Snapshot, item: Item) -> Vec2 {
    match item {
        Item::Node(_) => NODE_SIZE,
        Item::Link(id) => {
            #[allow(clippy::cast_precision_loss)] // Name lengths are tiny.
            let chars = snap.link_name(id).chars().count() as f32;
            vec2((34.0 + chars * 7.4).clamp(70.0, 240.0), LINK_HEIGHT)
        }
    }
}

/// Positions and camera for one scope. Kept per scope so going back restores the view.
#[derive(Debug)]
struct ScopeView {
    pos: HashMap<Item, Vec2>,
    pinned: HashSet<Item>,
    heat: f32,
    pan: Vec2,
    zoom: f32,
    /// Keep zooming to fit while the layout settles, until the user moves the camera.
    auto_fit: bool,
}

impl ScopeView {
    fn new() -> Self {
        Self {
            pos: HashMap::new(),
            pinned: HashSet::new(),
            heat: START_HEAT,
            pan: Vec2::ZERO,
            zoom: 1.0,
            auto_fit: true,
        }
    }

    /// Adds new items at a sensible starting point and forgets removed ones.
    fn sync(&mut self, scene: &Scene) {
        let alive: HashSet<Item> = scene.items.iter().copied().collect();
        self.pos.retain(|item, _| alive.contains(item));
        self.pinned.retain(|item| alive.contains(item));
        if scene.items.iter().all(|i| self.pos.contains_key(i)) {
            return;
        }

        // Links on an inner ring; each node near the first link it uses; loners on an outer ring.
        let links: Vec<Item> = scene
            .items
            .iter()
            .copied()
            .filter(|i| matches!(i, Item::Link(_)))
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let ring = 60.0 + 45.0 * links.len() as f32;
        for (i, &link) in links.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let angle = std::f32::consts::TAU * i as f32 / links.len() as f32;
            self.pos
                .entry(link)
                .or_insert_with(|| Vec2::angled(angle) * ring);
        }
        let golden = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
        for (i, &item) in scene.items.iter().enumerate() {
            let Item::Node(node) = item else { continue };
            if self.pos.contains_key(&item) {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let spin = Vec2::angled(golden * i as f32);
            let anchor = scene
                .edges
                .iter()
                .find(|&&(n, _)| n == node)
                .and_then(|&(_, l)| self.pos.get(&Item::Link(l)).copied());
            let p = match anchor {
                Some(a) => a * 1.6 + spin * 90.0,
                None => spin * (ring * 2.0 + 220.0),
            };
            self.pos.insert(item, p);
        }
        self.heat = START_HEAT;
    }

    /// One step of a Fruchterman–Reingold style simulation. Returns whether anything moved.
    fn step(&mut self, snap: &Snapshot, scene: &Scene) -> bool {
        if self.heat < MIN_HEAT {
            return false;
        }
        let k = SPRING_LENGTH;
        let items = &scene.items;
        let radius: Vec<f32> = items
            .iter()
            .map(|&i| item_size(snap, i).length() * 0.5)
            .collect();
        let pos: Vec<Vec2> = items
            .iter()
            .map(|i| self.pos.get(i).copied().unwrap_or_default())
            .collect();
        let mut disp = vec![Vec2::ZERO; items.len()];

        // Everything repels everything, strongly when boxes overlap.
        for a in 0..items.len() {
            for b in (a + 1)..items.len() {
                let delta = pos[a] - pos[b];
                let d = delta.length().max(1.0);
                let clearance = radius[a] + radius[b] + 20.0;
                if d > REPULSION_RANGE.max(clearance) {
                    continue;
                }
                let mut force = k * k / d;
                if d < clearance {
                    force += (clearance - d) * 6.0;
                }
                let push = delta / d * force;
                disp[a] += push;
                disp[b] -= push;
            }
        }

        // Spokes pull a node and its links together.
        let index: HashMap<Item, usize> =
            items.iter().enumerate().map(|(i, &it)| (it, i)).collect();
        for &(node, link) in &scene.edges {
            let (Some(&a), Some(&b)) = (index.get(&Item::Node(node)), index.get(&Item::Link(link)))
            else {
                continue;
            };
            let delta = pos[a] - pos[b];
            let d = delta.length().max(1.0);
            let pull = delta / d * (d * d / k);
            disp[a] -= pull;
            disp[b] += pull;
        }

        // Gentle gravity keeps disconnected pieces from drifting away.
        let mut moved = 0.0_f32;
        for (i, item) in items.iter().enumerate() {
            if self.pinned.contains(item) {
                continue;
            }
            let d = disp[i] - pos[i] * GRAVITY;
            let len = d.length();
            if len > 0.0 {
                let step = d / len * len.min(self.heat);
                moved = moved.max(step.length());
                self.pos.insert(*item, pos[i] + step);
            }
        }
        self.heat *= 0.965;
        moved > 0.05
    }

    fn bounds(&self, snap: &Snapshot) -> Option<Rect> {
        self.pos
            .iter()
            .map(|(&item, &p)| Rect::from_center_size(p.to_pos2(), item_size(snap, item)))
            .reduce(Rect::union)
    }

    fn fit(&mut self, snap: &Snapshot, viewport: Rect) {
        if let Some(bounds) = self.bounds(snap) {
            let bounds = bounds.expand(40.0);
            self.zoom = (viewport.width() / bounds.width())
                .min(viewport.height() / bounds.height())
                .clamp(MIN_ZOOM, 1.0);
            self.pan = -bounds.center().to_vec2();
        }
    }
}

/// The canvas widget. Holds per-scope layouts; the network itself comes from the snapshot.
#[derive(Debug, Default)]
pub(crate) struct GraphView {
    scopes: HashMap<NodeId, ScopeView>,
    dragging: Option<Drag>,
}

/// What the current drag gesture is moving.
#[derive(Debug, Clone, Copy)]
enum Drag {
    Item(Item),
    Camera,
}

impl GraphView {
    /// Forgets all layouts, for when the network is rebuilt.
    pub(crate) fn reset(&mut self) {
        self.scopes.clear();
        self.dragging = None;
    }

    /// Re-runs the layout for `scope` from scratch.
    pub(crate) fn relayout(&mut self, scope: NodeId) {
        self.scopes.remove(&scope);
    }

    /// Zooms `scope` to show everything again.
    pub(crate) fn fit(&mut self, scope: NodeId) {
        if let Some(view) = self.scopes.get_mut(&scope) {
            view.auto_fit = true;
        }
    }

    #[allow(clippy::too_many_lines)] // One pass: input, simulation, drawing.
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        snap: &Snapshot,
        scope: NodeId,
        selection: Option<Item>,
        activity: &Activity,
        now: f64,
    ) -> Option<ViewAction> {
        let scene = Scene::new(snap, scope);
        let view = self.scopes.entry(scope).or_insert_with(ScopeView::new);
        view.sync(&scene);

        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, style::CANVAS_BG);

        // Simulate a bounded amount per frame so big scopes stay responsive while settling.
        let steps = (40_000 / scene.items.len().pow(2).max(1)).clamp(1, 12);
        let mut moving = false;
        for _ in 0..steps {
            moving |= view.step(snap, &scene);
        }
        if view.auto_fit {
            view.fit(snap, rect);
        }
        if moving {
            ui.ctx().request_repaint();
        }

        let to_screen = |view: &ScopeView, p: Vec2| rect.center() + (p + view.pan) * view.zoom;
        let to_world = |view: &ScopeView, s: Pos2| (s - rect.center()) / view.zoom - view.pan;
        let hit = |view: &ScopeView, s: Pos2| -> Option<Item> {
            let w = to_world(view, s).to_pos2();
            // Nodes are drawn on top of links, so test them first.
            let mut order: Vec<&Item> = scene.items.iter().collect();
            order.sort_by_key(|i| matches!(i, Item::Link(_)));
            order.into_iter().copied().find(|&item| {
                view.pos.get(&item).is_some_and(|&p| {
                    Rect::from_center_size(p.to_pos2(), item_size(snap, item)).contains(w)
                })
            })
        };

        // ---- Input -----------------------------------------------------------------------
        let mut action = None;
        let hovered = response.hover_pos().and_then(|p| hit(view, p));

        if response.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            self.dragging = Some(
                origin
                    .and_then(|p| hit(view, p))
                    .map_or(Drag::Camera, Drag::Item),
            );
        }
        if response.dragged() {
            let delta = response.drag_delta() / view.zoom;
            match self.dragging {
                Some(Drag::Item(item)) => {
                    if let Some(p) = view.pos.get_mut(&item) {
                        *p += delta;
                    }
                    view.pinned.insert(item);
                    view.heat = view.heat.max(6.0);
                }
                _ => view.pan += delta,
            }
            view.auto_fit = false;
            ui.ctx().request_repaint();
        }
        if response.drag_stopped() {
            self.dragging = None;
        }
        if response.clicked() {
            action = Some(ViewAction::Select(hovered));
        }
        if response.double_clicked()
            && let Some(Item::Node(node)) = hovered
            && snap.node(node).is_some_and(|n| !n.children.is_empty())
        {
            action = Some(ViewAction::Enter(node));
        }
        if response.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = pinch * (scroll * 0.002).exp();
            if (factor - 1.0).abs() > f32::EPSILON
                && let Some(pointer) = response.hover_pos()
            {
                let anchor = to_world(view, pointer);
                view.zoom = (view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
                view.pan = (pointer - rect.center()) / view.zoom - anchor;
                view.auto_fit = false;
            }
            if !ui.ctx().wants_keyboard_input()
                && ui.input(|i| {
                    i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Backspace)
                })
            {
                action = Some(ViewAction::Up);
            }
        }

        // ---- Drawing ---------------------------------------------------------------------
        let zoom = view.zoom;
        let focus = hovered.or(selection);
        let text_size = |base: f32| (base * zoom).clamp(0.0, 40.0);
        let screen_rect = |item: Item| {
            let p = view.pos.get(&item).copied().unwrap_or_default();
            Rect::from_center_size(to_screen(view, p), item_size(snap, item) * zoom)
        };

        draw_grid(&painter, rect, view.pan, zoom);

        for &(node, link) in &scene.edges {
            let (a, b) = (Item::Node(node), Item::Link(link));
            let color = style::link_color(snap.link_name(link));
            let lit = focus.is_some_and(|f| f == a || f == b);
            let stroke = if lit {
                Stroke::new(2.5_f32, color)
            } else {
                Stroke::new(
                    1.5_f32,
                    color.gamma_multiply(if focus.is_some() { 0.25 } else { 0.6 }),
                )
            };
            painter.line_segment([screen_rect(a).center(), screen_rect(b).center()], stroke);
        }

        // Where each node of the network appears at this level: itself if it is a child of the
        // scope, or the child that contains it. `None` for the scope itself and outsiders.
        let rects: HashMap<Item, Rect> = scene.items.iter().map(|&i| (i, screen_rect(i))).collect();
        let place = |node: NodeId| {
            snap.child_of_scope_containing(scope, node)
                .and_then(|c| rects.get(&Item::Node(c)).copied())
        };
        draw_glows(&painter, activity, now, &rects, &place, zoom);

        for &item in &scene.items {
            let r = screen_rect(item);
            let selected = selection == Some(item);
            let near = focus.is_some_and(|f| f == item || scene.has_edge(f, item));
            let dim = focus.is_some() && !near;
            match item {
                Item::Link(link) => {
                    let name = snap.link_name(link);
                    let outer = scene.outer_links.contains(&link);
                    draw_link(&painter, r, name, outer, selected, dim, text_size(12.5));
                }
                Item::Node(node) => {
                    let Some(entry) = snap.node(node) else {
                        continue;
                    };
                    let nested = snap.descendant_count(node);
                    let hovered = hovered == Some(item);
                    draw_node(
                        &painter,
                        r,
                        entry,
                        nested,
                        selected,
                        hovered,
                        dim,
                        text_size(1.0),
                    );
                }
            }
        }

        draw_flights(&painter, activity, now, &rects, &place, zoom);
        if !activity.flights.is_empty() {
            ui.ctx().request_repaint();
        }

        if scene.items.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Nothing inside this node.",
                FontId::proportional(15.0),
                style::TEXT_WEAK,
            );
        }
        painter.text(
            rect.left_bottom() + vec2(10.0, -8.0),
            Align2::LEFT_BOTTOM,
            "Scroll to zoom · drag to pan or move · double-click to open · Esc to go up",
            FontId::proportional(12.0),
            style::TEXT_WEAK,
        );

        action
    }
}

/// Halos behind links carrying a packet and nodes that just received one.
fn draw_glows(
    painter: &egui::Painter,
    activity: &Activity,
    now: f64,
    rects: &HashMap<Item, Rect>,
    place: &dyn Fn(NodeId) -> Option<Rect>,
    zoom: f32,
) {
    let mut glow: HashMap<Rect2, (f32, Color32, f32)> = HashMap::new();
    let mut add = |r: Rect, strength: f32, color: Color32, radius: f32| {
        let e = glow.entry(Rect2(r)).or_insert((0.0, color, radius));
        if strength > e.0 {
            *e = (strength, color, radius);
        }
    };
    for f in &activity.flights {
        let color = style::event_color(&f.kind);
        if now >= f.start
            && now < f.arrival()
            && let Some(&r) = rects.get(&Item::Link(f.link))
        {
            add(r, 0.8, color, r.height() * 0.5);
        }
        let since = now - f.arrival();
        if (0.0..GLOW_SECONDS).contains(&since) {
            #[allow(clippy::cast_possible_truncation)] // A 0..1 fraction.
            let strength = (1.0 - since / GLOW_SECONDS) as f32;
            for &r in &f.receivers {
                if let Some(rect) = place(r) {
                    add(rect, strength, color, 6.0 * zoom);
                }
            }
        }
    }
    for (Rect2(r), (strength, color, radius)) in glow {
        let pad = 7.0 * zoom.max(0.5);
        painter.rect_filled(
            r.expand(pad),
            radius + pad,
            color.gamma_multiply(0.18 * strength),
        );
        painter.rect_stroke(
            r.expand(pad * 0.5),
            radius + pad * 0.5,
            Stroke::new(2.0_f32, color.gamma_multiply(strength)),
            StrokeKind::Outside,
        );
    }
}

/// Dots travelling sender → link → receiver for every packet in flight.
fn draw_flights(
    painter: &egui::Painter,
    activity: &Activity,
    now: f64,
    rects: &HashMap<Item, Rect>,
    place: &dyn Fn(NodeId) -> Option<Rect>,
    zoom: f32,
) {
    let radius = (4.5 * zoom).clamp(2.5, 7.0);
    for f in &activity.flights {
        if now < f.start || now >= f.arrival() {
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
        let t = ((now - f.start) / f.duration) as f32;
        let color = style::event_color(&f.kind);
        let link = rects.get(&Item::Link(f.link)).map(Rect::center);
        let from = place(f.sender).map(|r| r.center());
        let draw = |start: Option<Pos2>, end: Option<Pos2>| {
            let pos = match (start, link, end) {
                (s, Some(l), e) => {
                    if t < 0.5 {
                        s.unwrap_or(l).lerp(l, t * 2.0)
                    } else {
                        l.lerp(e.unwrap_or(l), t * 2.0 - 1.0)
                    }
                }
                // The link is not shown at this level: go straight across if both ends are.
                (Some(s), None, Some(e)) if s != e => s.lerp(e, t),
                _ => return,
            };
            painter.circle_filled(pos, radius * 2.2, color.gamma_multiply(0.25));
            painter.circle_filled(pos, radius, color);
        };
        if f.receivers.is_empty() {
            if t < 0.5 {
                draw(from, None);
            }
        } else {
            for &r in &f.receivers {
                draw(from, place(r).map(|r| r.center()));
            }
        }
    }
}

/// `Rect` as a hash key, for merging glows on the same box.
#[derive(Clone, Copy, PartialEq)]
struct Rect2(Rect);

impl Eq for Rect2 {}

impl std::hash::Hash for Rect2 {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        for v in [self.0.min.x, self.0.min.y, self.0.max.x, self.0.max.y] {
            v.to_bits().hash(state);
        }
    }
}

fn draw_grid(painter: &egui::Painter, rect: Rect, pan: Vec2, zoom: f32) {
    let spacing = 40.0 * zoom;
    if spacing < 12.0 {
        return;
    }
    let origin = rect.center() + pan * zoom;
    let start = origin - ((origin - rect.min) / spacing).floor() * spacing;
    let mut x = start.x;
    while x < rect.max.x {
        let mut y = start.y;
        while y < rect.max.y {
            painter.circle_filled(Pos2::new(x, y), 1.0, style::GRID_DOT);
            y += spacing;
        }
        x += spacing;
    }
}

fn draw_link(
    painter: &egui::Painter,
    r: Rect,
    name: &str,
    outer: bool,
    selected: bool,
    dim: bool,
    font: f32,
) {
    let base = style::link_color(name);
    let color = if dim { base.gamma_multiply(0.35) } else { base };
    let radius = r.height() * 0.5;
    painter.rect_filled(r, radius, style::CANVAS_BG);
    painter.rect_filled(r, radius, color.gamma_multiply(0.22));
    let stroke = Stroke::new(if selected { 2.5_f32 } else { 1.5 }, color);
    if outer {
        // Dashed outline: this link belongs to a node outside the current level.
        let c = [
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
            r.left_top(),
        ];
        painter.extend(Shape::dashed_line(&c, stroke, 6.0, 4.0));
    } else {
        painter.rect_stroke(r, radius, stroke, StrokeKind::Inside);
    }
    if selected {
        painter.rect_stroke(
            r.expand(3.0),
            radius + 3.0,
            Stroke::new(1.0_f32, Color32::WHITE),
            StrokeKind::Outside,
        );
    }
    if font >= 6.0 {
        let label = if outer {
            format!("↗ {name}")
        } else {
            name.to_owned()
        };
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::monospace(font),
            if dim { style::TEXT_WEAK } else { style::TEXT },
        );
    }
}

#[allow(clippy::too_many_arguments)] // Plain drawing helper; a struct would add nothing.
fn draw_node(
    painter: &egui::Painter,
    r: Rect,
    node: &crate::snapshot::NodeEntry,
    nested: usize,
    selected: bool,
    hovered: bool,
    dim: bool,
    scale: f32,
) {
    let rounding = 6.0 * scale;
    let accent = style::kind_color(&node.kind);
    let (fill, border) = if dim {
        (style::NODE_BG_DIM, style::NODE_BORDER.gamma_multiply(0.4))
    } else {
        (style::NODE_BG, style::NODE_BORDER)
    };
    painter.rect_filled(r, rounding, fill);
    let bar = Rect::from_min_size(r.min, vec2(5.0 * scale, r.height()));
    painter.rect_filled(
        bar,
        rounding,
        if dim {
            accent.gamma_multiply(0.4)
        } else {
            accent
        },
    );
    let stroke = match (selected, hovered) {
        (true, _) => Stroke::new(2.5_f32, Color32::WHITE),
        (false, true) => Stroke::new(1.5_f32, style::TEXT),
        _ => Stroke::new(1.0_f32, border),
    };
    painter.rect_stroke(r, rounding, stroke, StrokeKind::Inside);
    if nested > 0 {
        // A stacked-card edge hints that there is more inside.
        let back = r.translate(vec2(4.0, 4.0) * scale);
        painter.rect_stroke(
            back,
            rounding,
            Stroke::new(1.0_f32, border),
            StrokeKind::Inside,
        );
    }

    let title = 14.0 * scale;
    if title < 6.0 {
        return;
    }
    let text_color = if dim { style::TEXT_WEAK } else { style::TEXT };
    let left = r.left() + 14.0 * scale;
    painter.text(
        Pos2::new(left, r.top() + 9.0 * scale),
        Align2::LEFT_TOP,
        truncate(&node.name, 13),
        FontId::proportional(title),
        text_color,
    );
    let label = match &node.logic {
        Some(logic) => format!("{} · {logic}", node.kind),
        None => node.kind.clone(),
    };
    if node.sent + node.received > 0 {
        painter.text(
            Pos2::new(r.right() - 8.0 * scale, r.top() + 9.0 * scale),
            Align2::RIGHT_TOP,
            format!("↑{} ↓{}", node.sent, node.received),
            FontId::monospace(10.0 * scale),
            style::TEXT_WEAK,
        );
    }
    painter.text(
        Pos2::new(left, r.bottom() - 8.0 * scale),
        Align2::LEFT_BOTTOM,
        label,
        FontId::proportional(11.0 * scale),
        if dim { style::TEXT_WEAK } else { accent },
    );
    if nested > 0 {
        painter.text(
            Pos2::new(r.right() - 8.0 * scale, r.bottom() - 8.0 * scale),
            Align2::RIGHT_BOTTOM,
            format!("{nested} ⏵"),
            FontId::proportional(11.0 * scale),
            style::TEXT_WEAK,
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}
