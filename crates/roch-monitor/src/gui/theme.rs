//! The palette and type the window draws with, built from the user's [`Appearance`] (Roch GPU's
//! `theme.rs`, which is Roch Viewer's look): by default Consolas 12 px, bold for headings and the
//! live value, a black or white ground, and the Roch red for what moves.
//!
//! The current palette lives in the egui context, and the sizes in a process-wide [`Look`], so
//! every widget reads one set of choices and a change in Settings repaints everything next frame.

use std::cell::Cell;

use eframe::egui::{self, Color32, Context, FontData, FontDefinitions, FontFamily, FontId, Id, Stroke};

use super::appearance::{Appearance, apply_contrast, mix};
use super::fonts;

/// The sizes and switches every drawing function reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub text: f32,
    pub row: f32,
    pub value_width: f32,
    pub bold_headings: bool,
    pub bold_values: bool,
    pub radius: f32,
    pub show_counts: bool,
    pub show_details: bool,
    pub show_summary: bool,
}

impl Look {
    const DEFAULT: Look = Look {
        text: 12.0,
        row: 22.0,
        value_width: 112.0,
        bold_headings: true,
        bold_values: true,
        radius: 4.0,
        show_counts: true,
        show_details: true,
        show_summary: true,
    };

    pub fn from(a: &Appearance) -> Look {
        // Rows grow with the text so it stays clear of the next one, then by the spacing asked for.
        let row = (a.text_size + 10.0 + a.row_spacing).max(a.text_size + 4.0).round();
        Look {
            text: a.text_size,
            row,
            value_width: a.value_width,
            bold_headings: a.bold_headings,
            bold_values: a.bold_values,
            radius: a.corner_radius,
            show_counts: a.show_counts,
            show_details: a.show_details,
            show_summary: a.show_summary,
        }
    }
}

// Per thread: everything is drawn on the UI thread, and tests running side by side each get their
// own, so one test's text size never moves another's rows.
thread_local! {
    static LOOK: Cell<Look> = const { Cell::new(Look::DEFAULT) };
}

pub fn look() -> Look {
    LOOK.with(Cell::get)
}

/// Body text size.
pub fn body() -> f32 {
    look().text
}

/// Height of every table row.
pub fn row() -> f32 {
    look().row
}

pub fn radius(base: u8) -> egui::CornerRadius {
    egui::CornerRadius::same((base as f32 * look().radius / 4.0).round().clamp(0.0, 255.0) as u8)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub header: Color32,
    /// Every other row; the background itself when shading is off.
    pub band: Color32,
    pub hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub brand: Color32,
    pub brand_hover: Color32,
    /// The live value.
    pub accent: Color32,
    pub warn: Color32,
    pub ok: Color32,
    pub grid: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    pub fn from_appearance(a: &Appearance, dark: bool) -> Palette {
        let c = a.colors(dark);
        let bg = c.background;
        Palette {
            dark,
            bg,
            panel: mix(bg, c.text, 0.04),
            header: c.heading,
            band: if a.row_shading { c.shading } else { bg },
            hover: c.hover,
            border: c.border,
            text: apply_contrast(c.text, bg, a.contrast),
            muted: apply_contrast(c.dimmed, bg, a.contrast),
            brand: c.accent,
            brand_hover: c.accent_hover,
            accent: apply_contrast(c.value, bg, a.contrast),
            warn: if dark { rgb(0xFFB020) } else { rgb(0xB45309) },
            ok: if dark { rgb(0x51CF66) } else { rgb(0x2F9E44) },
            grid: mix(bg, c.text, if dark { 0.15 } else { 0.11 }),
        }
    }

    /// Graph series colours, distinct on black and on white. The first is the value colour.
    pub fn series(&self, i: usize) -> Color32 {
        const DARK: [u32; 7] = [0x22B8CF, 0xFAB005, 0x51CF66, 0x9775FA, 0xF06595, 0x4DABF7, 0xC0EB75];
        const LIGHT: [u32; 7] = [0x0C8599, 0xE67700, 0x2F9E44, 0x6741D9, 0xC2255C, 0x1971C2, 0x5C940D];
        if i.is_multiple_of(8) {
            return self.accent;
        }
        rgb(if self.dark { DARK[i % 8 - 1] } else { LIGHT[i % 8 - 1] })
    }
}

fn palette_id() -> Id {
    Id::unique("roch_monitor_palette")
}

