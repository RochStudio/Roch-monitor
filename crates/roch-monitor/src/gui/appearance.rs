//! What the window looks like, as the user set it in Settings: Roch GPU's appearance preferences
//! (`preferences.rs`), which share Roch Viewer's palette, with the monitor's own layout choices.
//!
//! Saved inside settings.json under "appearance". Every field has a default and a sane range, so a
//! hand-edited or older file loses only the values it got wrong.

use eframe::egui::Color32;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Light, Dark, or whatever Windows is set to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
    System,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System];

    pub fn name(self) -> &'static str {
        match self {
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
            ThemeMode::System => "Follow Windows",
        }
    }

    pub fn is_dark(self, system_dark: bool) -> bool {
        match self {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => system_dark,
        }
    }
}

/// `#RRGGBB`.
pub fn hex(color: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
}

/// `#RRGGBB` or `RRGGBB`, any case.
pub fn parse_hex(text: &str) -> Option<Color32> {
    let digits = text.trim().trim_start_matches('#');
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok().map(rgb)
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The colours one mode is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    /// Window background.
    pub background: Color32,
    /// Sensor names, headings, ordinary text.
    pub text: Color32,
    /// The live value in the Current column.
    pub value: Color32,
    /// Device tags, the bar beside each heading, selections: the brand colour.
    pub accent: Color32,
    /// The accent under the pointer.
    pub accent_hover: Color32,
    /// Panel and button outlines, the table's rules.
    pub border: Color32,
    /// Group headings, device details, secondary text.
    pub dimmed: Color32,
    /// Every other sensor row, when row shading is on.
    pub shading: Color32,
    /// A row or button under the pointer.
    pub hover: Color32,
    /// The band behind each device heading.
    pub heading: Color32,
}

/// The editable colour roles, in the order the Settings window lists them.
pub const COLOR_ROLES: &[(&str, &str)] = &[
    ("background", "Background"),
    ("text", "Text"),
    ("value", "Current value"),
    ("accent", "Accent"),
    ("accent_hover", "Accent (hover)"),
    ("border", "Borders"),
    ("dimmed", "Dimmed text"),
    ("shading", "Row shading"),
    ("hover", "Hover"),
    ("heading", "Device headings"),
];

/// Mix `a` toward `b` by `t`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

impl Colors {
    /// Roch Viewer's and Roch GPU's light theme.
    pub const DEFAULT_LIGHT: Colors = Colors {
        background: rgb(0xFFFFFF),
        text: rgb(0x000000),
        value: rgb(0xB91C1C),
        accent: rgb(0xD0343A),
        accent_hover: rgb(0xE0383E),
        border: rgb(0xD0D0D0),
        dimmed: rgb(0x62666D),
        shading: rgb(0xF5F5F5),
        hover: rgb(0xE6E6E6),
        heading: rgb(0xEFEFEF),
    };

    /// Roch Viewer's and Roch GPU's dark theme.
    pub const DEFAULT_DARK: Colors = Colors {
        background: rgb(0x000000),
        text: rgb(0xFFFFFF),
        value: rgb(0xFF5A5F),
        accent: rgb(0xD0343A),
        accent_hover: rgb(0xE0383E),
        border: rgb(0x383838),
        dimmed: rgb(0xA0A0A0),
        shading: rgb(0x101010),
        hover: rgb(0x222222),
        heading: rgb(0x161616),
    };

    pub fn get(&self, role: &str) -> Color32 {
        match role {
            "background" => self.background,
            "text" => self.text,
            "value" => self.value,
            "accent" => self.accent,
            "accent_hover" => self.accent_hover,
            "border" => self.border,
            "dimmed" => self.dimmed,
            "shading" => self.shading,
            "heading" => self.heading,
            _ => self.hover,
        }
    }

    pub fn set(&mut self, role: &str, color: Color32) {
        let slot = match role {
            "background" => &mut self.background,
            "text" => &mut self.text,
            "value" => &mut self.value,
            "accent" => &mut self.accent,
            "accent_hover" => &mut self.accent_hover,
            "border" => &mut self.border,
            "dimmed" => &mut self.dimmed,
            "shading" => &mut self.shading,
            "heading" => &mut self.heading,
            _ => &mut self.hover,
        };
        *slot = color;
    }
}

