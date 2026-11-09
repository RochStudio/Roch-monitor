//! The notification area (Roch GPU's `tray.rs`, grown into HWiNFO's tray): the Roch icon, whose
//! tooltip lists the readings chosen for the tray, and one more icon per chosen sensor that draws
//! its live value as digits, the way HWiNFO puts a sensor in the tray.
//!
//! Events arrive on whatever thread the shell uses; they go into a channel and the UI thread is
//! woken to handle them in `App::logic`, which keeps running while the window is hidden.

use std::sync::mpsc::Receiver;

use eframe::egui::{Color32, Context};

/// What the tray asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Show,
    Settings,
    ResetStats,
    Exit,
}

/// One sensor shown in the tray: the digits drawn in its icon and the line its tooltip carries.
#[derive(Debug, Clone, PartialEq)]
pub struct TrayValue {
    pub key: String,
    pub digits: String,
    pub tooltip: String,
}

/// The shell truncates a notification tooltip at 127 characters.
pub const TOOLTIP_MAX: usize = 127;

pub fn clip_tooltip(text: &str) -> String {
    if text.chars().count() <= TOOLTIP_MAX {
        text.to_owned()
    } else {
        text.chars().take(TOOLTIP_MAX - 1).collect::<String>() + "…"
    }
}

/// A reading as the few characters a 16 px icon has room for: whole numbers up to 999, two
/// decimals under 10 for a unit that needs them (volts), and thousands as "5.7k" or "13k".
pub fn icon_digits(value: f64, unit: &str) -> String {
    if !value.is_finite() {
        return "-".into();
    }
    let a = value.abs();
    let sign = if value < 0.0 { "-" } else { "" };
    if a >= 10_000.0 {
        format!("{sign}{:.0}k", a / 1000.0)
    } else if a >= 999.5 {
        format!("{sign}{:.1}k", a / 1000.0)
    } else if a < 10.0 && matches!(unit, "V" | "A") {
        format!("{sign}{a:.2}")
    } else if a < 10.0 && unit == "W" {
        format!("{sign}{a:.1}")
    } else {
        format!("{sign}{a:.0}")
    }
}

/// A 4 x 7 pixel font: one row per u8, the low four bits from the left.
fn glyph(c: char) -> Option<(u32, [u8; 7])> {
    Some(match c {
        '0' => (4, [0b0110, 0b1001, 0b1011, 0b1101, 0b1001, 0b1001, 0b0110]),
        '1' => (4, [0b0010, 0b0110, 0b0010, 0b0010, 0b0010, 0b0010, 0b0111]),
        '2' => (4, [0b0110, 0b1001, 0b0001, 0b0010, 0b0100, 0b1000, 0b1111]),
        '3' => (4, [0b1110, 0b0001, 0b0001, 0b0110, 0b0001, 0b0001, 0b1110]),
        '4' => (4, [0b0010, 0b0110, 0b1010, 0b1010, 0b1111, 0b0010, 0b0010]),
        '5' => (4, [0b1111, 0b1000, 0b1110, 0b0001, 0b0001, 0b1001, 0b0110]),
        '6' => (4, [0b0110, 0b1000, 0b1000, 0b1110, 0b1001, 0b1001, 0b0110]),
        '7' => (4, [0b1111, 0b0001, 0b0010, 0b0010, 0b0100, 0b0100, 0b0100]),
        '8' => (4, [0b0110, 0b1001, 0b1001, 0b0110, 0b1001, 0b1001, 0b0110]),
        '9' => (4, [0b0110, 0b1001, 0b1001, 0b0111, 0b0001, 0b0001, 0b0110]),
        'k' => (4, [0b1000, 0b1000, 0b1001, 0b1010, 0b1100, 0b1010, 0b1001]),
        '-' => (3, [0b000, 0b000, 0b000, 0b111, 0b000, 0b000, 0b000]),
        '.' => (1, [0, 0, 0, 0, 0, 0, 1]),
        _ => return None,
    })
}

/// Draw `text` centred in a `size` x `size` RGBA square: `fg` digits on a `bg` ground, the font
/// scaled up by whole pixels for a larger icon so the digits stay crisp.
pub fn render_digits(text: &str, size: u32, fg: Color32, bg: Color32) -> Vec<u8> {
    let size = size.max(16);
    let glyphs: Vec<(u32, [u8; 7])> = text.chars().filter_map(glyph).collect();
    let natural: u32 = glyphs.iter().map(|g| g.0).sum::<u32>() + glyphs.len().saturating_sub(1) as u32;
    let scale = (size / 16).max(1);
    let scale = if natural * scale > size { 1 } else { scale };
    let width = natural * scale;
    let height = 7 * scale;
    let x0 = size.saturating_sub(width) / 2;
    let y0 = size.saturating_sub(height) / 2;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..size * size {
        rgba.extend_from_slice(&[bg.r(), bg.g(), bg.b(), bg.a()]);
    }
    let mut x = x0;
    for (w, rows) in glyphs {
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..w {
                if bits >> (w - 1 - col) & 1 == 1 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let (px, py) = (x + col * scale + dx, y0 + row as u32 * scale + dy);
                            if px < size && py < size {
                                let i = ((py * size + px) * 4) as usize;
                                rgba[i..i + 4].copy_from_slice(&[fg.r(), fg.g(), fg.b(), fg.a()]);
                            }
                        }
                    }
                }
            }
        }
        x += (w + 1) * scale;
    }
    rgba
}

