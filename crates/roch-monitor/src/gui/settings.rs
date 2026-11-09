//! What the window remembers between runs, in %APPDATA%\RochMonitor\settings.json.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::appearance::Appearance;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Colours, font, sizes and layout: everything on the Settings window's appearance pages.
    pub appearance: Appearance,
    pub interval_ms: u64,
    /// Sensor keys hidden from the list (and from CSV logs).
    pub hidden: Vec<String>,
    /// Device ids folded away.
    pub collapsed: Vec<String>,
    /// Parent rows opened to show their per-thread children.
    pub expanded: Vec<String>,
    /// Sensor keys drawn in the graph panel.
    pub graphs: Vec<String>,
    /// Seconds of history the graphs show.
    pub graph_seconds: u32,
    pub graph_height: f32,
    /// Read the DDR5 PMIC's ADC (a selector write per channel, restored afterwards).
    pub pmic_adc: bool,
    pub log_directory: Option<String>,
    pub window_size: Option<[f32; 2]>,
    /// Sensor keys shown in the tray, each as its own icon with its value drawn in (HWiNFO's "Show
    /// in tray"), and listed in the Roch icon's tooltip.
    pub tray_sensors: Vec<String>,
    /// Draw each tray sensor as an icon of its own; off lists them in the Roch icon's tooltip only.
    pub tray_icons: bool,
    /// Minimising hides the window in the tray.
    pub minimize_to_tray: bool,
    /// The close button hides the window in the tray instead of exiting; Exit is on the tray menu.
    pub close_to_tray: bool,
}

/// How many sensors can be in the tray at once: each is an icon of its own.
pub const MAX_TRAY_SENSORS: usize = 6;

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            interval_ms: 1000,
            hidden: Vec::new(),
            collapsed: Vec::new(),
            expanded: Vec::new(),
            graphs: Vec::new(),
            graph_seconds: 300,
            graph_height: 240.0,
            pmic_adc: true,
            log_directory: None,
            window_size: None,
            tray_sensors: Vec::new(),
            tray_icons: true,
            minimize_to_tray: true,
            close_to_tray: false,
        }
    }
}

pub fn directory() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("RochMonitor")
}

fn path() -> PathBuf {
    directory().join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read_to_string(path()).ok().and_then(|s| Self::parse(&s)).unwrap_or_default()
    }

    /// Tolerates the byte-order mark Notepad and PowerShell put on a UTF-8 file they have edited.
    pub fn parse(text: &str) -> Option<Settings> {
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(directory());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            // Written beside and renamed over, so a crash mid-write cannot leave half a file.
            let tmp = path().with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, path());
            }
        }
    }

    pub fn toggle(list: &mut Vec<String>, key: &str) {
        if let Some(i) = list.iter().position(|k| k == key) {
            list.remove(i);
        } else {
            list.push(key.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_file_keeps_the_defaults_for_what_it_lacks() {
        let s: Settings = serde_json::from_str(r#"{"appearance": {"theme": "light"}, "graph_seconds": 60}"#).unwrap();
        assert_eq!(s.appearance.theme, super::super::appearance::ThemeMode::Light);
        assert_eq!(s.graph_seconds, 60);
        assert_eq!(s.interval_ms, 1000);
        assert!(s.appearance.group_headings);
    }

    #[test]
    fn appearance_round_trips_through_the_file() {
        let mut s = Settings::default();
        s.appearance.font_family = "Cascadia Mono".into();
        s.appearance.text_size = 15.0;
        let back = Settings::parse(&serde_json::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn a_byte_order_mark_is_not_a_reason_to_forget_everything() {
        let s = Settings::parse("\u{feff}{\"interval_ms\": 2000}").unwrap();
        assert_eq!(s.interval_ms, 2000);
    }

    #[test]
    fn toggling_adds_then_removes() {
        let mut v = Vec::new();
        Settings::toggle(&mut v, "a");
        assert_eq!(v, vec!["a"]);
        Settings::toggle(&mut v, "a");
        assert!(v.is_empty());
    }
}