/// Stored as an object of role -> "#RRGGBB"; a role that is missing or malformed keeps its default.
#[derive(Serialize, Deserialize, Default)]
struct ColorsFile(std::collections::BTreeMap<String, String>);

fn colors_to_file(c: &Colors) -> ColorsFile {
    ColorsFile(COLOR_ROLES.iter().map(|(role, _)| (role.to_string(), hex(c.get(role)))).collect())
}

fn colors_from_file(file: ColorsFile, fallback: Colors) -> Colors {
    let mut c = fallback;
    for (role, _) in COLOR_ROLES {
        if let Some(color) = file.0.get(*role).and_then(|t| parse_hex(t)) {
            c.set(role, color);
        }
    }
    c
}

/// A named pair of colour sets, one per mode. Roch GPU's presets, so the two tools can match.
pub struct Preset {
    pub name: &'static str,
    pub light: Colors,
    pub dark: Colors,
}

/// Starting points for the colours; every role can be changed after.
pub fn presets() -> Vec<Preset> {
    // A preset whose heading band is not given gets one a step off its background.
    let with_heading = |c: Colors| Colors { heading: mix(c.background, c.text, 0.08), ..c };
    let tint = |base: Colors, accent: u32, hover: u32, value: u32| Colors {
        accent: rgb(accent),
        accent_hover: rgb(hover),
        value: rgb(value),
        ..base
    };
    vec![
        Preset { name: "Roch (default)", light: Colors::DEFAULT_LIGHT, dark: Colors::DEFAULT_DARK },
        Preset {
            name: "High contrast",
            light: with_heading(Colors {
                background: rgb(0xFFFFFF),
                text: rgb(0x000000),
                value: rgb(0x8B0000),
                accent: rgb(0x0000C0),
                accent_hover: rgb(0x0000FF),
                border: rgb(0x000000),
                dimmed: rgb(0x404040),
                shading: rgb(0xE8E8E8),
                hover: rgb(0xD0D0D0),
                heading: rgb(0),
            }),
            dark: with_heading(Colors {
                background: rgb(0x000000),
                text: rgb(0xFFFFFF),
                value: rgb(0xFFFF00),
                accent: rgb(0x00FFFF),
                accent_hover: rgb(0x80FFFF),
                border: rgb(0xFFFFFF),
                dimmed: rgb(0xC0C0C0),
                shading: rgb(0x202020),
                hover: rgb(0x303030),
                heading: rgb(0),
            }),
        },
        Preset {
            name: "Graphite",
            light: with_heading(Colors {
                background: rgb(0xF3F3F3),
                text: rgb(0x1E1E1E),
                value: rgb(0x9E2A2B),
                accent: rgb(0x4A4A4A),
                accent_hover: rgb(0x5E5E5E),
                border: rgb(0xC8C8C8),
                dimmed: rgb(0x6E6E6E),
                shading: rgb(0xE9E9E9),
                hover: rgb(0xDDDDDD),
                heading: rgb(0),
            }),
            dark: with_heading(Colors {
                background: rgb(0x1E1E1E),
                text: rgb(0xE6E6E6),
                value: rgb(0xFF7B72),
                accent: rgb(0x5A5A5A),
                accent_hover: rgb(0x6E6E6E),
                border: rgb(0x3C3C3C),
                dimmed: rgb(0x9A9A9A),
                shading: rgb(0x262626),
                hover: rgb(0x2E2E2E),
                heading: rgb(0),
            }),
        },
        Preset {
            name: "Ocean",
            light: with_heading(tint(Colors::DEFAULT_LIGHT, 0x1F6FEB, 0x388BFD, 0x0B5CAD)),
            dark: with_heading(Colors {
                background: rgb(0x0D1117),
                shading: rgb(0x161B22),
                hover: rgb(0x21262D),
                border: rgb(0x30363D),
                ..tint(Colors::DEFAULT_DARK, 0x1F6FEB, 0x388BFD, 0x58A6FF)
            }),
        },
        Preset {
            name: "Forest",
            light: with_heading(tint(Colors::DEFAULT_LIGHT, 0x2E7D32, 0x388E3C, 0x1B5E20)),
            dark: with_heading(Colors {
                background: rgb(0x0B140D),
                shading: rgb(0x122016),
                hover: rgb(0x1B2C1F),
                border: rgb(0x2A3D2E),
                ..tint(Colors::DEFAULT_DARK, 0x2E7D32, 0x43A047, 0x7EE787)
            }),
        },
        Preset {
            name: "Amber",
            light: with_heading(tint(Colors::DEFAULT_LIGHT, 0xB45309, 0xD97706, 0x92400E)),
            dark: with_heading(Colors {
                background: rgb(0x120D05),
                shading: rgb(0x1C140A),
                hover: rgb(0x2A1E0E),
                border: rgb(0x3A2A14),
                ..tint(Colors::DEFAULT_DARK, 0xB45309, 0xD97706, 0xFFB000)
            }),
        },
        Preset {
            name: "Purple",
            light: with_heading(tint(Colors::DEFAULT_LIGHT, 0x7C3AED, 0x8B5CF6, 0x6D28D9)),
            dark: with_heading(Colors {
                background: rgb(0x0F0B16),
                shading: rgb(0x17111F),
                hover: rgb(0x221A2E),
                border: rgb(0x32284A),
                ..tint(Colors::DEFAULT_DARK, 0x7C3AED, 0x8B5CF6, 0xC4B5FD)
            }),
        },
    ]
}

