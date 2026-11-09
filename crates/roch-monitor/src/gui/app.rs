//! The window: Roch GPU's telemetry table, grown into a whole-machine sensor list.
//!
//! Header (machine, toolbar), the list (device headings, group headings, sensors with current /
//! minimum / maximum / average), an optional graph panel, and the footer with the run timer and
//! Reset Stats, as both Roch tools' telemetry windows end.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, Context, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, Ui,
    pos2, vec2,
};
use roch_sensors::engine::{Engine, MIN_INTERVAL_MS, State};
use roch_sensors::hw::Driver;
use roch_sensors::model::{DeviceKind, format_value};
use roch_sensors::sources::{Options, factories};
use roch_sensors::system::SystemInfo;

use super::appearance::{self, Appearance};
use super::graph::{self, Series};
use super::rows::{self, Row};
use super::settings::Settings;
use super::settings_window::{self, GRAPH_WINDOWS, INTERVALS, SettingsWindow};
use super::theme::{self, Palette, body, pal, row};
use super::tray::{Tray, TrayCommand, TrayValue, icon_digits};
use crate::logger::{self, CsvLogger};
use crate::report;

/// Points kept per sensor: an hour at the default one-second poll.
const HISTORY: usize = 3600;
const MIN_NAME_COL: f32 = 220.0;
const MAX_GRAPHS: usize = 8;
/// How often "Follow Windows" looks at what Windows is set to.
const SYSTEM_THEME_CHECK: Duration = Duration::from_secs(2);
/// Settings changed in the Settings window are written at most this often while it is being used.
const SAVE_DELAY: Duration = Duration::from_millis(800);

enum Action {
    ToggleDevice(String),
    ToggleExpand(String),
    Graph(String, bool),
    Hide(String),
    Unhide(String),
    ResetSensor(String),
    Copy(String),
    CollapseAll,
    ExpandAll,
    Tray(String),
}

pub struct MonitorApp {
    sys: SystemInfo,
    engine: Engine,
    options: Options,
    settings: Settings,
    search: String,
    show_hidden: bool,
    show_sources: bool,
    logger: Option<CsvLogger>,
    notice: Option<(String, Instant)>,
    hardware_note: Option<String>,
    /// Whether PawnIO opened when the window started, for the Sources window.
    pawnio: Result<(), String>,
    search_id: egui::Id,
    /// False under test, so a test never writes the user's settings file.
    persist: bool,
    settings_window: SettingsWindow,
    /// What was last applied to the window, so a change is applied once rather than every frame.
    applied: Option<(Appearance, bool)>,
    applied_font: Option<String>,
    applied_level: Option<bool>,
    applied_opacity: Option<f32>,
    /// How many windows the title-bar colour was last set on, and for which mode.
    title_bars: Option<(bool, usize)>,
    system_dark: (bool, Option<Instant>),
    unsaved_since: Option<Instant>,
    icon: Option<std::sync::Arc<egui::IconData>>,
    tray: Option<Tray>,
    /// The tray icon is made after the first frame (a few ms of shell calls the window need not
    /// wait for), or at once when starting in the tray.
    tray_due: bool,
    hidden: bool,
    start_hidden: bool,
    /// Set by the tray's Exit, so a close-to-tray setting does not swallow it.
    really_close: bool,
    tray_revision: u64,
}

fn start_engine(ctx: &Context, sys: &SystemInfo, options: Options, settings: &Settings) -> Engine {
    let options = Options { pmic_adc: settings.pmic_adc, ..options };
    let engine = Engine::start(factories(sys, options), settings.interval_ms, HISTORY);
    let ctx = ctx.clone();
    // Ten sources finishing within a few milliseconds of each other become one frame, not ten.
    engine.on_update(move || ctx.request_repaint_after(Duration::from_millis(30)));
    engine
}

/// Why the driver-based sensors are missing, when they are. PawnIO answering is enough; inpoutx64
/// is only asked when PawnIO is not there, so a machine with PawnIO never loads it.
fn driver_note(sys: &SystemInfo, options: &Options, pawnio: &Result<(), String>) -> Option<String> {
    if !options.hardware {
        return Some(
            "Started with --no-hardware: per-core, board, DIMM and memory-controller sensors are off.".to_owned(),
        );
    }
    if options.driver.pawnio() && pawnio.is_ok() {
        return None;
    }
    if options.driver.inpout() && roch_sensors::hw::inpout::driver().is_ok() {
        return None;
    }
    Some(if !sys.elevated {
        "Not running as administrator, so no driver can open: per-core, board, DIMM and memory-controller sensors are missing.".to_owned()
    } else {
        "No driver for the per-core, board, DIMM and memory-controller sensors: install PawnIO from pawnio.eu. Sources says what each source found.".to_owned()
    })
}

fn pawnio_state(options: &Options) -> Result<(), String> {
    if options.hardware && options.driver.pawnio() {
        roch_sensors::hw::pawnio::available()
    } else {
        Err("not used".into())
    }
}

impl MonitorApp {
    pub fn new(ctx: &Context, sys: SystemInfo, settings: Settings, options: Options) -> MonitorApp {
        let hardware_note = driver_note(&sys, &options, &pawnio_state(&options));
        let engine = start_engine(ctx, &sys, options, &settings);
        Self::with_engine(sys, settings, options, engine, hardware_note, true)
    }

    pub(crate) fn with_engine(
        sys: SystemInfo,
        settings: Settings,
        options: Options,
        engine: Engine,
        hardware_note: Option<String>,
        persist: bool,
    ) -> MonitorApp {
        MonitorApp {
            sys,
            engine,
            options,
            settings,
            search: String::new(),
            show_hidden: false,
            show_sources: false,
            logger: None,
            notice: None,
            hardware_note,
            pawnio: pawnio_state(&options),
            search_id: egui::Id::unique("roch_monitor_search"),
            persist,
            settings_window: SettingsWindow::default(),
            applied: None,
            applied_font: None,
            applied_level: None,
            applied_opacity: None,
            title_bars: None,
            system_dark: (true, None),
            unsaved_since: None,
            icon: None,
            tray: None,
            tray_due: persist,
            hidden: false,
            start_hidden: false,
            really_close: false,
            tray_revision: u64::MAX,
        }
    }

    /// Open the Settings window (the `--settings [page]` flag, and the toolbar's button).
    pub(crate) fn open_settings(&mut self, ctx: &Context, page: Option<settings_window::Page>) {
        self.settings_window.open(ctx);
        if let Some(page) = page {
            self.settings_window.page = page;
        }
    }

    /// `--minimized`: the window starts hidden, and the tray icon is made at once to bring it back.
    pub(crate) fn start_in_tray(&mut self) {
        self.start_hidden = true;
        self.hidden = true;
    }

    // -- the tray -----------------------------------------------------------------------------

