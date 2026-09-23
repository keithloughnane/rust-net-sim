//! Interactive test harness for the Emergence Engine.
//!
//! The sandbox talks to the engine only through the compiled native library and its C ABI,
//! the same way Unity or Unreal will. Run it with `cargo sandbox`, which builds that library
//! first.

mod native;

use eframe::egui;
use native::{NativeLibrary, NativeWorld};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Emergence Sandbox")
            .with_inner_size([520.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Emergence Sandbox",
        options,
        Box::new(|_cc| Ok(Box::new(SandboxApp::new()))),
    )
}

/// The world and any error from the last native call.
#[derive(Debug)]
struct Session {
    world: NativeWorld,
    last_error: Option<String>,
}

#[derive(Debug)]
struct SandboxApp {
    library: Result<std::sync::Arc<NativeLibrary>, String>,
    session: Option<Session>,
}

impl SandboxApp {
    fn new() -> Self {
        let path = NativeLibrary::default_path();
        let library = NativeLibrary::load(&path).map_err(|e| e.to_string());
        let mut app = Self {
            library,
            session: None,
        };
        app.reset_world();
        app
    }

    fn reset_world(&mut self) {
        let Ok(library) = &self.library else { return };
        self.session = Some(match NativeWorld::new(library.clone()) {
            Ok(world) => Session {
                world,
                last_error: None,
            },
            Err(e) => {
                self.library = Err(format!("could not create world: {e}"));
                return;
            }
        });
    }
}

impl eframe::App for SandboxApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Emergence Sandbox");

            let library = match &self.library {
                Ok(library) => library,
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                    ui.label(
                        "Build the native library first: run the sandbox with `cargo sandbox`.",
                    );
                    return;
                }
            };
            ui.label(format!("Native library v{}", library.version()));
            ui.small(library.path().display().to_string());
            ui.separator();

            let mut reset = false;
            if let Some(session) = &mut self.session {
                ui.horizontal(|ui| {
                    if ui.button("Tick").clicked()
                        && let Err(e) = session.world.tick()
                    {
                        session.last_error = Some(e.to_string());
                    }
                    reset = ui.button("New world").clicked();
                });
                match session.world.tick_count() {
                    Ok(count) => ui.label(format!("Tick count: {count}")),
                    Err(e) => ui.colored_label(ui.visuals().error_fg_color, e.to_string()),
                };
                if let Some(e) = &session.last_error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            }
            if reset {
                self.reset_world();
            }
        });
    }
}