/// Pull a colour away from (or toward) the background: Roch GPU's contrast slider.
pub fn apply_contrast(fg: Color32, bg: Color32, amount: f32) -> Color32 {
    if amount == 0.0 {
        return fg;
    }
    let luminance = |c: Color32| 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
    let target = if amount < 0.0 {
        bg
    } else if luminance(bg) > 127.5 {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    let t = amount.abs().min(1.0) * if amount < 0.0 { 0.65 } else { 1.0 };
    mix(fg, target, t)
}

/// Every appearance choice the Settings window offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    pub theme: ThemeMode,
    pub light: Colors,
    pub dark: Colors,
    /// -1 softens text toward the background, +1 pushes it to black or white.
    pub contrast: f32,
    pub row_shading: bool,
    pub corner_radius: f32,
    pub font_family: String,
    /// Body text size in px; 12 is Roch Viewer's compact size.
    pub text_size: f32,
    pub bold_headings: bool,
    pub bold_values: bool,
    pub ui_scale: f32,
    /// Extra height for every table row; below 0 packs them tighter.
    pub row_spacing: f32,
    /// Width of each of the four value columns.
    pub value_width: f32,
    pub group_headings: bool,
    pub show_counts: bool,
    pub show_details: bool,
    pub show_summary: bool,
    pub always_on_top: bool,
    pub opacity: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            light: Colors::DEFAULT_LIGHT,
            dark: Colors::DEFAULT_DARK,
            contrast: 0.0,
            row_shading: true,
            corner_radius: 4.0,
            font_family: crate::gui::fonts::DEFAULT_FAMILY.into(),
            text_size: 12.0,
            bold_headings: true,
            bold_values: true,
            ui_scale: 1.0,
            row_spacing: 0.0,
            value_width: 112.0,
            group_headings: true,
            show_counts: true,
            show_details: true,
            show_summary: true,
            always_on_top: false,
            opacity: 1.0,
        }
    }
}

pub const TEXT_SIZE: (f32, f32) = (9.0, 20.0);
pub const ROW_SPACING: (f32, f32) = (-3.0, 12.0);
pub const VALUE_WIDTH: (f32, f32) = (80.0, 200.0);
pub const UI_SCALE: (f32, f32) = (0.75, 2.0);
pub const OPACITY: (f32, f32) = (0.3, 1.0);
pub const CORNER: (f32, f32) = (0.0, 14.0);