    /// Everything the tray needs each frame: make it, act on its menu, hide on minimise or close,
    /// and give it the latest readings.
    fn tray_tick(&mut self, ctx: &Context) {
        if self.tray_due {
            self.tray_due = false;
            self.tray = Tray::new(ctx);
            if self.tray.is_none() && self.start_hidden {
                // Nowhere to hide: show the window rather than run invisibly with no way back.
                self.show_from_tray(ctx);
            }
        }
        let commands: Vec<TrayCommand> =
            self.tray.as_ref().map(|t| t.commands.try_iter().collect()).unwrap_or_default();
        for cmd in commands {
            match cmd {
                TrayCommand::Show => self.show_from_tray(ctx),
                TrayCommand::Settings => {
                    self.show_from_tray(ctx);
                    self.settings_window.open(ctx);
                }
                TrayCommand::ResetStats => self.reset_stats(),
                TrayCommand::Exit => {
                    self.really_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if self.tray.is_some() && !self.hidden {
            let (minimized, close) =
                ctx.input(|i| (i.viewport().minimized == Some(true), i.viewport().close_requested()));
            if minimized && self.settings.minimize_to_tray {
                self.hide_to_tray(ctx);
            } else if close && self.settings.close_to_tray && !self.really_close {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hide_to_tray(ctx);
            }
        }
        self.update_tray();
    }

    fn hide_to_tray(&mut self, ctx: &Context) {
        self.hidden = true;
        self.settings_window.open = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn show_from_tray(&mut self, ctx: &Context) {
        if self.hidden {
            self.hidden = false;
            self.start_hidden = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Readings for the tooltip when none have been put in the tray: the temperatures and the power
    /// a glance at the tray is usually for.
    const TRAY_DEFAULTS: [&'static str; 5] =
        ["cpu/temp/package", "cpu/temp/coremax", "cpu/power/package", "gpu0/temp", "gpu0/power/board"];

    /// The Roch icon's tooltip and the value icons, from the latest readings.
    pub(crate) fn tray_contents(&self) -> (String, Vec<TrayValue>) {
        let st = self.engine.lock();
        let line = |key: &str| -> Option<(String, String, String)> {
            let r = st.sensor(key)?;
            let device = st.device(&r.info.device).map(short_device).unwrap_or_default();
            let value = format_value(r.stat.current, &r.info.unit, r.info.decimals);
            // "GPU Temperature" already says whose it is; "Core Max" needs "CPU" in front.
            let name = if r.info.label.starts_with(&device) {
                r.info.label.clone()
            } else {
                format!("{device} {}", r.info.label)
            };
            Some((format!("{name}: {value}"), icon_digits(r.stat.current, &r.info.unit), value))
        };
        let chosen: Vec<&str> = if self.settings.tray_sensors.is_empty() {
            Self::TRAY_DEFAULTS.iter().copied().filter(|k| st.sensor(k).is_some()).take(4).collect()
        } else {
            self.settings.tray_sensors.iter().map(String::as_str).collect()
        };
        let mut tooltip = String::from("Roch Monitor");
        for key in &chosen {
            if let Some((text, _, _)) = line(key) {
                tooltip.push('\n');
                tooltip.push_str(&text);
            }
        }
        let values = if self.settings.tray_icons {
            self.settings
                .tray_sensors
                .iter()
                .filter_map(|key| {
                    line(key).map(|(text, digits, _)| TrayValue { key: key.clone(), digits, tooltip: text })
                })
                .collect()
        } else {
            Vec::new()
        };
        (tooltip, values)
    }

    /// Push the latest readings to the tray, once per poll rather than once per frame.
    fn update_tray(&mut self) {
        if self.tray.is_none() {
            return;
        }
        let revision = self.engine.lock().value_revision;
        if revision == self.tray_revision {
            return;
        }
        self.tray_revision = revision;
        let (tooltip, values) = self.tray_contents();
        let dark = self.settings.appearance.dark;
        if let Some(tray) = &mut self.tray {
            tray.set_tooltip(&tooltip);
            tray.sync_values(&values, dark.value, dark.background);
        }
    }

    pub(crate) fn set_icon(&mut self, icon: std::sync::Arc<egui::IconData>) {
        self.icon = Some(icon);
    }

    /// Whether the window is dark now: the theme setting, or Windows' own for Follow Windows.
    fn is_dark(&mut self) -> bool {
        let theme = self.settings.appearance.theme;
        if theme == appearance::ThemeMode::System
            && self.system_dark.1.is_none_or(|at| at.elapsed() >= SYSTEM_THEME_CHECK)
        {
            self.system_dark = (appearance::system_dark(), Some(Instant::now()));
        }
        theme.is_dark(self.system_dark.0)
    }

    /// Apply whatever in the appearance has changed since the last frame: palette and sizes, the
    /// font, zoom, window level, opacity and title bars. Each is applied once per change.
    pub(crate) fn apply_appearance(&mut self, ctx: &Context) {
        let dark = self.is_dark();
        let a = &self.settings.appearance;
        if self.applied_font.as_deref() != Some(a.font_family.as_str()) {
            theme::install_fonts(ctx, &a.font_family);
            self.applied_font = Some(a.font_family.clone());
        }
        if self.applied.as_ref().is_none_or(|(prev, prev_dark)| prev != a || *prev_dark != dark) {
            theme::apply(ctx, a, dark);
            self.applied = Some((a.clone(), dark));
        }
        if (ctx.zoom_factor() - a.ui_scale).abs() > f32::EPSILON {
            ctx.set_zoom_factor(a.ui_scale);
        }
        if self.applied_level != Some(a.always_on_top) {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if a.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }));
            self.applied_level = Some(a.always_on_top);
        }
        if self.persist && self.applied_opacity != Some(a.opacity) && super::native::apply_opacity(a.opacity) {
            self.applied_opacity = Some(a.opacity);
        }
        // Again whenever the number of windows changes, so the Settings window gets it too.
        if self.persist {
            let expected = 1 + self.settings_window.open as usize;
            if self.title_bars != Some((dark, expected)) {
                let set = super::native::set_title_bars_dark(dark);
                if set >= expected {
                    self.title_bars = Some((dark, expected));
                }
            }
        }
    }

    /// Note that settings changed; they are written once the changes stop for a moment.
    fn changed(&mut self) {
        self.unsaved_since.get_or_insert_with(Instant::now);
    }

    fn save_if_settled(&mut self) {
        if self.unsaved_since.is_some_and(|t| t.elapsed() >= SAVE_DELAY) {
            self.save_settings();
            self.unsaved_since = None;
        }
    }

    fn save_settings(&self) {
        if self.persist {
            self.settings.save();
        }
    }

    fn say(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    fn restart_engine(&mut self, ctx: &Context) {
        if let Some(mut l) = self.logger.take() {
            l.stop();
        }
        self.engine.stop();
        self.engine = start_engine(ctx, &self.sys, self.options, &self.settings);
    }

    fn reset_stats(&mut self) {
        self.engine.lock().reset_stats();
    }

    // -- header -------------------------------------------------------------------------------

    fn header(&mut self, ui: &mut Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new("ROCH MONITOR").font(theme::bold(16.0)).color(p.text));
            ui.label(
                RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .font(theme::regular(body() - 1.0))
                    .color(p.muted),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, fill, color) = if self.hardware_note.is_none() {
                    ("LIVE · READ ONLY", p.header, p.text)
                } else {
                    ("LIMITED · NO DRIVER", p.header, p.warn)
                };
                let galley = ui.painter().layout_no_wrap(text.into(), theme::bold(body()), color);
                let (r, _) = ui.allocate_exact_size(galley.size() + vec2(20.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, theme::radius(5), fill);
                ui.painter().galley(r.min + vec2(10.0, 5.0), galley, color);
                ui.add_space(8.0);
                // The quick switch: one click between light and dark, leaving Follow Windows for
                // the Settings window.
                let dark = pal(&ctx).dark;
                let theme_label = if dark { "☀ Light" } else { "☾ Dark" };
                if ui.button(theme_label).on_hover_text("Switch between light and dark").clicked() {
                    self.settings.appearance.theme =
                        if dark { appearance::ThemeMode::Light } else { appearance::ThemeMode::Dark };
                    self.changed();
                }
                let settings_button = egui::Button::new(RichText::new("⚙ Settings").font(theme::bold(body())));
                let response = ui.add(settings_button).on_hover_text(
                    "Colours, font, text size, layout and window: everything about how Roch Monitor looks",
                );
                #[cfg(test)]
                tests::record("button:settings", response.rect);
                if response.clicked() {
                    self.settings_window.open(&ctx);
                }
            });
        });
        if theme::look().show_summary {
            let mut parts = vec![self.sys.cpu_name(), self.sys.board_name()];
            let mem = self.sys.memory_summary();
            if !mem.is_empty() {
                parts.push(mem);
            }
            {
                let st = self.engine.lock();
                for d in st.ordered_devices().into_iter().filter(|d| d.kind == DeviceKind::Gpu) {
                    parts.push(d.name.clone());
                }
            }
            ui.label(RichText::new(parts.join("  ·  ")).font(theme::regular(body())).color(p.muted));
        }
        ui.add_space(6.0);

        // Wraps onto a second line in a narrow window rather than running under its own buttons.
        ui.horizontal_wrapped(|ui| {
            let search = egui::TextEdit::singleline(&mut self.search)
                .id(self.search_id)
                .hint_text("Search sensors  (Ctrl+F)")
                .desired_width(230.0);
            ui.add(search);
            if !self.search.is_empty() && ui.small_button("×").on_hover_text("Clear the search").clicked() {
                self.search.clear();
            }
            ui.add_space(6.0);

            ui.label(RichText::new("Poll").color(p.muted));
            let current =
                INTERVALS.iter().find(|(ms, _)| *ms == self.settings.interval_ms).map_or("custom", |(_, l)| l);
            let mut chosen = self.settings.interval_ms;
            egui::ComboBox::from_id_salt("interval").selected_text(current).width(78.0).show_ui(ui, |ui| {
                for (ms, label) in INTERVALS {
                    ui.selectable_value(&mut chosen, ms, label);
                }
            });
            if chosen != self.settings.interval_ms {
                self.settings.interval_ms = chosen.max(MIN_INTERVAL_MS);
                self.engine.set_interval(self.settings.interval_ms);
                self.save_settings();
            }
            ui.add_space(6.0);

            if ui.button("Expand all").clicked() {
                self.apply(&ctx, Action::ExpandAll);
            }
            if ui.button("Collapse all").clicked() {
                self.apply(&ctx, Action::CollapseAll);
            }
            ui.add_space(6.0);

            let log_label = match &self.logger {
                Some(l) => format!("■ Stop log ({})", l.rows()),
                None => "● Log CSV".to_owned(),
            };
            let log_button = egui::Button::new(RichText::new(log_label).color(if self.logger.is_some() {
                p.accent
            } else {
                p.text
            }));
            if ui
                .add(log_button)
                .on_hover_text("Write every visible sensor to a CSV file once per poll, as HWiNFO logs")
                .clicked()
            {
                self.toggle_log();
            }
            if ui
                .button("Save report")
                .on_hover_text("Write every sensor's current, minimum, maximum and average to a text file")
                .clicked()
            {
                self.save_report();
            }
            if ui.button("Sources").on_hover_text("What each transport found, and how long it takes to read").clicked()
            {
                self.show_sources = !self.show_sources;
            }
        });

        if let Some(note) = &self.hardware_note {
            ui.add_space(4.0);
            Frame::new()
                .fill(p.header)
                .corner_radius(CornerRadius::same(4))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(note).color(p.warn));
                });
        }
        if let Some((text, at)) = &self.notice {
            if at.elapsed() < Duration::from_secs(8) {
                ui.add_space(4.0);
                ui.label(RichText::new(text).color(p.ok));
                ctx.request_repaint_after(Duration::from_secs(1));
            } else {
                self.notice = None;
            }
        }
        ui.add_space(4.0);
    }

    // -- the list -----------------------------------------------------------------------------

    fn columns(r: Rect) -> [Rect; 5] {
        let name = (r.width() - 4.0 * theme::look().value_width).max(MIN_NAME_COL);
        let value = ((r.width() - name) / 4.0).max(60.0);
        let mut out = [r; 5];
        out[0] = Rect::from_min_max(r.min, pos2(r.left() + name, r.bottom()));
        for (i, c) in out.iter_mut().enumerate().skip(1) {
            let x0 = r.left() + name + value * (i - 1) as f32;
            *c = Rect::from_min_max(pos2(x0, r.top()), pos2(x0 + value, r.bottom()));
        }
        out
    }

    fn table(&mut self, ui: &mut Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        // The fixed column heading; its right edge leaves room for the scrollbar below it.
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), row()), Sense::hover());
        let cols = Self::columns(Rect::from_min_max(head.min, pos2(head.right() - 14.0, head.bottom())));
        let painter = ui.painter();
        painter.text(
            pos2(cols[0].left() + 8.0, cols[0].center().y),
            Align2::LEFT_CENTER,
            "Sensor",
            theme::bold(body()),
            p.text,
        );
        for (c, t) in cols[1..].iter().zip(["Current", "Minimum", "Maximum", "Average"]) {
            painter.text(pos2(c.right() - 8.0, c.center().y), Align2::RIGHT_CENTER, t, theme::bold(body()), p.text);
        }
        painter.hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, p.border));

        let list = {
            let st = self.engine.lock();
            rows::build(
                &st,
                &rows::View {
                    search: &self.search,
                    hidden: &self.settings.hidden,
                    show_hidden: self.show_hidden,
                    collapsed: &self.settings.collapsed,
                    expanded: &self.settings.expanded,
                    group_headings: self.settings.appearance.group_headings,
                },
            )
        };
        if list.is_empty() {
            ui.add_space(30.0);
            let text = if self.search.is_empty() {
                "Waiting for the first readings…"
            } else {
                "No sensor matches the search."
            };
            ui.vertical_centered(|ui| ui.label(RichText::new(text).color(p.muted)));
            return;
        }

        let graphs = self.settings.graphs.clone();
        let hidden = self.settings.hidden.clone();
        let in_tray = self.settings.tray_sensors.clone();
        let stale_after = (self.engine.interval() * 3).max(Duration::from_secs(5));
        let mut actions = Vec::new();
        ui.scope(|ui| {
            style_scrollbar(ui, p);
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row(), list.len(), |ui, range| {
                let st = self.engine.lock();
                // Bands count only the sensor rows of one device, so they alternate whatever is open.
                let mut band = band_index_before(&list, range.start);
                for row in &list[range] {
                    if matches!(row, Row::Device { .. }) {
                        band = 0;
                    }
                    draw_row(ui, &st, row, &mut band, p, &graphs, &hidden, &in_tray, stale_after, &mut actions);
                }
            });
        });
        for a in actions {
            self.apply(&ctx, a);
        }
    }

    fn apply(&mut self, ctx: &Context, action: Action) {
        let s = &mut self.settings;
        match action {
            Action::ToggleDevice(id) => Settings::toggle(&mut s.collapsed, &id),
            Action::ToggleExpand(key) => Settings::toggle(&mut s.expanded, &key),
            Action::Graph(key, add) => {
                if s.graphs.contains(&key) {
                    s.graphs.retain(|k| *k != key);
                } else {
                    if !add {
                        s.graphs.clear();
                    }
                    if s.graphs.len() >= MAX_GRAPHS {
                        s.graphs.remove(0);
                    }
                    s.graphs.push(key);
                }
            }
            Action::Hide(key) => {
                if !s.hidden.contains(&key) {
                    s.hidden.push(key.clone());
                }
                s.graphs.retain(|k| *k != key);
            }
            Action::Unhide(key) => s.hidden.retain(|k| *k != key),
            Action::ResetSensor(key) => self.engine.lock().reset_sensor(&key),
            Action::Copy(text) => ctx.copy_text(text),
            Action::CollapseAll => {
                let st = self.engine.lock();
                s.collapsed = st.devices.iter().map(|d| d.id.clone()).collect();
            }
            Action::Tray(key) => {
                if s.tray_sensors.contains(&key) {
                    s.tray_sensors.retain(|k| *k != key);
                } else if s.tray_sensors.len() < super::settings::MAX_TRAY_SENSORS {
                    s.tray_sensors.push(key);
                }
                self.tray_revision = u64::MAX;
            }
            Action::ExpandAll => {
                s.collapsed.clear();
                let st = self.engine.lock();
                let parents: Vec<String> = st.sensors.iter().filter_map(|r| r.info.parent.clone()).collect();
                for p in parents {
                    if !s.expanded.contains(&p) {
                        s.expanded.push(p);
                    }
                }
            }
        }
        self.save_settings();
    }

    // -- graphs -------------------------------------------------------------------------------

    fn graphs(&mut self, ui: &mut Ui, p: &Palette) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("GRAPHS").font(theme::bold(body())).color(p.text));
            ui.add_space(8.0);
            let mut window = self.settings.graph_seconds;
            let current = GRAPH_WINDOWS.iter().find(|(s, _)| *s == window).map_or("5 min", |(_, l)| l);
            egui::ComboBox::from_id_salt("graph-window").selected_text(current).width(70.0).show_ui(ui, |ui| {
                for (s, label) in GRAPH_WINDOWS {
                    ui.selectable_value(&mut window, s, label);
                }
            });
            if window != self.settings.graph_seconds {
                self.settings.graph_seconds = window;
                self.save_settings();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Clear graphs").clicked() {
                    self.settings.graphs.clear();
                    self.save_settings();
                }
                ui.label(RichText::new("Click a sensor to graph it · Ctrl+click to add").color(p.muted));
            });
        });
        ui.add_space(4.0);
        let (series, now) = {
            let st = self.engine.lock();
            let now = self.engine.shared().epoch().elapsed().as_secs_f32();
            let series: Vec<Series> = self
                .settings
                .graphs
                .iter()
                .enumerate()
                .filter_map(|(i, key)| {
                    let row = st.sensor(key)?;
                    let short = st.device(&row.info.device).map(short_device).unwrap_or_default();
                    Some(Series {
                        key: key.clone(),
                        label: format!("{} · {}", row.info.label, short),
                        unit: row.info.unit.clone(),
                        decimals: row.info.decimals,
                        color: p.series(i),
                        points: row.history.points().copied().collect(),
                        current: row.stat.current,
                    })
                })
                .collect();
            (series, now)
        };
        let removed = graph::draw(ui, &series, now, self.settings.graph_seconds as f32, p);
        if !removed.is_empty() {
            self.settings.graphs.retain(|k| !removed.contains(k));
            self.save_settings();
        }
    }

    // -- footer -------------------------------------------------------------------------------

    fn footer(&mut self, ui: &mut Ui, p: &Palette) {
        let (elapsed, sensors, slowest) = {
            let st = self.engine.lock();
            let slowest = st.sources.iter().filter(|s| s.polls > 0).map(|s| s.last_poll).max().unwrap_or_default();
            (st.stats_since.elapsed().as_secs(), st.sensors.len(), slowest)
        };
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(format!("Running: {}", report::hms(elapsed))).color(p.text));
            ui.label(
                RichText::new(format!(
                    "·  {sensors} sensors  ·  poll {}  ·  slowest source {:.1} ms",
                    INTERVALS.iter().find(|(ms, _)| *ms == self.settings.interval_ms).map_or("?", |(_, l)| l),
                    slowest.as_secs_f64() * 1000.0
                ))
                .color(p.muted),
            );
            if let Some(l) = &self.logger {
                let name = l.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let link = ui.add(
                    egui::Label::new(
                        RichText::new(format!("·  ● logging {} rows to {name}", l.rows())).color(p.accent),
                    )
                    .sense(Sense::click()),
                );
                if link.on_hover_text("Show the log file in Explorer").clicked() {
                    super::native::reveal(l.path());
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .button(RichText::new("Reset Stats").font(theme::bold(body())))
                    .on_hover_text(
                        "Clear every minimum, maximum, average and graph (Ctrl+R). Nothing on the hardware is changed.",
                    )
                    .clicked()
                {
                    self.reset_stats();
                }
            });
        });
        // The timer moves once a second even when nothing else does.
        ui.ctx().request_repaint_after(Duration::from_millis(1000));
    }

    // -- sources window -----------------------------------------------------------------------

    fn sources_window(&mut self, ctx: &Context, p: &Palette) {
        if !self.show_sources {
            return;
        }
        let mut open = true;
        egui::Window::new("Sources").open(&mut open).default_width(760.0).resizable(true).show(ctx, |ui| {
            let pawnio = match (&self.pawnio, self.options.hardware, self.options.driver) {
                (_, false, _) => "PawnIO: not used (--no-hardware)".to_owned(),
                (_, true, Driver::InpOut) => "PawnIO: not used (--driver inpoutx64)".to_owned(),
                (Ok(()), true, _) => "PawnIO: installed, open".to_owned(),
                (Err(e), true, _) => format!("PawnIO: {e}"),
            };
            // Asked without loading it: inpoutx64 is only loaded by a source PawnIO could not serve.
            let inpout = match roch_sensors::hw::inpout::loaded() {
                Some(Ok(d)) => format!("inpoutx64: loaded from {}", d.path().display()),
                Some(Err(e)) => format!("inpoutx64: {e}"),
                None => "inpoutx64: not loaded".to_owned(),
            };
            ui.label(RichText::new(pawnio).color(p.muted));
            ui.label(RichText::new(inpout).color(p.muted));
            ui.label(
                RichText::new(format!("Administrator: {}", if self.sys.elevated { "yes" } else { "no" }))
                    .color(p.muted),
            );
            ui.separator();
            let st = self.engine.lock();
            egui::Grid::new("sources-grid").striped(true).num_columns(3).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                ui.label(RichText::new("Source").font(theme::bold(body())));
                ui.label(RichText::new("Read time").font(theme::bold(body())));
                ui.label(RichText::new("Status").font(theme::bold(body())));
                ui.end_row();
                for s in &st.sources {
                    ui.label(&s.name);
                    ui.label(if s.polls > 0 {
                        format!("{:.2} ms", s.last_poll.as_secs_f64() * 1000.0)
                    } else {
                        "—".into()
                    });
                    let color = if s.failed { p.muted } else { p.text };
                    ui.add(egui::Label::new(RichText::new(&s.status).color(color)).wrap());
                    ui.end_row();
                }
            });
        });
        if !open {
            self.show_sources = false;
        }
    }

    // -- logging and reports ------------------------------------------------------------------

    fn output_directory(&self) -> PathBuf {
        self.settings.log_directory.as_ref().map(PathBuf::from).unwrap_or_else(logger::default_directory)
    }

    fn toggle_log(&mut self) {
        if let Some(mut l) = self.logger.take() {
            l.stop();
            let msg = format!("Stopped logging: {} rows in {}", l.rows(), l.path().display());
            self.say(msg);
            return;
        }
        let path = self.output_directory().join(logger::timestamped_name());
        match CsvLogger::start(self.engine.shared().clone(), &path, &self.settings.hidden) {
            Ok(l) => {
                self.logger = Some(l);
                self.say(format!("Logging to {}", path.display()));
            }
            Err(e) => self.say(format!("Could not start the log at {}: {e}", path.display())),
        }
    }

    fn save_report(&mut self) {
        let text = {
            let st = self.engine.lock();
            report::text(&st, &self.sys)
        };
        let dir = self.output_directory();
        let path = dir.join(format!("RochMonitor_Report_{}.txt", chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")));
        let written = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text.as_bytes()));
        match written {
            Ok(()) => {
                super::native::reveal(&path);
                self.say(format!("Saved the report to {}", path.display()));
            }
            Err(e) => self.say(format!("Could not save the report: {e}")),
        }
    }

    /// One frame of the whole window. Split from `App::ui` so a test can drive it with synthetic
    /// input through `Context::run_ui`, which needs no eframe frame.
    pub(crate) fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let p = pal(&ctx);
        egui::Panel::top("header")
            .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 14, right: 14, top: 10, bottom: 4 }))
            .show(ui, |ui| self.header(ui, &p));
        egui::Panel::bottom("footer")
            .frame(
                Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin { left: 14, right: 14, top: 4, bottom: 6 })
                    .stroke(Stroke::new(1.0, p.border)),
            )
            .exact_size(36.0)
            .show(ui, |ui| self.footer(ui, &p));
        if !self.settings.graphs.is_empty() {
            let panel = egui::Panel::bottom("graphs")
                .resizable(true)
                .default_size(self.settings.graph_height)
                .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 14, right: 14, top: 8, bottom: 6 }))
                .show(ui, |ui| self.graphs(ui, &p));
            self.settings.graph_height = panel.response.rect.height();
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 14, right: 14, top: 2, bottom: 2 }))
            .show(ui, |ui| self.table(ui, &p));
        self.sources_window(&ctx, &p);
        self.settings_window(&ctx);
    }

    /// The Settings window, when open. Its changes are live: the next frame draws with them.
    fn settings_window(&mut self, ctx: &Context) {
        if !self.settings_window.open {
            return;
        }
        let before = self.settings.clone();
        let (sensors, sensor_names) = {
            let st = self.engine.lock();
            let names = self
                .settings
                .tray_sensors
                .iter()
                .filter_map(|key| {
                    let r = st.sensor(key)?;
                    let device = st.device(&r.info.device).map(short_device).unwrap_or_default();
                    Some((key.clone(), format!("{device} · {}", r.info.label)))
                })
                .collect::<Vec<_>>();
            (st.sensors.len(), names)
        };
        let log_directory = logger::default_directory().display().to_string();
        let main = ctx.input(|i| i.viewport().outer_rect);
        let outcome = self.settings_window.show(
            ctx,
            &mut self.settings,
            settings_window::Context2 {
                sensors,
                log_directory,
                show_hidden: &mut self.show_hidden,
                can_change_startup: self.persist,
                sensor_names,
            },
            self.icon.clone(),
            main,
        );
        if outcome.interval_changed {
            self.settings.interval_ms = self.settings.interval_ms.max(MIN_INTERVAL_MS);
            self.engine.set_interval(self.settings.interval_ms);
        }
        if outcome.restart_sensors {
            self.restart_engine(ctx);
        }
        if self.settings != before {
            self.changed();
            ctx.request_repaint();
        }
        if !self.settings_window.open {
            // Closed this frame: write now rather than after the settle delay.
            self.save_settings();
            self.unsaved_since = None;
        }
    }

    fn keyboard(&mut self, ctx: &Context) {
        let (find, reset, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::CTRL, egui::Key::F),
                i.consume_key(egui::Modifiers::CTRL, egui::Key::R),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if find {
            ctx.memory_mut(|m| m.request_focus(self.search_id));
        }
        if reset {
            self.reset_stats();
        }
        if escape && !self.search.is_empty() {
            self.search.clear();
        }
    }
}