#[cfg(windows)]
mod imp {
    use std::sync::mpsc::{Sender, channel};

    use super::*;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    struct ValueIcon {
        key: String,
        icon: TrayIcon,
        drawn: (String, Color32, Color32),
        tooltip: String,
    }

    pub struct Tray {
        main: TrayIcon,
        values: Vec<ValueIcon>,
        pub commands: Receiver<TrayCommand>,
        tooltip: String,
        icon_size: u32,
    }

    /// The small-icon size the shell draws the notification area at (16 px at 100%).
    fn small_icon_size() -> u32 {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
        // SAFETY: a metrics query.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
        size.clamp(16, 64) as u32
    }

    /// The 16 px frame of the embedded .ico: hand-tuned for the notification area.
    fn roch_icon() -> Option<Icon> {
        let (rgba, w, h) = crate::gui::ico_frame_rgba(crate::gui::ICO, 16)?;
        Icon::from_rgba(rgba, w, h).ok()
    }

    impl Tray {
        /// Create the Roch icon. `None` when the shell refuses, and the window then minimises as
        /// usual.
        pub fn new(ctx: &Context) -> Option<Tray> {
            let (tx, rx) = channel::<TrayCommand>();
            let menu = Menu::new();
            let show = MenuItem::new("Show Roch Monitor", true, None);
            let settings = MenuItem::new("Settings…", true, None);
            let reset = MenuItem::new("Reset Stats", true, None);
            let exit = MenuItem::new("Exit", true, None);
            menu.append_items(&[
                &show,
                &settings,
                &PredefinedMenuItem::separator(),
                &reset,
                &PredefinedMenuItem::separator(),
                &exit,
            ])
            .ok()?;
            let ids = [
                (show.id().clone(), TrayCommand::Show),
                (settings.id().clone(), TrayCommand::Settings),
                (reset.id().clone(), TrayCommand::ResetStats),
                (exit.id().clone(), TrayCommand::Exit),
            ];
            let mut builder = TrayIconBuilder::new()
                .with_id("roch-monitor")
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Roch Monitor");
            if let Some(icon) = roch_icon() {
                builder = builder.with_icon(icon);
            }
            let main = builder.build().ok()?;

            let (wake, send) = (ctx.clone(), tx.clone());
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                if let Some((_, cmd)) = ids.iter().find(|(id, _)| *id == e.id) {
                    let _ = send.send(*cmd);
                    wake.request_repaint();
                }
            }));
            let (wake, send): (Context, Sender<TrayCommand>) = (ctx.clone(), tx);
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                // Any of our icons, clicked with the left button, brings the window back.
                let show = matches!(
                    e,
                    TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                        | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }
                );
                if show {
                    let _ = send.send(TrayCommand::Show);
                    wake.request_repaint();
                }
            }));
            Some(Tray {
                main,
                values: Vec::new(),
                commands: rx,
                tooltip: "Roch Monitor".into(),
                icon_size: small_icon_size(),
            })
        }

        /// Change the Roch icon's hover text, only when it differs: the shell call is not free.
        pub fn set_tooltip(&mut self, text: &str) {
            let text = clip_tooltip(text);
            if self.tooltip != text {
                let _ = self.main.set_tooltip(Some(&text));
                self.tooltip = text;
            }
        }

        /// Make the value icons match `wanted`, in order: new sensors get an icon, removed ones lose
        /// theirs, and an icon is redrawn only when its digits or colours changed.
        pub fn sync_values(&mut self, wanted: &[TrayValue], fg: Color32, bg: Color32) {
            self.values.retain(|v| wanted.iter().any(|w| w.key == v.key));
            for w in wanted {
                let size = self.icon_size;
                let image = || Icon::from_rgba(render_digits(&w.digits, size, fg, bg), size, size).ok();
                match self.values.iter_mut().find(|v| v.key == w.key) {
                    Some(v) => {
                        let drawn = (w.digits.clone(), fg, bg);
                        if v.drawn != drawn {
                            let _ = v.icon.set_icon(image());
                            v.drawn = drawn;
                        }
                        let tip = clip_tooltip(&w.tooltip);
                        if v.tooltip != tip {
                            let _ = v.icon.set_tooltip(Some(&tip));
                            v.tooltip = tip;
                        }
                    }
                    None => {
                        let mut builder = TrayIconBuilder::new()
                            .with_id(format!("roch-monitor:{}", w.key))
                            .with_menu_on_left_click(false)
                            .with_tooltip(clip_tooltip(&w.tooltip));
                        if let Some(icon) = image() {
                            builder = builder.with_icon(icon);
                        }
                        if let Ok(icon) = builder.build() {
                            self.values.push(ValueIcon {
                                key: w.key.clone(),
                                icon,
                                drawn: (w.digits.clone(), fg, bg),
                                tooltip: clip_tooltip(&w.tooltip),
                            });
                        }
                    }
                }
            }
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
            TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
            for v in &self.values {
                let _ = v.icon.set_visible(false);
            }
            let _ = self.main.set_visible(false);
        }
    }
}