/// The file form: plain fields, each optional, so one bad value cannot cost the rest.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct AppearanceFile {
    theme: Option<ThemeMode>,
    light: Option<ColorsFile>,
    dark: Option<ColorsFile>,
    contrast: Option<f32>,
    row_shading: Option<bool>,
    corner_radius: Option<f32>,
    font_family: Option<String>,
    text_size: Option<f32>,
    bold_headings: Option<bool>,
    bold_values: Option<bool>,
    ui_scale: Option<f32>,
    row_spacing: Option<f32>,
    value_width: Option<f32>,
    group_headings: Option<bool>,
    show_counts: Option<bool>,
    show_details: Option<bool>,
    show_summary: Option<bool>,
    always_on_top: Option<bool>,
    opacity: Option<f32>,
}

fn clamp(v: Option<f32>, default: f32, (lo, hi): (f32, f32)) -> f32 {
    v.filter(|v| v.is_finite()).map_or(default, |v| v.clamp(lo, hi))
}

impl Serialize for Appearance {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        AppearanceFile {
            theme: Some(self.theme),
            light: Some(colors_to_file(&self.light)),
            dark: Some(colors_to_file(&self.dark)),
            contrast: Some(self.contrast),
            row_shading: Some(self.row_shading),
            corner_radius: Some(self.corner_radius),
            font_family: Some(self.font_family.clone()),
            text_size: Some(self.text_size),
            bold_headings: Some(self.bold_headings),
            bold_values: Some(self.bold_values),
            ui_scale: Some(self.ui_scale),
            row_spacing: Some(self.row_spacing),
            value_width: Some(self.value_width),
            group_headings: Some(self.group_headings),
            show_counts: Some(self.show_counts),
            show_details: Some(self.show_details),
            show_summary: Some(self.show_summary),
            always_on_top: Some(self.always_on_top),
            opacity: Some(self.opacity),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Appearance {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Read leniently: anything that is not the expected shape becomes "not given".
        let value = serde_json::Value::deserialize(d)?;
        let map = value.as_object().cloned().unwrap_or_default();
        let get = |k: &str| map.get(k).cloned().unwrap_or(serde_json::Value::Null);
        let f = |k: &str| get(k).as_f64().map(|v| v as f32);
        let b = |k: &str| get(k).as_bool();
        let colors = |k: &str| serde_json::from_value::<ColorsFile>(get(k)).ok();
        let file = AppearanceFile {
            theme: serde_json::from_value(get("theme")).ok(),
            light: colors("light"),
            dark: colors("dark"),
            contrast: f("contrast"),
            row_shading: b("row_shading"),
            corner_radius: f("corner_radius"),
            font_family: get("font_family").as_str().map(str::to_owned),
            text_size: f("text_size"),
            bold_headings: b("bold_headings"),
            bold_values: b("bold_values"),
            ui_scale: f("ui_scale"),
            row_spacing: f("row_spacing"),
            value_width: f("value_width"),
            group_headings: b("group_headings"),
            show_counts: b("show_counts"),
            show_details: b("show_details"),
            show_summary: b("show_summary"),
            always_on_top: b("always_on_top"),
            opacity: f("opacity"),
        };
        let d = Appearance::default();
        Ok(Appearance {
            theme: file.theme.unwrap_or(d.theme),
            light: file.light.map_or(d.light, |c| colors_from_file(c, Colors::DEFAULT_LIGHT)),
            dark: file.dark.map_or(d.dark, |c| colors_from_file(c, Colors::DEFAULT_DARK)),
            contrast: clamp(file.contrast, d.contrast, (-1.0, 1.0)),
            row_shading: file.row_shading.unwrap_or(d.row_shading),
            corner_radius: clamp(file.corner_radius, d.corner_radius, CORNER),
            font_family: file
                .font_family
                .map(|s| s.trim().chars().take(128).collect::<String>())
                .filter(|s| !s.is_empty())
                .unwrap_or(d.font_family),
            text_size: clamp(file.text_size, d.text_size, TEXT_SIZE),
            bold_headings: file.bold_headings.unwrap_or(d.bold_headings),
            bold_values: file.bold_values.unwrap_or(d.bold_values),
            ui_scale: clamp(file.ui_scale, d.ui_scale, UI_SCALE),
            row_spacing: clamp(file.row_spacing, d.row_spacing, ROW_SPACING),
            value_width: clamp(file.value_width, d.value_width, VALUE_WIDTH),
            group_headings: file.group_headings.unwrap_or(d.group_headings),
            show_counts: file.show_counts.unwrap_or(d.show_counts),
            show_details: file.show_details.unwrap_or(d.show_details),
            show_summary: file.show_summary.unwrap_or(d.show_summary),
            always_on_top: file.always_on_top.unwrap_or(d.always_on_top),
            opacity: clamp(file.opacity, d.opacity, OPACITY),
        })
    }
}

impl Appearance {
    pub fn colors(&self, dark: bool) -> &Colors {
        if dark { &self.dark } else { &self.light }
    }