/// A device as a graph legend names it: short enough that several series fit on one line.
fn short_device(d: &roch_sensors::model::DeviceInfo) -> String {
    match d.kind {
        DeviceKind::Cpu => "CPU".into(),
        DeviceKind::Board => "Board".into(),
        DeviceKind::System => "System".into(),
        DeviceKind::Memory => "Memory".into(),
        DeviceKind::Errors => "WHEA".into(),
        DeviceKind::Gpu if d.order == 0 => "GPU".into(),
        DeviceKind::Gpu => format!("GPU {}", d.order),
        // "DIMM A2: G Skill Intl F4-..." -> "DIMM A2".
        DeviceKind::Dimm => d.name.split(':').next().unwrap_or(&d.name).to_owned(),
        DeviceKind::Drive | DeviceKind::Network => {
            let n: String = d.name.chars().take(22).collect();
            if d.name.chars().count() > 22 { format!("{n}…") } else { n }
        }
    }
}

/// How many sensor rows of the current device sit above `start`, so a scrolled view keeps the bands
/// in step with an unscrolled one.
fn band_index_before(list: &[Row], start: usize) -> usize {
    let mut band = 0;
    for row in &list[..start] {
        match row {
            Row::Device { .. } => band = 0,
            Row::Sensor { .. } => band += 1,
            Row::Group { .. } => {}
        }
    }
    band
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    ui: &mut Ui,
    st: &State,
    row: &Row,
    band: &mut usize,
    p: &Palette,
    graphs: &[String],
    hidden: &[String],
    in_tray: &[String],
    stale_after: Duration,
    actions: &mut Vec<Action>,
) {
    let width = ui.available_width();
    match row {
        Row::Device { id, kind, name, detail, collapsed, sensors } => {
            let (r, resp) = ui.allocate_exact_size(vec2(width, theme::row()), Sense::click());
            #[cfg(test)]
            tests::record(id, r);
            let painter = ui.painter();
            painter.rect_filled(r, CornerRadius::ZERO, if resp.hovered() { p.hover } else { p.header });
            painter.rect_filled(Rect::from_min_size(r.min, vec2(3.0, r.height())), CornerRadius::ZERO, p.brand);
            let mut x = r.left() + 10.0;
            let chevron = if *collapsed { "▸" } else { "▾" };
            painter.text(pos2(x, r.center().y), Align2::LEFT_CENTER, chevron, theme::bold(body()), p.text);
            x += 16.0;
            let tag =
                painter.text(pos2(x, r.center().y), Align2::LEFT_CENTER, kind.tag(), theme::bold(body()), p.accent);
            x = tag.right() + 10.0;
            let n = painter.text(pos2(x, r.center().y), Align2::LEFT_CENTER, name, theme::bold(body()), p.text);
            x = n.right() + 12.0;
            let look = theme::look();
            let right_edge = if look.show_counts {
                let count_text = if *sensors == 1 { "1 sensor".to_owned() } else { format!("{sensors} sensors") };
                painter
                    .text(
                        pos2(r.right() - 10.0, r.center().y),
                        Align2::RIGHT_CENTER,
                        count_text,
                        theme::regular(body() - 1.0),
                        p.muted,
                    )
                    .left()
            } else {
                r.right()
            };
            if look.show_details && !detail.is_empty() {
                let mut job =
                    egui::text::LayoutJob::simple_singleline(detail.clone(), theme::regular(body() - 1.0), p.muted);
                job.wrap.max_width = (right_edge - 16.0 - x).max(0.0);
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                let galley = painter.layout_job(job);
                painter.galley(pos2(x, r.center().y - galley.size().y / 2.0), galley, p.muted);
            }
            if resp.clicked() {
                actions.push(Action::ToggleDevice(id.clone()));
            }
            resp.context_menu(|ui| {
                if ui.button("Collapse all devices").clicked() {
                    actions.push(Action::CollapseAll);
                    ui.close();
                }
                if ui.button("Expand everything").clicked() {
                    actions.push(Action::ExpandAll);
                    ui.close();
                }
            });
        }
        Row::Group { title } => {
            let (r, _) = ui.allocate_exact_size(vec2(width, theme::row()), Sense::hover());
            let painter = ui.painter();
            let t = painter.text(
                pos2(r.left() + 26.0, r.center().y + 1.0),
                Align2::LEFT_CENTER,
                *title,
                theme::bold(body() - 1.5),
                p.muted,
            );
            painter.hline((t.right() + 8.0)..=(r.right() - 10.0), r.center().y + 1.0, Stroke::new(1.0, p.grid));
        }
        Row::Sensor { index, depth, children, expanded, hidden: is_hidden } => {
            let Some(s) = st.sensors.get(*index) else { return };
            let (r, resp) = ui.allocate_exact_size(vec2(width, theme::row()), Sense::click());
            #[cfg(test)]
            tests::record(&s.info.key, r);
            let cols = MonitorApp::columns(Rect::from_min_max(r.min, pos2(r.right(), r.bottom())));
            let graphed = graphs.iter().position(|k| *k == s.info.key);
            let painter = ui.painter();
            let fill = if resp.hovered() {
                p.hover
            } else if *band % 2 == 1 {
                p.band
            } else {
                Color32::TRANSPARENT
            };
            *band += 1;
            painter.rect_filled(r, CornerRadius::ZERO, fill);
            if let Some(g) = graphed {
                painter.rect_filled(Rect::from_min_size(r.min, vec2(3.0, r.height())), CornerRadius::ZERO, p.series(g));
            }
            let stale = s.last_seen.is_some_and(|t| t.elapsed() > stale_after);
            let dim = *is_hidden || stale;
            let text_color = if dim { p.muted } else { p.text };

            let indent = 26.0 + *depth as f32 * 20.0;
            let mut x = cols[0].left() + indent;
            if *children > 0 {
                painter.text(
                    pos2(x - 14.0, r.center().y),
                    Align2::LEFT_CENTER,
                    if *expanded { "▾" } else { "▸" },
                    theme::bold(body()),
                    p.text,
                );
            }
            let mut job =
                egui::text::LayoutJob::simple_singleline(s.info.label.clone(), theme::regular(body()), text_color);
            job.wrap.max_width = (cols[0].right() - x - 8.0).max(10.0);
            job.wrap.max_rows = 1;
            job.wrap.overflow_character = Some('…');
            let galley = painter.layout_job(job);
            let label_w = galley.size().x;
            painter.galley(pos2(x, r.center().y - galley.size().y / 2.0), galley, text_color);
            x += label_w;
            if *children > 0 && !*expanded {
                painter.text(
                    pos2(x + 8.0, r.center().y),
                    Align2::LEFT_CENTER,
                    format!("+{children}"),
                    theme::regular(body() - 2.0),
                    p.muted,
                );
            }

            let (u, d) = (s.info.unit.as_str(), s.info.decimals);
            let figures = [
                (format_value(s.stat.current, u, d), true),
                (format_value(s.stat.minimum, u, d), false),
                (format_value(s.stat.maximum, u, d), false),
                (format_value(s.stat.average(), u, d), false),
            ];
            for (c, (text, current)) in cols[1..].iter().zip(figures) {
                let (font, color) = if current {
                    (theme::value_font(body()), if dim { p.muted } else { p.accent })
                } else {
                    (theme::regular(body()), text_color)
                };
                painter.text(pos2(c.right() - 8.0, c.center().y), Align2::RIGHT_CENTER, text, font, color);
            }

            let in_name = resp.hover_pos().is_some_and(|at| cols[0].contains(at));
            if resp.clicked() {
                let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
                let on_name = resp.interact_pointer_pos().is_some_and(|at| cols[0].contains(at));
                if *children > 0 && on_name && !ctrl {
                    actions.push(Action::ToggleExpand(s.info.key.clone()));
                } else {
                    actions.push(Action::Graph(s.info.key.clone(), ctrl));
                }
            }
            let key = s.info.key.clone();
            let line = format!(
                "{}: {}  (min {}, max {}, avg {})",
                s.info.label,
                format_value(s.stat.current, u, d),
                format_value(s.stat.minimum, u, d),
                format_value(s.stat.maximum, u, d),
                format_value(s.stat.average(), u, d)
            );
            let resp = if in_name {
                let mut tip = s.info.label.clone();
                if !s.info.source.is_empty() {
                    tip.push_str(&format!("\n{}", s.info.source));
                }
                tip.push_str(&format!("\n{}  ·  {} samples", s.info.key, s.stat.samples()));
                if *children > 0 {
                    tip.push_str("\nClick the name to show or hide the per-thread rows.");
                }
                resp.on_hover_text(tip)
            } else {
                resp
            };
            let already_hidden = hidden.contains(&key);
            resp.context_menu(|ui| {
                let graph_label = if graphed.is_some() { "Remove from graph" } else { "Graph" };
                if ui.button(graph_label).clicked() {
                    actions.push(Action::Graph(key.clone(), true));
                    ui.close();
                }
                if *children > 0
                    && ui.button(if *expanded { "Fold per-thread rows" } else { "Show per-thread rows" }).clicked()
                {
                    actions.push(Action::ToggleExpand(key.clone()));
                    ui.close();
                }
                if ui.button("Reset this sensor's statistics").clicked() {
                    actions.push(Action::ResetSensor(key.clone()));
                    ui.close();
                }
                if ui.button("Copy").clicked() {
                    actions.push(Action::Copy(line.clone()));
                    ui.close();
                }
                let tray_label = if in_tray.contains(&key) { "Remove from tray" } else { "Show in tray" };
                let full = in_tray.len() >= super::settings::MAX_TRAY_SENSORS && !in_tray.contains(&key);
                if ui
                    .add_enabled(!full, egui::Button::new(tray_label))
                    .on_disabled_hover_text("Six sensors are in the tray already")
                    .clicked()
                {
                    actions.push(Action::Tray(key.clone()));
                    ui.close();
                }
                ui.separator();
                if already_hidden {
                    if ui.button("Unhide sensor").clicked() {
                        actions.push(Action::Unhide(key.clone()));
                        ui.close();
                    }
                } else if ui.button("Hide sensor").clicked() {
                    actions.push(Action::Hide(key.clone()));
                    ui.close();
                }
            });
        }
    }
}

