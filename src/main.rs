mod scanner;
mod app;

use app::FolderCompareApp;
use eframe::egui;

fn main() -> eframe::Result<()> {
    // Load icon
    let icon_bytes = include_bytes!("../AppIcon.png");
    let icon = load_icon(icon_bytes);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 700.0])
            .with_title("Folder Compare Pro")
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };
    
    eframe::run_native(
        "Folder Compare",
        options,
        Box::new(|cc| Box::new(FolderCompareApp::new(cc))),
    )
}

fn load_icon(bytes: &[u8]) -> egui::IconData {
    let image = image::load_from_memory(bytes).expect("Failed to load icon");
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}
