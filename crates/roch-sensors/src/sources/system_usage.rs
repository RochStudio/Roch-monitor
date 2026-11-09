//! Memory use, the way HWiNFO's System section shows it. No driver: GlobalMemoryStatusEx.

use crate::engine::{Batch, Source};
use crate::model::{DeviceInfo, DeviceKind, Group, SensorInfo};
use crate::system::SystemInfo;

const DEV: &str = "system";

pub fn system_device(sys: &SystemInfo) -> DeviceInfo {
    let name = if sys.computer.is_empty() { "System".to_owned() } else { sys.computer.clone() };
    DeviceInfo::new(DEV, DeviceKind::System, name).detail(sys.os.clone())
}

pub struct SystemUsage {
    device: DeviceInfo,
}

impl SystemUsage {
    pub fn new(sys: &SystemInfo) -> SystemUsage {
        SystemUsage { device: system_device(sys) }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Memory {
    pub load: f64,
    pub total_mb: f64,
    pub available_mb: f64,
    pub commit_limit_mb: f64,
    pub commit_available_mb: f64,
}

#[cfg(windows)]
pub fn memory() -> Option<Memory> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: dwLength set as the API requires; the struct is plain data.
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 {
        return None;
    }
    const MB: f64 = 1024.0 * 1024.0;
    Some(Memory {
        load: m.dwMemoryLoad as f64,
        total_mb: m.ullTotalPhys as f64 / MB,
        available_mb: m.ullAvailPhys as f64 / MB,
        commit_limit_mb: m.ullTotalPageFile as f64 / MB,
        commit_available_mb: m.ullAvailPageFile as f64 / MB,
    })
}

#[cfg(not(windows))]
pub fn memory() -> Option<Memory> {
    None
}

impl Source for SystemUsage {
    fn name(&self) -> &str {
        "System"
    }

    fn poll(&mut self, out: &mut Batch) {
        out.device(self.device.clone());
        out.status("System: memory use from GlobalMemoryStatusEx.");
        let Some(m) = memory() else { return };
        let s = |key: &str, label: &str, unit: &str, decimals: u8| {
            SensorInfo::new(DEV, key, Group::Memory, label, unit, decimals)
        };
        let used = m.total_mb - m.available_mb;
        out.put(&s("phys/used", "Physical Memory Used", "MB", 0), used);
        out.put(&s("phys/available", "Physical Memory Available", "MB", 0), m.available_mb);
        out.put(
            &SensorInfo::new(DEV, "phys/load", Group::Load, "Physical Memory Load", "%", 1),
            used / m.total_mb * 100.0,
        );
        let committed = m.commit_limit_mb - m.commit_available_mb;
        out.put(&s("virt/committed", "Virtual Memory Committed", "MB", 0), committed);
        out.put(&s("virt/available", "Virtual Memory Available", "MB", 0), m.commit_available_mb);
        out.put(
            &SensorInfo::new(DEV, "virt/load", Group::Load, "Virtual Memory Load", "%", 1),
            committed / m.commit_limit_mb * 100.0,
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn this_machine_reports_its_memory() {
        let m = super::memory().unwrap();
        assert!(m.total_mb > 1024.0);
        assert!(m.available_mb <= m.total_mb);
    }
}