/// Roch GPU's monitor scrollbar: a slim grip, muted, no track.
fn style_scrollbar(ui: &mut Ui, p: &Palette) {
    let s = &mut ui.spacing_mut().scroll;
    *s = egui::style::ScrollStyle::solid();
    s.bar_width = 9.0;
    s.bar_inner_margin = 3.0;
    s.bar_outer_margin = 2.0;
    s.foreground_color = true;
    let v = &mut ui.visuals_mut().widgets;
    v.inactive.fg_stroke.color = p.muted.gamma_multiply(0.35);
    v.hovered.fg_stroke.color = p.muted.gamma_multiply(0.65);
    v.active.fg_stroke.color = p.muted.gamma_multiply(0.85);
}

impl eframe::App for MonitorApp {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.keyboard(ctx);
        self.apply_appearance(ctx);
        self.tray_tick(ctx);
        self.save_if_settled();
        if self.settings.appearance.theme == appearance::ThemeMode::System {
            // Follow Windows has to look again now and then, frame or no frame.
            ctx.request_repaint_after(SYSTEM_THEME_CHECK);
        }
        if let Some(r) = ctx.input(|i| i.viewport().inner_rect) {
            // In the window's own units, not egui's zoomed points, so a 150% scale does not shrink
            // the window it reopens at.
            let zoom = ctx.zoom_factor();
            self.settings.window_size = Some([(r.width() * zoom).round(), (r.height() * zoom).round()]);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let dark = self.applied.as_ref().is_none_or(|(_, dark)| *dark);
        Palette::from_appearance(&self.settings.appearance, dark).bg.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(mut l) = self.logger.take() {
            l.stop();
        }
        self.engine.stop();
        self.save_settings();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;

    use eframe::egui::{Event, Modifiers, PointerButton, Pos2, RawInput};
    use roch_sensors::engine::{Batch, Source};
    use roch_sensors::model::{DeviceInfo, Group, SensorInfo};

    use super::*;

    thread_local! {
        static RECTS: RefCell<Vec<(String, Rect)>> = const { RefCell::new(Vec::new()) };
    }

    /// Where each device heading and sensor row was drawn this frame.
    pub(crate) fn record(key: &str, r: Rect) {
        RECTS.with(|v| v.borrow_mut().push((key.to_owned(), r)));
    }

    pub(crate) fn rect_of(key: &str) -> Option<Rect> {
        RECTS.with(|v| v.borrow().iter().rev().find(|(k, _)| k == key).map(|(_, r)| *r))
    }

    struct Fixture;

    impl Source for Fixture {
        fn name(&self) -> &str {
            "fixture"
        }
        fn poll(&mut self, out: &mut Batch) {
            out.device(DeviceInfo::new("cpu", DeviceKind::Cpu, "Test CPU"));
            out.device(DeviceInfo::new("board", DeviceKind::Board, "Test Board"));
            out.put(&SensorInfo::new("cpu", "temp/max", Group::Temperatures, "Core Max", "°C", 1), 40.0);
            let eff = SensorInfo::new("cpu", "clk/eff", Group::Clocks, "Core Effective Clock", "MHz", 0);
            out.put(&eff, 100.0);
            out.put(
                &SensorInfo::new("cpu", "clk/eff0", Group::Clocks, "Core 0 Effective Clock", "MHz", 0)
                    .parent("clk/eff"),
                50.0,
            );
            out.put(&SensorInfo::new("board", "fan/cpu", Group::Fans, "CPU Fan", "RPM", 0), 900.0);
            out.put(&SensorInfo::new("board", "temp/vrm", Group::Temperatures, "VRM MOS", "°C", 1), 45.0);
        }
    }

    pub(crate) struct Harness {
        pub(crate) ctx: Context,
        pub(crate) app: MonitorApp,
    }

    impl Harness {
        pub(crate) fn new() -> Harness {
            let ctx = Context::default();
            theme::install_fonts(&ctx, crate::gui::fonts::DEFAULT_FAMILY);
            theme::apply(&ctx, &Appearance::default(), true);
            // Fonts take effect from the next pass.
            ctx.run_ui(RawInput::default(), |_| {}).textures_delta.clear();
            let engine = Engine::start(
                vec![("fixture".into(), Box::new(|_: &mut Batch| Some(Box::new(Fixture) as Box<dyn Source>)))],
                MIN_INTERVAL_MS,
                64,
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while engine.lock().sensors.len() < 5 {
                assert!(Instant::now() < deadline, "the fixture never reported");
                std::thread::sleep(Duration::from_millis(10));
            }
            let app = MonitorApp::with_engine(
                SystemInfo::default(),
                Settings::default(),
                Options::default(),
                engine,
                None,
                false,
            );
            let mut h = Harness { ctx, app };
            h.frame(vec![], Modifiers::NONE);
            h
        }

        pub(crate) fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers) {
            RECTS.with(|v| v.borrow_mut().clear());
            // egui 0.37 takes the modifier state as an event rather than a field.
            let mut all = vec![Event::ModifiersChanged(modifiers)];
            all.extend(events);
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0))),
                events: all,
                ..Default::default()
            };
            // What `logic` does before each frame in the real window.
            self.app.apply_appearance(&self.ctx);
            let app = &mut self.app;
            // Nothing paints these passes, and egui insists on being told so.
            self.ctx.run_ui(input, |ui| app.draw(ui)).textures_delta.clear();
        }

        pub(crate) fn click(&mut self, at: Pos2, modifiers: Modifiers) {
            self.frame(vec![Event::PointerMoved(at)], modifiers);
            self.frame(
                vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers }],
                modifiers,
            );
            self.frame(
                vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers }],
                modifiers,
            );
            self.frame(vec![], Modifiers::NONE);
        }

        /// A point in a row's value columns, away from the name.
        fn values_of(&self, key: &str) -> Pos2 {
            let r = rect_of(key).unwrap_or_else(|| panic!("{key} was not drawn"));
            pos2(r.right() - 60.0, r.center().y)
        }
    }

    fn texts(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
            _ => {}
        }
    }

    #[test]
    fn the_sources_window_names_both_drivers() {
        let mut h = Harness::new();
        h.app.show_sources = true;
        h.frame(vec![], Modifiers::NONE);
        let input =
            RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0))), ..Default::default() };
        let app = &mut h.app;
        let mut out = h.ctx.run_ui(input, |ui| app.draw(ui));
        out.textures_delta.clear();
        let mut drawn = Vec::new();
        out.shapes.iter().for_each(|s| texts(&s.shape, &mut drawn));
        let all = drawn.join("\n");
        // Whatever this machine has: PawnIO's state, and inpoutx64's without loading it.
        assert!(all.lines().any(|l| l.starts_with("PawnIO: ")), "{all}");
        assert!(all.lines().any(|l| l.starts_with("inpoutx64: ")), "{all}");
        assert!(roch_sensors::hw::inpout::loaded().is_none(), "drawing the window loaded inpoutx64");
    }

    #[test]
    fn clicking_a_sensor_graphs_it_and_ctrl_click_adds_another() {
        let mut h = Harness::new();
        let at = h.values_of("cpu/temp/max");
        h.click(at, Modifiers::NONE);
        assert_eq!(h.app.settings.graphs, vec!["cpu/temp/max"]);
        let at = h.values_of("board/fan/cpu");
        h.click(at, Modifiers::CTRL);
        assert_eq!(h.app.settings.graphs, vec!["cpu/temp/max", "board/fan/cpu"]);
        // A plain click on a graphed row takes it off again.
        let at = h.values_of("cpu/temp/max");
        h.click(at, Modifiers::NONE);
        assert_eq!(h.app.settings.graphs, vec!["board/fan/cpu"]);
    }

    #[test]
    fn clicking_a_parents_name_unfolds_its_children_without_graphing() {
        let mut h = Harness::new();
        assert!(rect_of("cpu/clk/eff0").is_none(), "children start folded");
        let r = rect_of("cpu/clk/eff").unwrap();
        h.click(pos2(r.left() + 60.0, r.center().y), Modifiers::NONE);
        assert!(h.app.settings.expanded.contains(&"cpu/clk/eff".to_owned()));
        assert!(h.app.settings.graphs.is_empty(), "the name toggles the fold, not the graph");
        assert!(rect_of("cpu/clk/eff0").is_some(), "the child row is drawn once unfolded");
    }

    #[test]
    fn clicking_a_device_heading_folds_the_device() {
        let mut h = Harness::new();
        let r = rect_of("board").unwrap();
        h.click(r.center(), Modifiers::NONE);
        assert_eq!(h.app.settings.collapsed, vec!["board"]);
        assert!(rect_of("board/fan/cpu").is_none());
        assert!(rect_of("board").is_some(), "the heading stays to unfold it again");
    }

    #[test]
    fn searching_narrows_the_list() {
        let mut h = Harness::new();
        h.app.search = "vrm".into();
        h.frame(vec![], Modifiers::NONE);
        assert!(rect_of("board/temp/vrm").is_some());
        assert!(rect_of("cpu/temp/max").is_none());
        assert!(rect_of("cpu").is_none(), "a device with nothing matching is not shown");
    }

    #[test]
    fn hiding_a_sensor_removes_it_and_its_graph() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        h.app.apply(&ctx, Action::Graph("board/fan/cpu".into(), false));
        h.app.apply(&ctx, Action::Hide("board/fan/cpu".into()));
        h.frame(vec![], Modifiers::NONE);
        assert!(rect_of("board/fan/cpu").is_none());
        assert!(h.app.settings.graphs.is_empty());
        h.app.show_hidden = true;
        h.frame(vec![], Modifiers::NONE);
        assert!(rect_of("board/fan/cpu").is_some(), "Show hidden brings it back, dimmed");
    }
}

