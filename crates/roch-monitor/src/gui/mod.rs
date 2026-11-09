//! The window half.
//!
//! * [`app`] — the eframe App: header, the sensor list, graphs, footer, sources window
//! * [`rows`] — the list's rows, flattened with folding and search; toolkit-free and tested
//! * [`graph`] — the graph panel
//! * [`settings_window`] — the Settings window: colours, font, sizes, layout, window, sensors
//! * [`appearance`] — the appearance choices and Roch GPU's colour presets
//! * [`theme`] — the palette, fonts and sizes built from those choices
//! * [`fonts`] — the fonts installed on the machine
//! * [`settings`] — what the window remembers
//! * [`native`] — title bars, opacity and Explorer

mod app;
mod appearance;
mod fonts;
mod graph;
mod native;
mod rows;
mod settings;
mod settings_window;
mod theme;

use std::sync::Arc;

use eframe::egui;

const ICON_PNG: &[u8] = include_bytes!("../../../../assets/icon.png");

fn window_icon() -> Arc<egui::IconData> {
    match image::load_from_memory_with_format(ICON_PNG, image::ImageFormat::Png) {
        Ok(img) => {
            let img = img.into_rgba8();
            let (width, height) = img.dimensions();
            Arc::new(egui::IconData { rgba: img.into_raw(), width, height })
        }
        Err(_) => Arc::new(egui::IconData::default()),
    }
}

pub fn run(args: &[String]) -> i32 {
    let settings = settings::Settings::load();
    let sys = roch_sensors::system::SystemInfo::detect();
    let options = crate::cli::options(args);
    // `--settings` opens the Settings window with the window; `--settings colours` at that page.
    let open_settings = args
        .iter()
        .position(|a| a == "--settings")
        .map(|i| args.get(i + 1).and_then(|name| settings_window::Page::from_name(name)));
    let size = settings.window_size.unwrap_or([1060.0, 920.0]);
    let icon = window_icon();
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Roch Monitor")
            .with_app_id("RochMonitor")
            .with_inner_size([size[0].max(780.0), size[1].max(480.0)])
            .with_min_inner_size([780.0, 480.0])
            .with_icon(icon.clone())
            .with_window_level(if settings.appearance.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }),
        renderer: eframe::Renderer::Glow,
        centered: true,
        persist_window: false,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Roch Monitor",
        native,
        Box::new(move |cc| {
            // The first frame draws with these; the app re-applies whenever Settings changes them.
            let a = &settings.appearance;
            theme::install_fonts(&cc.egui_ctx, &a.font_family);
            theme::apply(&cc.egui_ctx, a, a.theme.is_dark(appearance::system_dark()));
            cc.egui_ctx.set_zoom_factor(a.ui_scale);
            let mut app = app::MonitorApp::new(&cc.egui_ctx, sys, settings, options);
            app.set_icon(icon);
            if let Some(page) = open_settings {
                app.open_settings(&cc.egui_ctx, page);
            }
            Ok(Box::new(app))
        }),
    );
    match result {
        Ok(()) => 0,
        Err(e) => {
            message_box(&format!("Roch Monitor could not open its window:\n\n{e}"));
            1
        }
    }
}

#[cfg(windows)]
fn message_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (t, c) = (wide(text), wide("Roch Monitor"));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn message_box(text: &str) {
    eprintln!("{text}");
}
