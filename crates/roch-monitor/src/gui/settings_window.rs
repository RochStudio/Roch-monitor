//! The Settings window (Roch GPU's Settings gear, which is Roch Viewer's appearance settings): theme,
//! colours, font, sizes, layout, window behaviour and the sensor options, applied live to the main
//! window as they change.
//!
//! It is its own window, as in Roch GPU, and draws its controls in Consolas at a fixed size whatever
//! the table uses, so no font choice can make the place it is changed back unreadable.

use std::collections::BTreeMap;
use std::sync::Arc;

use eframe::egui::{self, Color32, Context, RichText, Ui, ViewportClass, ViewportId, vec2};

use super::appearance::{self, Appearance, COLOR_ROLES, Colors, ThemeMode};
use super::fonts;
use super::settings::Settings;
use super::theme::{self, Palette};

const NAV_WIDTH: f32 = 136.0;
const TEXT: f32 = 13.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Appearance,
    Colours,
    Text,
    Layout,
    Window,
    Sensors,
}

impl Page {
    /// "colours", "Text", "layout": a page by its name, any case ("colors" too).
    pub fn from_name(name: &str) -> Option<Page> {
        let wanted = name.trim().to_lowercase().replace("colors", "colours");
        Self::ALL.iter().find(|(_, n)| n.to_lowercase() == wanted).map(|(p, _)| *p)
    }

    pub const ALL: [(Page, &'static str); 6] = [
        (Page::Appearance, "Appearance"),
        (Page::Colours, "Colours"),
        (Page::Text, "Text"),
        (Page::Layout, "Layout"),
        (Page::Window, "Window"),
        (Page::Sensors, "Sensors"),
    ];
}

/// What the app has to do about a change made here, beyond repainting.
#[derive(Default, Debug, PartialEq)]
pub struct Outcome {
    pub interval_changed: bool,
    pub restart_sensors: bool,
}

/// What the Sensors page needs to know about the running monitor.
pub struct Context2<'a> {
    pub sensors: usize,
    pub log_directory: String,
    pub show_hidden: &'a mut bool,
}

pub struct SettingsWindow {
    pub open: bool,
    pub page: Page,
    just_opened: bool,
    font_filter: String,
    editing_dark: Option<bool>,
    hex: BTreeMap<String, String>,
}

impl Default for SettingsWindow {
    fn default() -> Self {
        Self {
            open: false,
            page: Page::Appearance,
            just_opened: false,
            font_filter: String::new(),
            editing_dark: None,
            hex: BTreeMap::new(),
        }
    }
}

pub const INTERVALS: [(u64, &str); 6] =
    [(250, "250 ms"), (500, "500 ms"), (1000, "1 s"), (2000, "2 s"), (5000, "5 s"), (10000, "10 s")];
pub const GRAPH_WINDOWS: [(u32, &str); 5] =
    [(60, "1 min"), (300, "5 min"), (900, "15 min"), (1800, "30 min"), (3600, "60 min")];

impl SettingsWindow {
    pub fn id() -> ViewportId {
        ViewportId::from_hash_of("roch_monitor_settings")
    }

    /// Open it, or bring it forward when it is already open.
    pub fn open(&mut self, ctx: &Context) {
        if self.open {
            ctx.send_viewport_cmd_to(Self::id(), egui::ViewportCommand::Focus);
        } else {
            self.open = true;
            self.just_opened = true;
            self.hex.clear();
        }
    }

