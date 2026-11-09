//! The low-level access every transport is built on: the two kernel drivers (PawnIO first,
//! inpoutx64 where PawnIO is not installed), the shared bus mutexes, PCI configuration through
//! ECAM, and the firmware tables.

pub mod cpuid;
pub mod firmware;
pub mod inpout;
pub mod mutex;
pub mod pawnio;
pub mod pci;
pub mod ports;
pub mod smbios;
pub mod topology;

/// Which kernel driver the board, DIMM, memory-controller and EC transports go through. The
/// per-core MSRs are PawnIO's alone, and the AMD SMU's PM table inpoutx64's alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Driver {
    /// PawnIO wherever it is installed and its module takes this machine; inpoutx64 for a
    /// transport PawnIO cannot serve. inpoutx64 is never loaded while PawnIO serves everything.
    #[default]
    Auto,
    /// PawnIO only: inpoutx64 is never loaded.
    PawnIo,
    /// inpoutx64 only, as Roch Monitor 1.0.0 did.
    InpOut,
}

impl Driver {
    pub fn parse(s: &str) -> Option<Driver> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Some(Driver::Auto),
            "pawnio" => Some(Driver::PawnIo),
            "inpoutx64" | "inpout" => Some(Driver::InpOut),
            _ => None,
        }
    }

    pub fn pawnio(self) -> bool {
        self != Driver::InpOut
    }

    pub fn inpout(self) -> bool {
        self != Driver::PawnIo
    }
}

/// Both drivers in one line, for reports: whether PawnIO opens, and whether anything has loaded
/// inpoutx64 (asked without loading it).
pub fn drivers_summary() -> String {
    let pawnio = match pawnio::available() {
        Ok(()) => "PawnIO open".to_owned(),
        Err(e) => e,
    };
    let inpout = match inpout::loaded() {
        None => "inpoutx64 not loaded".to_owned(),
        Some(Ok(d)) => format!("inpoutx64 loaded from {}", d.path().display()),
        Some(Err(e)) => format!("inpoutx64: {e}"),
    };
    format!("{pawnio}; {inpout}.")
}

/// True when this process holds an elevated token; the driver will not open otherwise.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: standard token query; the handle is closed before returning.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_driver_choice_reads_from_the_command_line() {
        assert_eq!(Driver::parse("PawnIO"), Some(Driver::PawnIo));
        assert_eq!(Driver::parse("inpoutx64"), Some(Driver::InpOut));
        assert_eq!(Driver::parse("auto"), Some(Driver::Auto));
        assert_eq!(Driver::parse("winring0"), None);
        assert!(Driver::Auto.pawnio() && Driver::Auto.inpout());
        assert!(!Driver::PawnIo.inpout() && !Driver::InpOut.pawnio());
    }
}