#[cfg(test)]
mod settings_tests {
    use eframe::egui::Modifiers;

    use super::tests::{Harness, rect_of};
    use super::*;
    use crate::gui::appearance::{ThemeMode, presets};
    use crate::gui::settings_window::Page;

    fn open_settings(h: &mut Harness) {
        let ctx = h.ctx.clone();
        h.app.settings_window.open(&ctx);
        h.frame(vec![], Modifiers::NONE);
        h.frame(vec![], Modifiers::NONE);
    }

    #[test]
    fn the_settings_button_opens_the_window() {
        let mut h = Harness::new();
        assert!(!h.app.settings_window.open);
        let at = rect_of("button:settings").expect("the button is drawn").center();
        h.click(at, Modifiers::NONE);
        assert!(h.app.settings_window.open);
    }

    #[test]
    fn every_page_draws() {
        let mut h = Harness::new();
        open_settings(&mut h);
        for (page, name) in Page::ALL {
            h.app.settings_window.page = page;
            h.frame(vec![], Modifiers::NONE);
            assert!(rect_of(&format!("page:{name}")).is_some(), "the nav lists {name}");
        }
        assert!(h.app.settings_window.open);
    }

    #[test]
    fn choosing_light_repaints_the_window_light() {
        let mut h = Harness::new();
        assert!(pal(&h.ctx).dark);
        open_settings(&mut h);
        let at = rect_of("theme:Light").expect("the Light radio is drawn").center();
        h.click(at, Modifiers::NONE);
        assert_eq!(h.app.settings.appearance.theme, ThemeMode::Light);
        h.frame(vec![], Modifiers::NONE);
        let p = pal(&h.ctx);
        assert!(!p.dark);
        assert_eq!(p.bg, Color32::WHITE);
    }

