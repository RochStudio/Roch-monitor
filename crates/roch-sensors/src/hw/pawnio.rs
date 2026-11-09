//! MSR reads through PawnIO (https://pawnio.eu), the signed, scriptable kernel driver
//! LibreHardwareMonitor moved to. inpoutx64 maps memory and ports but has no RDMSR, which is why
//! Roch Viewer could not read per-core temperatures; PawnIO can, through its IntelMSR and AMDFamily17
//! modules.
//!
//! The interface, from PawnIO's own `pawnio_um.h` and `PawnIOLib.cpp`: open `\Device\PawnIO`, load a
//! module blob into the handle with one IOCTL, then call the module's functions by name with another,
//! the input being a 32-byte NUL-padded name followed by u64 arguments and the output u64 values.
//!
//! An MSR read happens on whichever logical processor the calling thread is running on, so a reader
//! pins its thread to the processor it wants first ([`pin_to`]), as LibreHardwareMonitor does.
//!
//! Read-only: the only function called is `ioctl_read_msr`. The modules also export MSR writes,
//! and nothing here names them.

pub const INTEL_MSR_MODULE: &[u8] = include_bytes!("../../res/pawnio/IntelMSR.bin");
pub const AMD_FAMILY17_MODULE: &[u8] = include_bytes!("../../res/pawnio/AMDFamily17.bin");

/// `CTL_CODE(41394, function, METHOD_BUFFERED, FILE_ANY_ACCESS)`.
const fn ctl_code(function: u32) -> u32 {
    (41394 << 16) | (function << 2)
}
pub const IOCTL_LOAD_BINARY: u32 = ctl_code(0x821);
pub const IOCTL_EXECUTE: u32 = ctl_code(0x841);
const NAME_BYTES: usize = 32;

