//! The "Add node" window: build a node from the templates library and plug it into the level
//! being viewed. Custom nodes will come later.

use std::collections::BTreeSet;

use eframe::egui::{self, RichText};

use crate::native::{LinkId, NodeId, TemplateCatalog};
use crate::snapshot::Snapshot;
use crate::style;

/// Where the new node attaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkTarget {
    /// Nested in the parent, on no link.
    None,
    /// One of the parent's own links.
    Existing(LinkId),
    /// A new link, owned by the parent.
    New(String),
}

/// A request to build and attach a node.
#[derive(Debug, Clone)]
pub(crate) struct AddRequest {
    pub(crate) template: String,
    pub(crate) name: String,
    pub(crate) spec_json: String,
    pub(crate) parent: NodeId,
    pub(crate) link: LinkTarget,
}

/// The window's state, kept between frames while it is open.
#[derive(Debug, Default)]
pub(crate) struct AddNodeWindow {
    pub(crate) open: bool,
    template: String,
    name: String,
    apps: BTreeSet<String>,
    hardware: BTreeSet<String>,
    role: String,
    day_length: u64,
    schedule: String,
    lines: String,
    link: Option<LinkTarget>,
    new_link: String,
    /// The result of the last attempt, shown under the button.
    pub(crate) message: Option<Result<String, String>>,
}

