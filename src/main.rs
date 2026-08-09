#![forbid(unsafe_code)]

use ftnl_desktop::desktop::DesktopApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([920.0, 680.0])
            .with_min_inner_size([720.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "File Tunnel",
        options,
        Box::new(|context| Ok(Box::new(DesktopApp::new(context)))),
    )
}