    pub fn colors_mut(&mut self, dark: bool) -> &mut Colors {
        if dark { &mut self.dark } else { &mut self.light }
    }

    /// The preset these colours came from, if they still match one.
    pub fn preset_name(&self) -> Option<&'static str> {
        presets().into_iter().find(|p| p.light == self.light && p.dark == self.dark).map(|p| p.name)
    }
}

/// Whether Windows' apps are set to dark (Settings > Personalisation > Colours).
#[cfg(windows)]
pub fn system_dark() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            winreg::enums::KEY_READ,
        )
        .ok()
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme").ok())
        .is_some_and(|v| v == 0)
}

#[cfg(not(windows))]
pub fn system_dark() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_customised_appearance_survives_a_restart() {
        let mut a = Appearance {
            theme: ThemeMode::System,
            font_family: "Cascadia Mono".into(),
            text_size: 14.5,
            ..Default::default()
        };
        a.dark.value = Color32::from_rgb(3, 29, 84);
        a.light.heading = Color32::from_rgb(200, 210, 220);
        a.opacity = 0.8;
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"#031D54\""));
        let back: Appearance = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn bad_values_cost_only_themselves() {
        let a: Appearance = serde_json::from_str(
            r##"{"theme":"purple","text_size":999,"opacity":-5,"ui_scale":"big","row_spacing":3,
                "dark":{"value":"#00FF00","text":"nonsense"},"font_family":"   "}"##,
        )
        .unwrap();
        let d = Appearance::default();
        assert_eq!(a.theme, d.theme, "an unknown theme keeps the default");
        assert_eq!(a.text_size, TEXT_SIZE.1, "clamped, not discarded");
        assert_eq!(a.opacity, OPACITY.0);
        assert_eq!(a.ui_scale, 1.0);
        assert_eq!(a.row_spacing, 3.0);
        assert_eq!(a.dark.value, Color32::from_rgb(0, 255, 0));
        assert_eq!(a.dark.text, Colors::DEFAULT_DARK.text);
        assert_eq!(a.font_family, "Consolas");
    }

    #[test]
    fn hex_reads_both_spellings() {
        assert_eq!(parse_hex("#d0343a"), Some(Color32::from_rgb(0xD0, 0x34, 0x3A)));
        assert_eq!(parse_hex("D0343A"), Some(Color32::from_rgb(0xD0, 0x34, 0x3A)));
        assert_eq!(parse_hex("#D0343"), None);
        assert_eq!(hex(Color32::from_rgb(1, 2, 255)), "#0102FF");
    }

    #[test]
    fn the_default_is_the_roch_preset_and_every_preset_has_a_heading_band() {
        assert_eq!(Appearance::default().preset_name(), Some("Roch (default)"));
        for p in presets() {
            assert_ne!(p.dark.heading, p.dark.background, "{}", p.name);
            assert_ne!(p.light.heading, p.light.background, "{}", p.name);
        }
    }

    #[test]
    fn contrast_moves_toward_the_far_end() {
        let bg = Color32::BLACK;
        let fg = Color32::from_gray(160);
        assert_eq!(apply_contrast(fg, bg, 0.0), fg);
        assert!(apply_contrast(fg, bg, 1.0).r() > 250);
        assert!(apply_contrast(fg, bg, -1.0).r() < 160);
    }
}