#[cfg(windows)]
pub use imp::Tray;

#[cfg(not(windows))]
pub struct Tray {
    pub commands: Receiver<TrayCommand>,
}

#[cfg(not(windows))]
impl Tray {
    pub fn new(_ctx: &Context) -> Option<Tray> {
        None
    }
    pub fn set_tooltip(&mut self, _text: &str) {}
    pub fn sync_values(&mut self, _wanted: &[TrayValue], _fg: Color32, _bg: Color32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_fit_a_small_icon() {
        assert_eq!(icon_digits(45.4, "°C"), "45");
        assert_eq!(icon_digits(151.2, "W"), "151");
        assert_eq!(icon_digits(2040.0, "RPM"), "2.0k");
        assert_eq!(icon_digits(5674.0, "MHz"), "5.7k");
        assert_eq!(icon_digits(13801.0, "MHz"), "14k");
        assert_eq!(icon_digits(1.308, "V"), "1.31");
        assert_eq!(icon_digits(-5.0, "°C"), "-5");
        assert_eq!(icon_digits(f64::NAN, "°C"), "-");
    }

    #[test]
    fn digits_are_drawn_centred_on_the_ground() {
        let (fg, bg) = (Color32::WHITE, Color32::BLACK);
        let px = render_digits("45", 16, fg, bg);
        assert_eq!(px.len(), 16 * 16 * 4);
        let lit = px.chunks(4).filter(|p| p[0] == 255).count();
        assert!(lit > 10, "the digits light pixels");
        // The corners are ground.
        assert_eq!(&px[0..4], &[0, 0, 0, 255]);
        // Every character of the widest reading fits a 16 px icon.
        for text in ["2.0k", "1.31", "999", "-15"] {
            let glyphs: Vec<_> = text.chars().filter_map(glyph).collect();
            let width: u32 = glyphs.iter().map(|g| g.0).sum::<u32>() + glyphs.len() as u32 - 1;
            assert!(width <= 16, "{text} is {width} px wide");
        }
        // A larger icon scales the font by whole pixels.
        let big = render_digits("45", 32, fg, bg);
        let lit_big = big.chunks(4).filter(|p| p[0] == 255).count();
        assert_eq!(lit_big, lit * 4);
    }

    #[test]
    fn tooltips_are_clipped_to_the_shells_limit() {
        let long = "x".repeat(300);
        assert_eq!(clip_tooltip(&long).chars().count(), TOOLTIP_MAX);
        assert_eq!(clip_tooltip("short"), "short");
    }
}

#[cfg(test)]
mod preview {
    /// Writes what the value icons look like to PNGs, for a person to look at:
    /// `cargo test -p roch-monitor -- --ignored write_tray_previews`.
    #[test]
    #[ignore]
    fn write_tray_previews() {
        use eframe::egui::Color32;
        let dir = std::env::temp_dir().join("roch-monitor-tray-preview");
        std::fs::create_dir_all(&dir).unwrap();
        let (fg, bg) = (Color32::from_rgb(0xFF, 0x5A, 0x5F), Color32::BLACK);
        for (name, text) in [("temp", "47"), ("power", "151"), ("rpm", "2.0k"), ("volt", "1.31")] {
            for size in [16u32, 32] {
                let rgba = super::render_digits(text, size, fg, bg);
                // Upscaled 8x with nearest-neighbour, so the pixels can be seen.
                let scale = 128 / size;
                let mut big = image::RgbaImage::new(size * scale, size * scale);
                for (x, y, p) in big.enumerate_pixels_mut() {
                    let i = (((y / scale) * size + x / scale) * 4) as usize;
                    *p = image::Rgba([rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]);
                }
                big.save(dir.join(format!("{name}-{size}.png"))).unwrap();
            }
        }
        println!("{}", dir.display());
    }
}
