//! Borrow the caller's console for the command line (Roch GPU's `console.rs`): the exe is a
//! windows-subsystem program, so it has no console of its own. Output already redirected to a file
//! or pipe is left alone.

#[must_use]
pub struct ConsoleGuard {
    #[allow(dead_code)]
    restore_code_page: Option<u32>,
}

impl Drop for ConsoleGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(cp) = self.restore_code_page.take() {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            // SAFETY: restoring the code page found on entry.
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleOutputCP(cp);
            }
        }
    }
}

#[cfg(windows)]
pub fn attach() -> ConsoleGuard {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleOutputCP, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetConsoleOutputCP, SetStdHandle,
    };
    // SAFETY: standard console calls.
    unsafe {
        let current = GetStdHandle(STD_OUTPUT_HANDLE);
        let inherited = !current.is_null() && current != INVALID_HANDLE_VALUE;
        if inherited || AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return ConsoleGuard { restore_code_page: None };
        }
        let previous = GetConsoleOutputCP();
        SetConsoleOutputCP(65001);
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let h = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, h);
            SetStdHandle(STD_ERROR_HANDLE, h);
        }
        ConsoleGuard { restore_code_page: (previous != 0).then_some(previous) }
    }
}

#[cfg(not(windows))]
pub fn attach() -> ConsoleGuard {
    ConsoleGuard { restore_code_page: None }
}