    #[test]
    fn a_preset_recolours_both_modes() {
        let mut h = Harness::new();
        open_settings(&mut h);
        let at = rect_of("preset:Ocean").expect("the Ocean preset is drawn").center();
        h.click(at, Modifiers::NONE);
        let ocean = presets().into_iter().find(|p| p.name == "Ocean").unwrap();
        assert_eq!(h.app.settings.appearance.dark, ocean.dark);
        assert_eq!(h.app.settings.appearance.light, ocean.light);
        h.frame(vec![], Modifiers::NONE);
        assert_eq!(pal(&h.ctx).bg, ocean.dark.background);
    }

    #[test]
    fn the_nav_switches_pages() {
        let mut h = Harness::new();
        open_settings(&mut h);
        let at = rect_of("page:Text").unwrap().center();
        h.click(at, Modifiers::NONE);
        assert_eq!(h.app.settings_window.page, Page::Text);
    }

    #[test]
    fn escape_closes_it() {
        let mut h = Harness::new();
        open_settings(&mut h);
        h.frame(
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            Modifiers::NONE,
        );
        assert!(!h.app.settings_window.open);
    }

    #[test]
    fn text_size_font_and_layout_reach_the_list_live() {
        let mut h = Harness::new();
        let before = rect_of("cpu/temp/max").unwrap().height();
        h.app.settings.appearance.text_size = 16.0;
        h.app.settings.appearance.font_family = "Courier New".into();
        h.frame(vec![], Modifiers::NONE);
        h.frame(vec![], Modifiers::NONE);
        let after = rect_of("cpu/temp/max").unwrap().height();
        assert!(after > before, "rows grow with the text: {before} -> {after}");
        // Value columns: the name column takes what they leave.
        h.app.settings.appearance.value_width = 160.0;
        h.frame(vec![], Modifiers::NONE);
        assert_eq!(theme::look().value_width, 160.0);
        // Back to Consolas: the fonts are reinstalled, not left on the old family.
        h.app.settings.appearance.font_family = "Consolas".into();
        h.frame(vec![], Modifiers::NONE);
        assert_eq!(h.app.applied_font.as_deref(), Some("Consolas"));
    }