impl AddNodeWindow {
    /// Opens the window with sensible defaults for `catalog`.
    pub(crate) fn open(&mut self, catalog: &TemplateCatalog) {
        if self.template.is_empty() {
            self.template = catalog
                .templates
                .first()
                .map_or_else(String::new, |t| t.name.clone());
            self.reset_npc(catalog);
            self.apps = ["fileman", "mail"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            self.new_link = "cable".into();
        }
        self.name.clear();
        self.message = None;
        self.open = true;
    }

    fn reset_npc(&mut self, catalog: &TemplateCatalog) {
        let npc = &catalog.defaults["npc"];
        npc["role"]
            .as_str()
            .unwrap_or("civilian")
            .clone_into(&mut self.role);
        self.day_length = npc["day_length"].as_u64().unwrap_or(240);
        self.schedule = npc["schedule"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| format!("{} {}", e[0], e[1].as_str().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        self.lines = npc["lines"]
            .as_array()
            .map(|l| {
                l.iter()
                    .filter_map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
    }

    /// The spec JSON for the chosen template and settings.
    fn spec_json(&self) -> Result<String, String> {
        match self.template.as_str() {
            "computer" => Ok(serde_json::json!({
                "apps": self.apps,
                "hardware": self.hardware,
            })
            .to_string()),
            "npc" => {
                let schedule = parse_schedule(&self.schedule)?;
                let lines: Vec<&str> = self
                    .lines
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();
                Ok(serde_json::json!({
                    "role": self.role,
                    "day_length": self.day_length,
                    "schedule": schedule,
                    "lines": lines,
                })
                .to_string())
            }
            other => Err(format!("unknown template `{other}`")),
        }
    }

    /// Draws the window. Returns a request when the user presses Add.
    pub(crate) fn ui(
        &mut self,
        ctx: &egui::Context,
        catalog: &TemplateCatalog,
        snap: &Snapshot,
        scope: NodeId,
    ) -> Option<AddRequest> {
        if !self.open {
            return None;
        }
        let mut open = true;
        let mut request = None;
        let scope_node = snap.node(scope)?;
        let links: Vec<LinkId> = scope_node.internal_links.clone();
        if self.link.is_none()
            || matches!(&self.link, Some(LinkTarget::Existing(l)) if !links.contains(l))
        {
            self.link = Some(
                links
                    .first()
                    .map_or(LinkTarget::None, |&l| LinkTarget::Existing(l)),
            );
        }
        if self.name.is_empty() {
            self.name = suggest_name(snap, if self.template == "npc" { "npc" } else { "pc" });
        }

        egui::Window::new("Add node")
            .open(&mut open)
            .resizable(false)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("From");
                    let before = self.template.clone();
                    egui::ComboBox::from_id_salt("add-template")
                        .selected_text(&self.template)
                        .show_ui(ui, |ui| {
                            for t in &catalog.templates {
                                ui.selectable_value(&mut self.template, t.name.clone(), &t.name)
                                    .on_hover_text(&t.description);
                            }
                            ui.add_enabled(
                                false,
                                egui::Button::selectable(false, "custom node (coming later)"),
                            );
                        });
                    if before != self.template {
                        self.name.clear();
                    }
                });
                if let Some(t) = catalog.templates.iter().find(|t| t.name == self.template) {
                    ui.weak(&t.description);
                }
                ui.separator();

                self.placement_ui(ui, snap, scope, &links);
                ui.separator();

                match self.template.as_str() {
                    "computer" => self.computer_ui(ui, catalog),
                    "npc" => self.npc_ui(ui, catalog),
                    _ => {}
                }
                ui.separator();

                if ui.button(RichText::new("+ Add").strong()).clicked() {
                    let link = match self.link.clone().unwrap_or(LinkTarget::None) {
                        LinkTarget::New(_) => LinkTarget::New(self.new_link.trim().to_owned()),
                        other => other,
                    };
                    match self.spec_json() {
                        Ok(spec_json) => {
                            request = Some(AddRequest {
                                template: self.template.clone(),
                                name: self.name.trim().to_owned(),
                                spec_json,
                                parent: scope,
                                link,
                            });
                        }
                        Err(e) => self.message = Some(Err(e)),
                    }
                }
                match &self.message {
                    Some(Ok(m)) => {
                        ui.colored_label(style::PASS, m);
                    }
                    Some(Err(e)) => {
                        ui.colored_label(style::DROP, e);
                    }
                    None => {}
                }
            });
        if !open {
            self.open = false;
        }
        request
    }

    fn placement_ui(
        &mut self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        scope: NodeId,
        links: &[LinkId],
    ) {
        egui::Grid::new("add-common").num_columns(2).show(ui, |ui| {
            ui.label("Name");
            ui.text_edit_singleline(&mut self.name);
            ui.end_row();
            ui.label("Inside");
            ui.label(RichText::new(name_of(snap, scope)).strong())
                .on_hover_text("The level you are viewing. Open another node to add inside it.");
            ui.end_row();
            ui.label("On link");
            let current = self.link.clone().unwrap_or(LinkTarget::None);
            let label = match &current {
                LinkTarget::None => "none".to_owned(),
                LinkTarget::Existing(l) => snap.link_name(*l).to_owned(),
                LinkTarget::New(_) => "new link…".to_owned(),
            };
            egui::ComboBox::from_id_salt("add-link")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for &l in links {
                        let name = snap.link_name(l);
                        ui.selectable_value(
                            &mut self.link,
                            Some(LinkTarget::Existing(l)),
                            RichText::new(name).color(style::link_color(name)),
                        );
                    }
                    ui.selectable_value(
                        &mut self.link,
                        Some(LinkTarget::New(String::new())),
                        "new link…",
                    );
                    ui.selectable_value(&mut self.link, Some(LinkTarget::None), "none");
                });
            ui.end_row();
            if matches!(self.link, Some(LinkTarget::New(_))) {
                ui.label("Link name");
                ui.text_edit_singleline(&mut self.new_link);
                ui.end_row();
            }
        });
    }

    fn computer_ui(&mut self, ui: &mut egui::Ui, catalog: &TemplateCatalog) {
        ui.strong("Apps");
        egui::Grid::new("add-apps").num_columns(3).show(ui, |ui| {
            for (i, app) in catalog.apps.iter().enumerate() {
                let mut on = self.apps.contains(&app.name);
                if ui
                    .checkbox(&mut on, &app.name)
                    .on_hover_text(&app.title)
                    .changed()
                {
                    if on {
                        self.apps.insert(app.name.clone());
                    } else {
                        self.apps.remove(&app.name);
                    }
                }
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
        ui.strong("Hardware");
        ui.horizontal(|ui| {
            for tag in &catalog.hardware {
                let mut on = self.hardware.contains(tag);
                if ui.checkbox(&mut on, tag).changed() {
                    if on {
                        self.hardware.insert(tag.clone());
                    } else {
                        self.hardware.remove(tag);
                    }
                }
            }
        });
    }

    fn npc_ui(&mut self, ui: &mut egui::Ui, catalog: &TemplateCatalog) {
        egui::Grid::new("add-npc").num_columns(2).show(ui, |ui| {
            ui.label("Role");
            ui.horizontal(|ui| {
                for role in &catalog.npc_roles {
                    ui.radio_value(&mut self.role, role.clone(), role);
                }
            });
            ui.end_row();
            ui.label("Day length");
            ui.add(
                egui::DragValue::new(&mut self.day_length)
                    .range(1..=100_000)
                    .suffix(" ticks"),
            );
            ui.end_row();
            ui.label("Schedule");
            ui.text_edit_singleline(&mut self.schedule)
                .on_hover_text("tick goal, tick goal, … starting at 0, e.g. 0 sleep, 60 work");
            ui.end_row();
            ui.label("Lines");
            ui.text_edit_multiline(&mut self.lines)
                .on_hover_text("One line of dialogue per row");
            ui.end_row();
        });
    }

    /// Records how an add went; on success, suggests a fresh name for the next one.
    pub(crate) fn finished(&mut self, result: Result<String, String>) {
        if result.is_ok() {
            self.name.clear();
        }
        self.message = Some(result);
    }
}

/// Parses `0 sleep, 60 work` into `[[0, "sleep"], [60, "work"]]`.
pub(crate) fn parse_schedule(text: &str) -> Result<Vec<(u64, String)>, String> {
    text.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|entry| {
            let (tick, goal) = entry
                .split_once(char::is_whitespace)
                .ok_or_else(|| format!("schedule entry `{entry}` should be `tick goal`"))?;
            let tick = tick
                .parse()
                .map_err(|_| format!("`{tick}` in the schedule is not a tick number"))?;
            Ok((tick, goal.trim().to_owned()))
        })
        .collect()
}

/// `prefix-N` with the smallest N no node already uses.
pub(crate) fn suggest_name(snap: &Snapshot, prefix: &str) -> String {
    // With N nodes, one of the first N + 1 names is always free.
    (1..=snap.node_count() + 1)
        .map(|n| format!("{prefix}-{n}"))
        .find(|name| snap.find_node(name).is_none())
        .unwrap_or_else(|| prefix.to_owned())
}

fn name_of(snap: &Snapshot, id: NodeId) -> &str {
    if id == snap.root() {
        "world"
    } else {
        snap.node_name(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_schedules() {
        assert_eq!(
            parse_schedule("0 sleep, 60 go to work"),
            Ok(vec![(0, "sleep".into()), (60, "go to work".into())])
        );
        assert!(parse_schedule("soon sleep").is_err());
        assert!(parse_schedule("0").is_err());
    }
}