/// The execute IOCTL's input: the function name in 32 NUL-padded bytes, then the arguments.
pub fn execute_input(name: &str, args: &[u64]) -> Vec<u8> {
    let mut buf = vec![0u8; NAME_BYTES + 8 * args.len()];
    let bytes = name.as_bytes();
    let n = bytes.len().min(NAME_BYTES - 1);
    buf[..n].copy_from_slice(&bytes[..n]);
    for (i, a) in args.iter().enumerate() {
        buf[NAME_BYTES + 8 * i..NAME_BYTES + 8 * i + 8].copy_from_slice(&a.to_le_bytes());
    }
    buf
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    /// One open handle with one module loaded into it.
    pub struct PawnIo {
        handle: HANDLE,
    }

    // A handle is used from the one source thread that owns it.
    unsafe impl Send for PawnIo {}

    impl PawnIo {
        /// Open the driver and load `module`. The error says, in words, why not.
        pub fn open(module: &[u8]) -> Result<PawnIo, String> {
            let path: Vec<u16> = r"\\?\GLOBALROOT\Device\PawnIO".encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: a plain CreateFileW on a device path.
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE || handle.is_null() {
                // SAFETY: no arguments.
                let err = unsafe { GetLastError() };
                return Err(match err {
                    2 | 3 => "PawnIO is not installed. Per-core temperatures, clocks and VIDs need it: install it from pawnio.eu.".into(),
                    5 => "PawnIO is installed but refused this process: run Roch Monitor as administrator.".into(),
                    e => format!("PawnIO could not be opened (error {e})."),
                });
            }
            let io = PawnIo { handle };
            let mut returned = 0u32;
            // SAFETY: the blob outlives the call; no output buffer.
            let ok = unsafe {
                DeviceIoControl(
                    io.handle,
                    IOCTL_LOAD_BINARY,
                    module.as_ptr().cast(),
                    module.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: no arguments.
                let err = unsafe { GetLastError() };
                return Err(format!(
                    "PawnIO refused the MSR module (error {err}); the driver may be older than the module."
                ));
            }
            Ok(io)
        }

        /// Call one of the module's functions; `out` values come back.
        pub fn execute(&self, name: &str, args: &[u64], out: usize) -> Option<Vec<u64>> {
            let input = execute_input(name, args);
            let mut output = vec![0u64; out.max(1)];
            let mut returned = 0u32;
            // SAFETY: both buffers are ours and sized as passed.
            let ok = unsafe {
                DeviceIoControl(
                    self.handle,
                    IOCTL_EXECUTE,
                    input.as_ptr().cast(),
                    input.len() as u32,
                    output.as_mut_ptr().cast(),
                    (output.len() * 8) as u32,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return None;
            }
            output.truncate((returned as usize / 8).min(out));
            Some(output)
        }

        /// Read one MSR on the processor this thread is running on.
        pub fn read_msr(&self, msr: u32) -> Option<u64> {
            self.execute("ioctl_read_msr", &[msr as u64], 1).and_then(|v| v.first().copied())
        }
    }

    impl Drop for PawnIo {
        fn drop(&mut self) {
            // SAFETY: our own handle.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }

    /// Pin the calling thread to one logical processor. The returned guard puts the thread's
    /// previous affinity back when dropped.
    pub fn pin_to(group: u16, number: u8) -> Option<Pinned> {
        use windows_sys::Win32::System::SystemInformation::GROUP_AFFINITY;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadGroupAffinity};
        let wanted = GROUP_AFFINITY { Mask: 1usize << number, Group: group, Reserved: [0; 3] };
        let mut previous = GROUP_AFFINITY { Mask: 0, Group: 0, Reserved: [0; 3] };
        // SAFETY: the pseudo-handle of this thread, and two plain structs.
        let ok = unsafe { SetThreadGroupAffinity(GetCurrentThread(), &wanted, &mut previous) };
        if ok == 0 {
            return None;
        }
        // Let the scheduler move the thread there before the read runs.
        std::thread::yield_now();
        Some(Pinned { previous })
    }

    pub struct Pinned {
        previous: windows_sys::Win32::System::SystemInformation::GROUP_AFFINITY,
    }

    impl Drop for Pinned {
        fn drop(&mut self) {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadGroupAffinity};
            // SAFETY: as in pin_to.
            unsafe {
                SetThreadGroupAffinity(GetCurrentThread(), &self.previous, std::ptr::null_mut());
            }
        }
    }

    /// Raise the calling thread's priority until the guard drops. A thread pinned to a busy core
    /// otherwise waits out the running thread's time slice before its read: on the i9-14900KF a
    /// 16-thread sweep with a median of 0.28 ms took 74 ms now and then, and over 60 sweeps the
    /// worst was 7.0 ms at normal priority against 0.9 ms raised. Each read is microseconds, so
    /// taking the core ahead of normal-priority work costs that work nothing it would notice.
    pub fn raise_priority() -> Raised {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
        };
        // SAFETY: the pseudo-handle of this thread.
        unsafe {
            let previous = GetThreadPriority(GetCurrentThread());
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
            Raised { previous }
        }
    }

    pub struct Raised {
        previous: i32,
    }

    impl Drop for Raised {
        fn drop(&mut self) {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_NORMAL};
            // What GetThreadPriority returns when it fails (winbase.h).
            const THREAD_PRIORITY_ERROR_RETURN: i32 = 0x7FFF_FFFF;
            let previous =
                if self.previous == THREAD_PRIORITY_ERROR_RETURN { THREAD_PRIORITY_NORMAL } else { self.previous };
            // SAFETY: as in raise_priority.
            unsafe {
                SetThreadPriority(GetCurrentThread(), previous);
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{PawnIo, Pinned, Raised, pin_to, raise_priority};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ioctls_are_pawnios_own() {
        // CTL_CODE(41394, 0x821/0x841, METHOD_BUFFERED, FILE_ANY_ACCESS), from pawnio_um.h.
        assert_eq!(IOCTL_LOAD_BINARY, 0xA1B2_2084);
        assert_eq!(IOCTL_EXECUTE, 0xA1B2_2104);
    }

    #[test]
    fn execute_input_is_a_padded_name_then_the_arguments() {
        let buf = execute_input("ioctl_read_msr", &[0x19C]);
        assert_eq!(buf.len(), 40);
        assert_eq!(&buf[..14], b"ioctl_read_msr");
        assert!(buf[14..32].iter().all(|&b| b == 0));
        assert_eq!(u64::from_le_bytes(buf[32..40].try_into().unwrap()), 0x19C);
        // A name longer than the field keeps its terminating NUL.
        let long = execute_input(&"x".repeat(40), &[]);
        assert_eq!(long[31], 0);
    }

    #[test]
    fn the_modules_are_the_signed_release_files() {
        // Sizes of the 0.2.11 release; a rebuilt module would not carry the author's signature.
        assert_eq!(INTEL_MSR_MODULE.len(), 5324);
        assert_eq!(AMD_FAMILY17_MODULE.len(), 10652);
    }
}