    #[test]
    fn group_headings_can_be_turned_off() {
        let mut h = Harness::new();
        let count = |h: &Harness| {
            let st = h.app.engine.lock();
            rows::build(
                &st,
                &rows::View {
                    search: "",
                    hidden: &[],
                    show_hidden: false,
                    collapsed: &[],
                    expanded: &[],
                    group_headings: h.app.settings.appearance.group_headings,
                },
            )
            .iter()
            .filter(|r| matches!(r, Row::Group { .. }))
            .count()
        };
        assert!(count(&h) > 0);
        h.app.settings.appearance.group_headings = false;
        h.frame(vec![], Modifiers::NONE);
        assert_eq!(count(&h), 0);
    }
}

#[cfg(test)]
mod tray_tests {
    use eframe::egui::Modifiers;

    use super::tests::Harness;
    use super::*;

    #[test]
    fn chosen_sensors_become_value_icons_and_tooltip_lines() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        let (tooltip, values) = h.app.tray_contents();
        assert_eq!(tooltip, "Roch Monitor", "none of the default readings exist on the fixture");
        assert!(values.is_empty());
        h.app.apply(&ctx, Action::Tray("board/fan/cpu".into()));
        h.app.apply(&ctx, Action::Tray("cpu/temp/max".into()));
        let (tooltip, values) = h.app.tray_contents();
        assert!(tooltip.contains("Board CPU Fan: 900 RPM"), "{tooltip}");
        assert!(tooltip.contains("CPU Core Max: 40.0 °C"), "{tooltip}");
        let digits: Vec<&str> = values.iter().map(|v| v.digits.as_str()).collect();
        assert_eq!(digits, vec!["900", "40"]);
        // Off: the tooltip keeps them, the icons go.
        h.app.settings.tray_icons = false;
        assert!(h.app.tray_contents().1.is_empty());
        // Choosing a sensor again takes it out of the tray.
        h.app.apply(&ctx, Action::Tray("board/fan/cpu".into()));
        assert_eq!(h.app.settings.tray_sensors, vec!["cpu/temp/max"]);
    }

    #[test]
    fn the_tray_holds_at_most_six() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        for i in 0..9 {
            h.app.apply(&ctx, Action::Tray(format!("x/{i}")));
        }
        assert_eq!(h.app.settings.tray_sensors.len(), crate::gui::settings::MAX_TRAY_SENSORS);
    }

    #[test]
    fn starting_in_the_tray_without_a_tray_shows_the_window() {
        // Under test there is no shell to make an icon in, which is the case a machine without a
        // notification area hits: the window must not stay invisible with no way back.
        let mut h = Harness::new();
        h.app.start_in_tray();
        assert!(h.app.hidden);
        h.app.tray_due = true;
        h.app.tray_tick(&h.ctx.clone());
        if h.app.tray.is_none() {
            assert!(!h.app.hidden);
        }
        h.frame(vec![], Modifiers::NONE);
    }
}
