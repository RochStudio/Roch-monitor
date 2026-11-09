//! The named mutexes the mainstream monitoring tools take before touching a shared bus
//! (Roch Viewer's `NamedMutex`). HWiNFO, LibreHardwareMonitor and the Roch tools all agree on
//! these names, so a reading in flight is never interleaved with another tool's selector write.
//!
//! A failed wait means "do not touch the hardware this time", never "carry on regardless": an
//! interleaved index/data pair reads the wrong register at best and wedges an embedded controller at
//! worst.

use std::sync::OnceLock;

pub const ISA_BUS: &str = "Global\\Access_ISABUS.HTP.Method";
pub const SMBUS: &str = "Global\\Access_SMBUS.HTP.Method";
pub const PCI: &str = "Global\\Access_PCI";
pub const EC: &str = "Global\\Access_EC";

/// How long a reader waits for another tool to finish with a bus before skipping this sample.
pub const WAIT_MS: u32 = 500;

pub struct NamedMutex {
    #[cfg(windows)]
    handle: usize,
    name: &'static str,
}

/// Held while the bus is ours. Released on drop, on the thread that took it (a Windows mutex is
/// owned by a thread, which is also what excludes this process's other polling threads).
pub struct Guard<'a> {
    #[allow(dead_code)]
    mutex: &'a NamedMutex,
}

impl NamedMutex {
    #[cfg(windows)]
    fn create(name: &'static str) -> Self {
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: a plain named-mutex create; a null handle is checked at lock time.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) } as usize;
        Self { handle, name }
    }

    #[cfg(not(windows))]
    fn create(name: &'static str) -> Self {
        Self { name }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Take the bus, or `None` when another tool held it for longer than `timeout_ms`.
    #[cfg(windows)]
    pub fn lock(&self, timeout_ms: u32) -> Option<Guard<'_>> {
        use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        if self.handle == 0 {
            return None;
        }
        // SAFETY: the handle came from CreateMutexW and is never closed.
        let result = unsafe { WaitForSingleObject(self.handle as _, timeout_ms) };
        (result == WAIT_OBJECT_0 || result == WAIT_ABANDONED).then_some(Guard { mutex: self })
    }

    #[cfg(not(windows))]
    pub fn lock(&self, _timeout_ms: u32) -> Option<Guard<'_>> {
        None
    }
}

#[cfg(windows)]
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        // SAFETY: this thread owns the mutex; it took it in `lock`.
        unsafe {
            ReleaseMutex(self.mutex.handle as _);
        }
    }
}

fn get(cell: &'static OnceLock<NamedMutex>, name: &'static str) -> &'static NamedMutex {
    cell.get_or_init(|| NamedMutex::create(name))
}

pub fn isa() -> &'static NamedMutex {
    static M: OnceLock<NamedMutex> = OnceLock::new();
    get(&M, ISA_BUS)
}

pub fn smbus() -> &'static NamedMutex {
    static M: OnceLock<NamedMutex> = OnceLock::new();
    get(&M, SMBUS)
}

pub fn pci() -> &'static NamedMutex {
    static M: OnceLock<NamedMutex> = OnceLock::new();
    get(&M, PCI)
}

pub fn ec() -> &'static NamedMutex {
    static M: OnceLock<NamedMutex> = OnceLock::new();
    get(&M, EC)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_mutex_can_be_taken_and_given_back() {
        let m = NamedMutex::create("Local\\RochMonitorTestMutex");
        drop(m.lock(100).expect("uncontended"));
        assert!(m.lock(100).is_some());
    }
}
