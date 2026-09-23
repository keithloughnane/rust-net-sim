//! The stress-test checklist: run one test or all of them, see each check pass or fail, and
//! load any test into the viewer to watch it.

use std::collections::HashSet;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};

use crate::native::NativeLibrary;
use crate::stress::{self, Runner, RunnerEvent, TestReport};
use crate::style;

/// What the user asked the app to do.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TestAction {
    Watch(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    NotRun,
    Queued,
    Running,
    Passed,
    Failed,
}

#[derive(Debug, Default)]
pub(crate) struct TestsPanel {
    reports: Vec<Option<TestReport>>,
    queued: HashSet<usize>,
    running: Option<usize>,
    runner: Option<Runner>,
    expanded: HashSet<usize>,
}

impl TestsPanel {
    fn status(&self, i: usize) -> Status {
        if self.running == Some(i) {
            Status::Running
        } else if self.queued.contains(&i) {
            Status::Queued
        } else {
            match self.reports.get(i).and_then(Option::as_ref) {
                Some(r) if r.passed => Status::Passed,
                Some(_) => Status::Failed,
                None => Status::NotRun,
            }
        }
    }

    /// `not run`, `queued`, `running`, `passed` or `failed`.
    pub(crate) fn status_name(&self, i: usize) -> &'static str {
        match self.status(i) {
            Status::NotRun => "not run",
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Passed => "passed",
            Status::Failed => "failed",
        }
    }

    pub(crate) fn report(&self, i: usize) -> Option<&TestReport> {
        self.reports.get(i).and_then(Option::as_ref)
    }

    pub(crate) fn is_running(&self) -> bool {
        self.runner.is_some()
    }

    /// (passed, failed, total)
    pub(crate) fn counts(&self) -> (usize, usize, usize) {
        let passed = self.reports.iter().flatten().filter(|r| r.passed).count();
        let failed = self.reports.iter().flatten().filter(|r| !r.passed).count();
        (passed, failed, stress::ALL.len())
    }

    /// Starts running `tests` in the background.
    pub(crate) fn run(&mut self, library: &Arc<NativeLibrary>, tests: Vec<usize>) {
        if self.runner.is_some() || tests.is_empty() {
            return;
        }
        self.reports.resize(stress::ALL.len(), None);
        for &i in &tests {
            self.reports[i] = None;
            self.queued.insert(i);
        }
        self.runner = Some(Runner::start(Arc::clone(library), tests));
    }

    /// Collects progress from the background runner. Call every frame.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        let Some(runner) = &self.runner else { return };
        let mut done = false;
        while let Ok(event) = runner.events.try_recv() {
            match event {
                RunnerEvent::Started(i) => {
                    self.queued.remove(&i);
                    self.running = Some(i);
                }
                RunnerEvent::Finished(i, report) => {
                    if !report.passed {
                        self.expanded.insert(i);
                    }
                    self.reports[i] = Some(*report);
                    self.running = None;
                }
                RunnerEvent::Done => done = true,
            }
        }
        if done {
            self.runner = None;
            self.queued.clear();
            self.running = None;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    #[allow(clippy::too_many_lines)] // One flat checklist layout.
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        library: Option<&Arc<NativeLibrary>>,
    ) -> Option<TestAction> {
        let mut action = None;
        let mut to_run = None;
        ui.horizontal(|ui| {
            let busy = self.runner.is_some();
            if ui
                .add_enabled(!busy && library.is_some(), egui::Button::new("▶ Run all"))
                .clicked()
            {
                to_run = Some((0..stress::ALL.len()).collect::<Vec<_>>());
            }
            let failed: Vec<usize> = (0..stress::ALL.len())
                .filter(|&i| self.status(i) == Status::Failed)
                .collect();
            if ui
                .add_enabled(
                    !busy && !failed.is_empty(),
                    egui::Button::new("Re-run failed"),
                )
                .clicked()
            {
                to_run = Some(failed);
            }
            if busy
                && ui.button("Stop").clicked()
                && let Some(runner) = &self.runner
            {
                runner.cancel();
            }
            let (passed, failed, total) = self.counts();
            ui.separator();
            ui.colored_label(style::PASS, format!("{passed} passed"));
            ui.colored_label(style::DROP, format!("{failed} failed"));
            ui.weak(format!("of {total}"));
            if busy {
                ui.spinner();
            }
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let mut group = "";
                for (i, test) in stress::ALL.iter().enumerate() {
                    if test.group != group {
                        group = test.group;
                        ui.add_space(4.0);
                        ui.strong(group);
                    }
                    let status = self.status(i);
                    ui.horizontal(|ui| {
                        let (icon, color) = match status {
                            Status::NotRun => ("○", style::TEXT_WEAK),
                            Status::Queued => ("…", style::TEXT_WEAK),
                            Status::Running => ("◌", style::WARN),
                            Status::Passed => ("✔", style::PASS),
                            Status::Failed => ("✘", style::DROP),
                        };
                        ui.label(RichText::new(icon).color(color).strong());
                        let open = self.expanded.contains(&i);
                        let arrow = if open { "⏷" } else { "⏵" };
                        if ui
                            .selectable_label(open, format!("{arrow} {}", test.name))
                            .on_hover_text(test.description)
                            .clicked()
                            && !self.expanded.remove(&i)
                        {
                            self.expanded.insert(i);
                        }
                        let busy = self.runner.is_some();
                        if ui
                            .add_enabled(!busy && library.is_some(), egui::Button::new("Run").small())
                            .clicked()
                        {
                            to_run = Some(vec![i]);
                        }
                        if ui
                            .add(egui::Button::new("Watch").small())
                            .on_hover_text("Load this test's network into the viewer, paused, to step through it")
                            .clicked()
                        {
                            action = Some(TestAction::Watch(i));
                        }
                        if let Some(r) = self.reports.get(i).and_then(Option::as_ref) {
                            ui.weak(format!(
                                "{} ticks · {:.0?} · peak {}/tick · {} alerts{}",
                                r.ticks,
                                r.elapsed,
                                r.peak_transmissions,
                                r.alerts,
                                if r.fuse.is_some() { " · fuse tripped" } else { "" }
                            ));
                        }
                    });
                    if self.expanded.contains(&i) {
                        ui.indent(("test-detail", i), |ui| {
                            ui.label(RichText::new(test.description).weak().italics());
                            if let Some(r) = self.reports.get(i).and_then(Option::as_ref) {
                                report_ui(ui, r);
                            }
                        });
                    }
                }
            });

        if let (Some(tests), Some(library)) = (to_run, library) {
            self.run(library, tests);
        }
        action
    }
}

fn report_ui(ui: &mut egui::Ui, r: &TestReport) {
    for line in &r.lines {
        ui.horizontal(|ui| {
            let (icon, color) = if line.passed {
                ("✔", style::PASS)
            } else {
                ("✘", style::DROP)
            };
            ui.label(RichText::new(icon).color(color));
            ui.label(RichText::new(&line.label).color(if line.passed {
                style::TEXT
            } else {
                color
            }));
            ui.weak(format!("— {}", line.detail));
        });
    }
    if let Some(e) = &r.error {
        ui.colored_label(style::DROP, format!("✘ {e}"));
    }
    if let Some(f) = &r.fuse {
        ui.colored_label(Color32::from_gray(170), format!("fuse: {f}"));
    }
}
