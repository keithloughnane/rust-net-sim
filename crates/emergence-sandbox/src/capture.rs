//! Headless-ish capture for automated checks: pick a scenario and level from the environment,
//! screenshot the window once the layout has settled, save it as PNG, and quit.
//!
//! ```sh
//! EMERGENCE_SCENARIO=Office EMERGENCE_OPEN=pc-manager \
//!     EMERGENCE_SCREENSHOT=/tmp/shot.png cargo sandbox
//! ```

use std::path::PathBuf;

use eframe::egui;

/// Frames to wait before capturing, so the layout has time to settle. Override with
/// `EMERGENCE_SCREENSHOT_FRAMES`.
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
    /// Simulation speed in ticks per second.
    pub(crate) speed: Option<f32>,
    /// Name of a stress test to load into the viewer instead of a scenario.
    pub(crate) watch: Option<String>,
    /// Start playing, even when watching a test (which normally starts paused).
    pub(crate) play: bool,
    /// Bottom tab to show: `traffic`, `alerts` or `tests`.
    pub(crate) tab: Option<String>,
    /// Run the whole stress-test checklist at startup.
    pub(crate) run_tests: bool,
    /// Frames to wait before taking the screenshot.
    pub(crate) frames: Option<u32>,
    /// Start with the "Add node" window open.
    pub(crate) add_window: bool,
}

impl Options {
    pub(crate) fn from_env() -> Self {
        Self {
            scenario: std::env::var("EMERGENCE_SCENARIO").ok(),
            open: std::env::var("EMERGENCE_OPEN").ok(),
            screenshot: std::env::var_os("EMERGENCE_SCREENSHOT").map(PathBuf::from),
            speed: std::env::var("EMERGENCE_SPEED")
                .ok()
                .and_then(|s| s.parse().ok()),
            watch: std::env::var("EMERGENCE_WATCH").ok(),
            play: std::env::var_os("EMERGENCE_PLAY").is_some(),
            tab: std::env::var("EMERGENCE_TAB").ok(),
            run_tests: std::env::var_os("EMERGENCE_RUN_TESTS").is_some(),
            add_window: std::env::var_os("EMERGENCE_ADD_WINDOW").is_some(),
            frames: std::env::var("EMERGENCE_SCREENSHOT_FRAMES")
                .ok()
                .and_then(|s| s.parse().ok()),
        }
    }
}

/// Drives one screenshot-and-quit.
#[derive(Debug)]
pub(crate) struct Capture {
    path: PathBuf,
    frames: u32,
    wait: u32,
    requested: bool,
}

impl Capture {
    pub(crate) fn new(path: PathBuf, wait: Option<u32>) -> Self {
        Self {
            path,
            frames: 0,
            wait: wait.unwrap_or(SETTLE_FRAMES),
            requested: false,
        }
    }

    /// Call once per frame.
    pub(crate) fn update(&mut self, ctx: &egui::Context) {
        self.frames += 1;
        if !self.requested && self.frames >= self.wait {
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
