//! The application object.

use crate::startup::AppInit;

/// The eframe application.
pub struct OxTailApp {
    _init: AppInit,
}

impl OxTailApp {
    /// Creates the application. Called once by eframe with the creation context.
    pub fn new(_cc: &eframe::CreationContext<'_>, init: AppInit) -> Self {
        Self { _init: init }
    }
}

impl eframe::App for OxTailApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.label("OxTail");
    }
}

/// Native window options for `renderer`, restoring `window` if known.
pub fn native_options(
    renderer: eframe::Renderer,
    window: Option<oxtail_config::WindowGeometry>,
) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("OxTail")
        .with_app_id("oxtail")
        .with_inner_size([1100.0, 700.0])
        .with_min_inner_size([480.0, 240.0])
        .with_icon(crate::icon::icon_data())
        .with_drag_and_drop(true);
    if let Some(w) = window {
        viewport = viewport
            .with_inner_size([w.width.max(480.0), w.height.max(240.0)])
            .with_position([w.x, w.y])
            .with_maximized(w.maximized);
    }
    eframe::NativeOptions {
        viewport,
        renderer,
        ..Default::default()
    }
}
