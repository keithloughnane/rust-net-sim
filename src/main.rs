use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SmitherNet Sandbox")
            .with_inner_size([480.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "SmitherNet Sandbox",
        options,
        Box::new(|_cc| Ok(Box::<SandboxApp>::default())),
    )
}

#[derive(Default)]
struct SandboxApp {
    clicks: u32,
}

impl eframe::App for SandboxApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Hello, world!");
            if ui.button("Click me").clicked() {
                self.clicks += 1;
            }
            ui.label(format!("Clicked {} times", self.clicks));
        });
    }
}