    pub fn show(
        &mut self,
        ctx: &Context,
        settings: &mut Settings,
        info: Context2,
        icon: Option<Arc<egui::IconData>>,
        main: Option<egui::Rect>,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        if !self.open {
            return outcome;
        }
        let size = vec2(720.0, 620.0);
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Roch Monitor — Settings")
            .with_inner_size(size)
            .with_min_inner_size([560.0, 420.0])
            .with_window_level(if settings.appearance.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            });
        if let Some(icon) = icon {
            builder = builder.with_icon(icon);
        }
        if self.just_opened
            && let Some(main) = main
        {
            builder = builder.with_position(main.center() - size / 2.0);
        }
        self.just_opened = false;
        let mut info = info;
        ctx.show_viewport_immediate(Self::id(), builder, |ui, class| {
            if class != ViewportClass::EmbeddedWindow && ui.input(|i| i.viewport().close_requested()) {
                self.open = false;
                return;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.open = false;
            }
            let palette = theme::pal(ui.ctx());
            settings_style(ui, &palette);
            egui::Panel::bottom("settings-footer")
                .frame(egui::Frame::new().fill(palette.bg).inner_margin(egui::Margin::symmetric(12, 8)))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Reset appearance")
                            .on_hover_text(
                                "Every page but Sensors back to the Roch defaults. Hidden sensors and logs are kept.",
                            )
                            .clicked()
                        {
                            settings.appearance = Appearance::default();
                            self.hex.clear();
                        }
                        if ui
                            .button("Settings folder")
                            .on_hover_text("Open the folder that holds settings.json")
                            .clicked()
                        {
                            super::native::reveal(&super::settings::directory());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Close").clicked() {
                                self.open = false;
                            }
                        });
                    });
                });
            egui::Panel::left("settings-nav")
                .exact_size(NAV_WIDTH)
                .resizable(false)
                .frame(egui::Frame::new().fill(palette.bg).inner_margin(egui::Margin::symmetric(8, 12)))
                .show(ui, |ui| {
                    for (page, name) in Page::ALL {
                        let selected = self.page == page;
                        let button = egui::Button::new(RichText::new(name).color(if selected {
                            Color32::WHITE
                        } else {
                            palette.text
                        }))
                        .fill(if selected { palette.brand } else { palette.bg })
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(14)
                        .min_size(vec2(NAV_WIDTH - 16.0, 26.0));
                        let response = ui.add(button);
                        #[cfg(test)]
                        super::app::tests::record(&format!("page:{name}"), response.rect);
                        if response.clicked() {
                            self.page = page;
                        }
                    }
                });
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(palette.bg).inner_margin(egui::Margin {
                    left: 16,
                    right: 14,
                    top: 12,
                    bottom: 4,
                }))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt(self.page as u8).auto_shrink([false, false]).show(ui, |ui| {
                        let title = Page::ALL.iter().find(|(p, _)| *p == self.page).map_or("", |(_, n)| n);
                        ui.label(
                            RichText::new(title).font(theme::settings_font(TEXT + 3.0, true)).color(palette.brand),
                        );
                        ui.add_space(6.0);
                        match self.page {
                            Page::Appearance => self.appearance(ui, &mut settings.appearance),
                            Page::Colours => self.colours(ui, &mut settings.appearance),
                            Page::Text => self.text(ui, &mut settings.appearance),
                            Page::Layout => Self::layout(ui, &mut settings.appearance),
                            Page::Window => Self::window(ui, &mut settings.appearance),
                            Page::Sensors => Self::sensors(ui, settings, &mut info, &mut outcome),
                        }
                        ui.add_space(12.0);
                    });
                });
        });
        outcome
    }

    fn appearance(&mut self, ui: &mut Ui, a: &mut Appearance) {
        section(ui, "Theme");
        ui.horizontal(|ui| {
            for mode in ThemeMode::ALL {
                let _response = ui.radio_value(&mut a.theme, mode, mode.name());
                #[cfg(test)]
                super::app::tests::record(&format!("theme:{}", mode.name()), _response.rect);
            }
        });
        hint(ui, "Follow Windows switches with Settings > Personalisation > Colours.");
        section(ui, "Colour scheme");
        ui.horizontal_wrapped(|ui| {
            for preset in appearance::presets() {
                let current = preset.light == a.light && preset.dark == a.dark;
                let response = ui.selectable_label(current, preset.name);
                #[cfg(test)]
                super::app::tests::record(&format!("preset:{}", preset.name), response.rect);
                if response.clicked() {
                    a.light = preset.light;
                    a.dark = preset.dark;
                    self.hex.clear();
                }
            }
            if a.preset_name().is_none() {
                let _ = ui.selectable_label(true, "Custom");
            }
        });
        hint(ui, "A starting point: every colour can be changed on the Colours page.");
        section(ui, "Contrast");
        let mut percent = (a.contrast * 100.0).round();
        if ui.add(egui::Slider::new(&mut percent, -100.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%")).changed()
        {
            a.contrast = percent / 100.0;
        }
        hint(ui, "Above 0 pushes text and readings further from the background; below 0 softens them.");
        section(ui, "Rows");
        ui.checkbox(&mut a.row_shading, "Shade every other row");
        ui.horizontal(|ui| {
            ui.label("Corner roundness");
            ui.add(
                egui::Slider::new(&mut a.corner_radius, appearance::CORNER.0..=appearance::CORNER.1)
                    .step_by(1.0)
                    .fixed_decimals(0)
                    .suffix(" px"),
            );
        });
    }

    fn colours(&mut self, ui: &mut Ui, a: &mut Appearance) {
        let palette = theme::pal(ui.ctx());
        let editing_dark = self.editing_dark.unwrap_or(palette.dark);
        ui.horizontal(|ui| {
            ui.label("Editing");
            let mut dark = editing_dark;
            ui.selectable_value(&mut dark, false, "Light mode");
            ui.selectable_value(&mut dark, true, "Dark mode");
            if dark != editing_dark {
                self.editing_dark = Some(dark);
                self.hex.clear();
            }
        });
        hint(ui, "Each mode keeps its own colours. Click a swatch to pick, or type a #RRGGBB value.");
        ui.add_space(6.0);
        let defaults = if editing_dark { Colors::DEFAULT_DARK } else { Colors::DEFAULT_LIGHT };
        egui::Grid::new("colour-roles").num_columns(4).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
            for (role, label) in COLOR_ROLES {
                let key = format!("{}-{role}", if editing_dark { "dark" } else { "light" });
                let colors = a.colors_mut(editing_dark);
                let mut color = colors.get(role);
                ui.label(*label);
                if egui::color_picker::color_edit_button_srgba(ui, &mut color, egui::color_picker::Alpha::Opaque)
                    .changed()
                {
                    colors.set(role, color);
                    self.hex.remove(&key);
                }
                let text = self.hex.entry(key.clone()).or_insert_with(|| appearance::hex(color));
                let response = ui.add_sized([96.0, 22.0], egui::TextEdit::singleline(text).char_limit(7));
                if response.changed()
                    && let Some(parsed) = appearance::parse_hex(text)
                {
                    a.colors_mut(editing_dark).set(role, parsed);
                }
                if response.lost_focus() {
                    *text = appearance::hex(a.colors(editing_dark).get(role));
                }
                let default = defaults.get(role);
                if ui.add_enabled(color != default, egui::Button::new("Default")).clicked() {
                    a.colors_mut(editing_dark).set(role, default);
                    self.hex.remove(&key);
                }
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        if ui.button(format!("Reset all {} colours", if editing_dark { "dark" } else { "light" })).clicked() {
            *a.colors_mut(editing_dark) = defaults;
            self.hex.clear();
        }
        if !a.row_shading {
            hint(ui, "Row shading is off: turn it on under Appearance to see the shading colour.");
        }
    }

    fn text(&mut self, ui: &mut Ui, a: &mut Appearance) {
        let palette = theme::pal(ui.ctx());
        section(ui, "Font");
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.add(egui::TextEdit::singleline(&mut self.font_filter).desired_width(200.0).hint_text("font name"));
            if ui
                .add_enabled(
                    !a.font_family.eq_ignore_ascii_case(fonts::DEFAULT_FAMILY),
                    egui::Button::new(fonts::DEFAULT_FONT_LABEL),
                )
                .clicked()
            {
                a.font_family = fonts::DEFAULT_FAMILY.into();
            }
        });
        let filter = self.font_filter.to_lowercase();
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, palette.border))
            .corner_radius(theme::radius(4))
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("font-list").max_height(200.0).auto_shrink([false, true]).show(
                    ui,
                    |ui| {
                        for family in fonts::installed_families()
                            .iter()
                            .filter(|f| filter.is_empty() || f.name.to_lowercase().contains(&filter))
                        {
                            let selected = family.name.eq_ignore_ascii_case(&a.font_family);
                            let label = if family.monospace {
                                format!("{}  · fixed width", family.name)
                            } else {
                                family.name.clone()
                            };
                            if ui.selectable_label(selected, label).clicked() {
                                a.font_family = family.name.clone();
                            }
                        }
                    },
                );
            });
        hint(ui, "Fixed-width fonts keep the columns most even. Consolas is the Roch tools' own.");
        section(ui, "Size");
        ui.horizontal(|ui| {
            ui.label("Text size");
            ui.add(
                egui::Slider::new(&mut a.text_size, appearance::TEXT_SIZE.0..=appearance::TEXT_SIZE.1)
                    .step_by(0.5)
                    .fixed_decimals(1)
                    .suffix(" px"),
            );
        });
        hint(ui, "Rows grow with the text so it stays clear of the next one.");
        section(ui, "Weight");
        ui.checkbox(&mut a.bold_headings, "Bold headings, device names and column titles");
        ui.checkbox(&mut a.bold_values, "Bold current readings");
        section(ui, "Preview");
        preview(ui, a, &palette);
    }

    fn layout(ui: &mut Ui, a: &mut Appearance) {
        section(ui, "Scale");
        let mut scale = a.ui_scale * 100.0;
        if ui.add(egui::Slider::new(&mut scale, 75.0..=200.0).step_by(5.0).fixed_decimals(0).suffix("%")).changed() {
            a.ui_scale = scale / 100.0;
        }
        hint(ui, "Everything at once: text, buttons, spacing and graphs.");
        section(ui, "Rows and columns");
        ui.horizontal(|ui| {
            ui.label("Row spacing");
            ui.add(
                egui::Slider::new(&mut a.row_spacing, appearance::ROW_SPACING.0..=appearance::ROW_SPACING.1)
                    .step_by(1.0)
                    .fixed_decimals(0)
                    .suffix(" px"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Value columns");
            ui.add(
                egui::Slider::new(&mut a.value_width, appearance::VALUE_WIDTH.0..=appearance::VALUE_WIDTH.1)
                    .step_by(2.0)
                    .fixed_decimals(0)
                    .suffix(" px"),
            );
        });
        hint(ui, "The width of each of Current, Minimum, Maximum and Average; the sensor name takes the rest.");
        section(ui, "What the list shows");
        ui.checkbox(&mut a.group_headings, "Group headings under each device (TEMPERATURES, VOLTAGES, ...)");
        ui.checkbox(&mut a.show_counts, "Sensor counts on device headings");
        ui.checkbox(&mut a.show_details, "Details on device headings (chip, driver, link)");
        ui.checkbox(&mut a.show_summary, "Machine summary under the title");
    }

    fn window(ui: &mut Ui, a: &mut Appearance) {
        section(ui, "On screen");
        ui.checkbox(&mut a.always_on_top, "Keep Roch Monitor above other programs");
        let mut opacity = a.opacity * 100.0;
        ui.horizontal(|ui| {
            ui.label("Opacity");
            if ui.add(egui::Slider::new(&mut opacity, 30.0..=100.0).fixed_decimals(0).suffix("%")).changed() {
                a.opacity = opacity / 100.0;
            }
        });
        hint(ui, "For the main window; this one stays opaque so its controls stay readable.");
        section(ui, "Size");
        hint(ui, "The window opens at the size it was closed at.");
    }

    fn sensors(ui: &mut Ui, s: &mut Settings, info: &mut Context2, outcome: &mut Outcome) {
        section(ui, "Live readings");
        ui.horizontal(|ui| {
            ui.label("Update every");
            let current = INTERVALS.iter().find(|(ms, _)| *ms == s.interval_ms).map_or("custom", |(_, l)| l);
            let mut chosen = s.interval_ms;
            egui::ComboBox::from_id_salt("settings-interval").selected_text(current).width(90.0).show_ui(ui, |ui| {
                for (ms, label) in INTERVALS {
                    ui.selectable_value(&mut chosen, ms, label);
                }
            });
            if chosen != s.interval_ms {
                s.interval_ms = chosen;
                outcome.interval_changed = true;
            }
        });
        hint(ui, "Each source reads on its own thread; faster costs a little processor time and bus time.");
        section(ui, "Graphs");
        ui.horizontal(|ui| {
            ui.label("Show the last");
            let current = GRAPH_WINDOWS.iter().find(|(sec, _)| *sec == s.graph_seconds).map_or("5 min", |(_, l)| l);
            egui::ComboBox::from_id_salt("settings-graph").selected_text(current).width(90.0).show_ui(ui, |ui| {
                for (sec, label) in GRAPH_WINDOWS {
                    ui.selectable_value(&mut s.graph_seconds, sec, label);
                }
            });
        });
        section(ui, "DDR5 modules");
        if ui
            .checkbox(&mut s.pmic_adc, "Read each PMIC's measured rails (ADC)")
            .on_hover_text("One write per channel to the PMIC's ADC selector, R30h, restored afterwards. Off reads only the settings.")
            .changed()
        {
            outcome.restart_sensors = true;
        }
        hint(ui, "Changing it restarts the sensors, and with them the statistics.");
        section(ui, "Hidden sensors");
        ui.horizontal(|ui| {
            ui.label(format!("{} hidden of {}", s.hidden.len(), info.sensors));
            if ui.add_enabled(!s.hidden.is_empty(), egui::Button::new("Unhide all")).clicked() {
                s.hidden.clear();
            }
        });
        ui.checkbox(info.show_hidden, "Show hidden sensors in the list, dimmed");
        hint(ui, "Right-click a sensor in the list to hide it.");
        section(ui, "Logs and reports");
        let mut dir = s.log_directory.clone().unwrap_or_else(|| info.log_directory.clone());
        ui.horizontal(|ui| {
            ui.label("Folder");
            if ui.add(egui::TextEdit::singleline(&mut dir).desired_width(340.0)).changed() {
                s.log_directory = Some(dir.clone()).filter(|d| !d.trim().is_empty());
            }
            if ui.button("Open").clicked() {
                let _ = std::fs::create_dir_all(&dir);
                super::native::reveal(std::path::Path::new(&dir));
            }
            if ui.add_enabled(s.log_directory.is_some(), egui::Button::new("Default")).clicked() {
                s.log_directory = None;
            }
        });
    }
}

/// A few rows drawn the way the list will draw them with these choices.
fn preview(ui: &mut Ui, a: &Appearance, p: &Palette) {
    let rows = [
        ("Core Max", "38.0 °C", "34.0 °C", "55.0 °C"),
        ("CPU Package Power", "28.7 W", "26.9 W", "49.9 W"),
        ("Pump Fan", "2040 RPM", "2020 RPM", "2042 RPM"),
    ];
    let size = a.text_size;
    let height = (size + 10.0 + a.row_spacing).max(size + 4.0).round();
    let width = ui.available_width().min(520.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height * (rows.len() + 1) as f32 + 8.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, theme::radius(4), p.bg);
    painter.rect_stroke(rect, theme::radius(4), egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    let head = egui::Rect::from_min_size(rect.min + vec2(4.0, 4.0), vec2(width - 8.0, height));
    painter.rect_filled(head, egui::CornerRadius::ZERO, p.header);
    painter.rect_filled(egui::Rect::from_min_size(head.min, vec2(3.0, height)), egui::CornerRadius::ZERO, p.brand);
    let font = |bold: bool| {
        if bold { theme::bold(size) } else { theme::regular(size) }
    };
    let t = painter.text(
        head.left_center() + vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "CPU",
        theme::bold(size),
        p.accent,
    );
    painter.text(
        t.right_center() + vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "Intel Core i9-14900KF",
        theme::bold(size),
        p.text,
    );
    for (i, (name, cur, min, max)) in rows.iter().enumerate() {
        let r = egui::Rect::from_min_size(head.min + vec2(0.0, height * (i + 1) as f32), vec2(width - 8.0, height));
        if i % 2 == 1 {
            painter.rect_filled(r, egui::CornerRadius::ZERO, p.band);
        }
        painter.text(r.left_center() + vec2(26.0, 0.0), egui::Align2::LEFT_CENTER, *name, font(false), p.text);
        let col = (width - 8.0) / 2.0 / 3.0;
        for (j, (text, current)) in [(*cur, true), (*min, false), (*max, false)].into_iter().enumerate() {
            let x = r.left() + (width - 8.0) / 2.0 + col * (j + 1) as f32 - 8.0;
            let (f, c) = if current { (theme::value_font(size), p.accent) } else { (font(false), p.text) };
            painter.text(egui::pos2(x, r.center().y), egui::Align2::RIGHT_CENTER, text, f, c);
        }
    }
}

fn section(ui: &mut Ui, title: &str) {
    let p = theme::pal(ui.ctx());
    ui.add_space(8.0);
    ui.label(RichText::new(title).font(theme::settings_font(TEXT, true)).color(p.text));
    ui.add_space(2.0);
}

fn hint(ui: &mut Ui, text: &str) {
    let p = theme::pal(ui.ctx());
    ui.label(RichText::new(text).font(theme::settings_font(TEXT - 2.0, false)).color(p.muted));
}

/// The Settings window's own style: Consolas at a fixed size, the palette's colours.
fn settings_style(ui: &mut Ui, palette: &Palette) {
    let style = ui.style_mut();
    style.spacing.item_spacing = vec2(8.0, 6.0);
    style.spacing.button_padding = vec2(8.0, 3.0);
    style.spacing.slider_width = 220.0;
    style.spacing.interact_size.y = 22.0;
    for text_style in [egui::TextStyle::Body, egui::TextStyle::Button, egui::TextStyle::Monospace] {
        style.text_styles.insert(text_style, theme::settings_font(TEXT, false));
    }
    style.text_styles.insert(egui::TextStyle::Heading, theme::settings_font(TEXT + 3.0, true));
    style.text_styles.insert(egui::TextStyle::Small, theme::settings_font(TEXT - 2.0, false));
    let v = &mut style.visuals;
    v.widgets.inactive.bg_fill = palette.hover;
    v.widgets.inactive.weak_bg_fill = palette.hover;
    v.widgets.hovered.bg_fill = palette.border;
    v.widgets.hovered.weak_bg_fill = palette.border;
    v.widgets.active.bg_fill = palette.brand;
    v.selection.bg_fill = palette.brand;
    v.slider_trailing_fill = true;
    v.extreme_bg_color = palette.hover;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, palette.border);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette.brand);
}

#[cfg(test)]
mod tests {
    use super::Page;

    #[test]
    fn pages_are_found_by_name() {
        assert_eq!(Page::from_name("colours"), Some(Page::Colours));
        assert_eq!(Page::from_name("Colors"), Some(Page::Colours));
        assert_eq!(Page::from_name(" TEXT "), Some(Page::Text));
        assert_eq!(Page::from_name("fonts"), None);
    }
}
