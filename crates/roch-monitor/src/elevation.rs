//! Relaunch elevated (Roch GPU's `elevation.rs`). The board, DIMM and memory-controller readings go
//! through a kernel driver, which only opens for an administrator. Declining the prompt is allowed:
//! the window then runs with what needs no driver (CPU counters, GPU, drives, network).

pub fn join_args(args: &[String]) -> String {
    args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" ")
}

/// True when the elevated copy was started and this one should exit.
#[cfg(windows)]
pub fn relaunch_elevated(args: &[String]) -> bool {
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let Ok(exe) = std::env::current_exe() else { return false };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let file = wide(&exe.to_string_lossy());
    let mut all = args.to_vec();
    all.push("--elevated".into());
    let params = wide(&join_args(&all));
    let verb = wide("runas");
    // SAFETY: a zeroed SHELLEXECUTEINFOW with the documented fields set; the strings outlive the call.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_SHOWNORMAL;
        ShellExecuteExW(&mut info) != 0
    }
}

#[cfg(not(windows))]
pub fn relaunch_elevated(_args: &[String]) -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn arguments_with_spaces_are_quoted() {
        assert_eq!(super::join_args(&["--log".into(), "C:\\My Logs\\a.csv".into()]), "--log \"C:\\My Logs\\a.csv\"");
    }
}
