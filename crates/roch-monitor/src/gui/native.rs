//! The few things the window asks of Windows directly: title bars that follow the theme, window
//! opacity (Roch GPU's `apply_opacity`), and showing a file in Explorer. Only this process's own
//! windows are touched, found by their titles on the UI thread.

use std::path::Path;

/// This thread's top-level windows whose title starts with `prefix`.
#[cfg(windows)]
fn own_windows(prefix: &str) -> Vec<windows_sys::Win32::Foundation::HWND> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, GetWindowTextW};

    struct Search<'a> {
        prefix: &'a str,
        found: Vec<HWND>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: lparam is the caller's Search, alive for the whole enumeration.
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut title = [0u16; 96];
        // SAFETY: bounded read of a window title into our buffer.
        let n = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        if n > 0 && String::from_utf16_lossy(&title[..n as usize]).starts_with(search.prefix) {
            search.found.push(hwnd);
        }
        1
    }

    let mut search = Search { prefix, found: Vec::new() };
    // SAFETY: the callback only writes through the pointer we pass, which outlives the call.
    unsafe {
        EnumThreadWindows(GetCurrentThreadId(), Some(visit), &mut search as *mut Search as LPARAM);
    }
    search.found
}

/// Switch every Roch Monitor window's title bar to the dark or light frame. Returns how many windows
/// were set, so the caller can try again once a window it expects has opened.
#[cfg(windows)]
pub fn set_title_bars_dark(dark: bool) -> usize {
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    let windows = own_windows("Roch Monitor");
    let value: i32 = dark as i32;
    for hwnd in &windows {
        // SAFETY: a documented attribute with a BOOL-sized value, on our own window.
        unsafe {
            DwmSetWindowAttribute(*hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&value as *const i32).cast(), 4);
        }
    }
    windows.len()
}

#[cfg(not(windows))]
pub fn set_title_bars_dark(_dark: bool) -> usize {
    1
}

/// Make the main window translucent (Roch GPU's `apply_opacity`). Fully opaque takes the layered
/// style off again rather than leaving a layered window at 255.
#[cfg(windows)]
pub fn apply_opacity(opacity: f32) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED,
    };
    // The exact title: the Settings window stays opaque so its controls stay readable.
    let Some(&window) = own_windows("Roch Monitor").iter().find(|&&h| title_of(h) == "Roch Monitor") else {
        return false;
    };
    let alpha = (opacity.clamp(0.3, 1.0) * 255.0).round() as u8;
    // SAFETY: style and alpha changes on a window owned by this process.
    unsafe {
        let style = GetWindowLongPtrW(window, GWL_EXSTYLE);
        let layered = style & WS_EX_LAYERED as isize != 0;
        if alpha == 255 {
            if layered {
                SetWindowLongPtrW(window, GWL_EXSTYLE, style & !(WS_EX_LAYERED as isize));
            }
            return true;
        }
        if !layered {
            SetWindowLongPtrW(window, GWL_EXSTYLE, style | WS_EX_LAYERED as isize);
        }
        SetLayeredWindowAttributes(window, 0, alpha, LWA_ALPHA);
    }
    true
}

#[cfg(windows)]
fn title_of(hwnd: windows_sys::Win32::Foundation::HWND) -> String {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let mut title = [0u16; 96];
    // SAFETY: bounded read into our buffer.
    let n = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
    String::from_utf16_lossy(&title[..n.max(0) as usize])
}

#[cfg(not(windows))]
pub fn apply_opacity(_opacity: f32) -> bool {
    true
}

/// Open Explorer with the file selected (or the folder, for a folder).
pub fn reveal(path: &Path) {
    let arg = if path.is_dir() { path.display().to_string() } else { format!("/select,{}", path.display()) };
    let _ = std::process::Command::new("explorer.exe").arg(arg).spawn();
}
