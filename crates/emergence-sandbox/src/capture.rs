//! Headless-ish capture for automated checks: pick a scenario and level from the environment,
//! screenshot the window once the layout has settled, save it as PNG, and quit.
//!
//! ```sh
//! EMERGENCE_SCENARIO=Office EMERGENCE_OPEN=pc-manager \
//!     EMERGENCE_SCREENSHOT=/tmp/shot.png cargo sandbox
//! ```

use std::path::PathBuf;

use eframe::egui;

/// Frames to wait before capturing, so the layout has time to settle.
const SETTLE_FRAMES: u32 = 150;

/// Startup options read from the environment.
#[derive(Debug, Default)]
pub(crate) struct Options {
    /// Scenario name (case-insensitive) to build first.
    pub(crate) scenario: Option<String>,
    /// Name of a node whose level to open first.
    pub(crate) open: Option<String>,
    /// Where to save a screenshot before quitting.
    pub(crate) screenshot: Option<PathBuf>,
}

impl Options {
    pub(crate) fn from_env() -> Self {
        Self {
            scenario: std::env::var("EMERGENCE_SCENARIO").ok(),
            open: std::env::var("EMERGENCE_OPEN").ok(),
            screenshot: std::env::var_os("EMERGENCE_SCREENSHOT").map(PathBuf::from),
        }
    }
}

/// Drives one screenshot-and-quit.
#[derive(Debug)]
pub(crate) struct Capture {
    path: PathBuf,
    frames: u32,
    requested: bool,
}

impl Capture {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            frames: 0,
            requested: false,
        }
    }

    /// Call once per frame.
    pub(crate) fn update(&mut self, ctx: &egui::Context) {
        self.frames += 1;
        if !self.requested && self.frames >= SETTLE_FRAMES {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.requested = true;
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let result = image::save_buffer(
                &self.path,
                image.as_raw(),
                u32::try_from(w).unwrap_or(0),
                u32::try_from(h).unwrap_or(0),
                image::ExtendedColorType::Rgba8,
            );
            match result {
                Ok(()) => println!("saved screenshot to {}", self.path.display()),
                Err(e) => eprintln!("could not save screenshot: {e}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}
