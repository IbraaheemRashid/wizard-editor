fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 600.0])
            .with_title("Wizard Editor"),
        vsync: true,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "Wizard Editor",
        options,
        Box::new(|cc| Ok(Box::new(wizard_app::EditorApp::new(cc)))),
    )
}