pub fn pal(ctx: &Context) -> Palette {
    ctx.data(|d| d.get_temp::<Palette>(palette_id()))
        .unwrap_or_else(|| Palette::from_appearance(&Appearance::default(), true))
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// The heading face: bold, unless bold headings are turned off.
pub fn bold(size: f32) -> FontId {
    if look().bold_headings { FontId::new(size, FontFamily::Name("bold".into())) } else { regular(size) }
}

/// The Current column's face.
pub fn value_font(size: f32) -> FontId {
    if look().bold_values { FontId::new(size, FontFamily::Name("bold".into())) } else { regular(size) }
}

/// The Settings window always draws in Consolas, whatever the table uses, so a font that cannot
/// show the controls never makes the controls unreadable.
pub fn settings_font(size: f32, bold: bool) -> FontId {
    FontId::new(size, FontFamily::Name(if bold { "settings-bold" } else { "settings" }.into()))
}

/// Load the chosen family (its regular face, and its bold face for headings when it has one), with
/// Consolas, Segoe UI Symbol (for ▸ ▾ ● ·) and egui's own fonts behind it. A file that cannot be
/// read is skipped; the window always has something to draw with.
pub fn install_fonts(ctx: &Context, family: &str) {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    let mut load = |key: &str, path: std::path::PathBuf| -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        fonts.font_data.insert(key.to_owned(), std::sync::Arc::new(FontData::from_owned(bytes)));
        Some(key.to_owned())
    };
    let consolas = load("consolas", dir.join("Fonts").join("consola.ttf"));
    let consolas_bold = load("consolas-bold", dir.join("Fonts").join("consolab.ttf"));
    let symbols = load("segoe-symbol", dir.join("Fonts").join("seguisym.ttf"));
    let (chosen, chosen_bold) = if family.eq_ignore_ascii_case(fonts::DEFAULT_FAMILY) {
        (None, None)
    } else {
        match fonts::find_family(family) {
            Some(f) => {
                let regular = load("chosen", f.regular.clone());
                let bold = f.bold.clone().and_then(|b| load("chosen-bold", b));
                (regular, bold)
            }
            None => (None, None),
        }
    };
    let defaults: Vec<String> =
        FontDefinitions::default().families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let chain = |first: Vec<Option<String>>| -> Vec<String> {
        first.into_iter().flatten().chain(symbols.clone()).chain(defaults.iter().cloned()).collect()
    };
    let regular = chain(vec![chosen.clone(), consolas.clone()]);
    // A chosen family without a bold face uses its regular one, rather than Consolas Bold beside it.
    let bold = chain(vec![chosen_bold, chosen.clone(), consolas_bold.clone(), consolas.clone()]);
    fonts.families.insert(FontFamily::Proportional, regular.clone());
    fonts.families.insert(FontFamily::Monospace, regular);
    fonts.families.insert(FontFamily::Name("bold".into()), bold);
    fonts.families.insert(FontFamily::Name("settings".into()), chain(vec![consolas.clone()]));
    fonts.families.insert(FontFamily::Name("settings-bold".into()), chain(vec![consolas_bold, consolas]));
    ctx.set_fonts(fonts);
}

/// Store the palette and the look, and rebuild egui's visuals from them.
pub fn apply(ctx: &Context, a: &Appearance, dark: bool) {
    LOOK.with(|l| l.set(Look::from(a)));
    let p = Palette::from_appearance(a, dark);
    ctx.data_mut(|d| d.insert_temp(palette_id(), p));
    let mut style = (*ctx.global_style()).clone();
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.extreme_bg_color = p.panel;
    v.faint_bg_color = p.band;
    v.hyperlink_color = p.brand;
    v.selection.bg_fill = p.brand;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.text_cursor.stroke = Stroke::new(1.5, p.text);
    v.popup_shadow = egui::Shadow::NONE;
    v.window_shadow = egui::Shadow::NONE;
    v.slider_trailing_fill = true;
    let corner = radius(3);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.bg_fill = p.panel;
        w.weak_bg_fill = p.panel;
        w.bg_stroke = Stroke::new(1.0, p.border);
        w.fg_stroke = Stroke::new(1.0, p.text);
        w.corner_radius = corner;
        w.expansion = 0.0;
    }
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.brand_hover);
    v.widgets.active.bg_fill = p.brand;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.brand);
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.brand);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    style.visuals = v;
    let size = a.text_size;
    for (ts, font) in [
        (egui::TextStyle::Body, regular(size)),
        (egui::TextStyle::Button, regular(size)),
        (egui::TextStyle::Monospace, regular(size)),
        (egui::TextStyle::Small, regular((size - 1.5).max(8.0))),
        (egui::TextStyle::Heading, bold(size + 3.0)),
    ] {
        style.text_styles.insert(ts, font);
    }
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size.y = (size + 10.0).max(20.0);
    style.interaction.tooltip_delay = 0.35;
    ctx.set_style_of(egui::Theme::Light, style.clone());
    ctx.set_style_of(egui::Theme::Dark, style);
    ctx.set_theme(if dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::appearance::{Colors, presets};

    #[test]
    fn the_default_palette_is_the_roch_one() {
        let p = Palette::from_appearance(&Appearance::default(), true);
        assert_eq!(p.bg, Color32::BLACK);
        assert_eq!(p.accent, Colors::DEFAULT_DARK.value);
        assert_eq!(p.header, Colors::DEFAULT_DARK.heading);
        let p = Palette::from_appearance(&Appearance::default(), false);
        assert_eq!(p.bg, Color32::WHITE);
        assert_eq!(p.accent, Colors::DEFAULT_LIGHT.value);
    }

    #[test]
    fn shading_off_bands_in_the_background() {
        let a = Appearance { row_shading: false, ..Default::default() };
        let p = Palette::from_appearance(&a, true);
        assert_eq!(p.band, p.bg);
    }

    #[test]
    fn presets_and_contrast_reach_the_palette() {
        let ocean = presets().into_iter().find(|p| p.name == "Ocean").unwrap();
        let a = Appearance { dark: ocean.dark, contrast: 1.0, ..Default::default() };
        let p = Palette::from_appearance(&a, true);
        assert_eq!(p.bg, ocean.dark.background);
        assert_eq!(p.text, Color32::WHITE, "full contrast pushes text to white on a dark ground");
    }

    #[test]
    fn rows_grow_with_the_text() {
        let small = Look::from(&Appearance::default());
        let big = Look::from(&Appearance { text_size: 18.0, ..Default::default() });
        let tight = Look::from(&Appearance { row_spacing: -3.0, ..Default::default() });
        assert_eq!(small.row, 22.0, "12 px text keeps Roch GPU's 22 px rows");
        assert!(big.row > small.row);
        assert!(tight.row < small.row && tight.row >= 16.0);
    }
}
