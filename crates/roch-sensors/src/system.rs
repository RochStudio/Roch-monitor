//! What the machine is, resolved once at startup and shared by every source: the CPU, the board,
//! the memory and the OS. Each source decides from this whether its transport applies here, the
//! way Roch Viewer resolves the platform before any privileged read.

use crate::hw::cpuid::{CpuId, Vendor};
use crate::hw::smbios::{MemoryDevice, Smbios, short_maker};
use crate::hw::topology::Topology;

#[derive(Debug, Clone, Default)]
pub struct SystemInfo {
    pub cpu: CpuId,
    pub topology: Topology,
    pub smbios: Smbios,
    pub os: String,
    pub computer: String,
    pub elevated: bool,
}

impl SystemInfo {
    pub fn detect() -> SystemInfo {
        SystemInfo {
            cpu: CpuId::read(),
            topology: Topology::read(),
            smbios: Smbios::read().unwrap_or_default(),
            os: os_name(),
            computer: std::env::var("COMPUTERNAME").unwrap_or_default(),
            elevated: crate::hw::is_elevated(),
        }
    }

    pub fn vendor(&self) -> Vendor {
        self.cpu.vendor()
    }

    pub fn cpu_name(&self) -> String {
        if !self.cpu.brand.is_empty() {
            self.cpu.brand.clone()
        } else if !self.smbios.cpu_version.is_empty() {
            crate::hw::cpuid::clean_brand(&self.smbios.cpu_version)
        } else {
            "Processor".into()
        }
    }

    pub fn board_name(&self) -> String {
        self.smbios.board_display()
    }

    pub fn board_maker(&self) -> String {
        short_maker(&self.smbios.board_manufacturer)
    }

    pub fn is_asus(&self) -> bool {
        self.smbios.board_manufacturer.to_uppercase().starts_with("ASUS")
    }

    pub fn memory_generation(&self) -> Option<&'static str> {
        self.smbios.memory_generation()
    }

    /// "32 GB DDR4-3800": what the modules add up to and what they run at.
    pub fn memory_summary(&self) -> String {
        let total_mb: u64 = self.smbios.memory.iter().map(|m| m.size_mb).sum();
        if total_mb == 0 {
            return String::new();
        }
        let generation = self.memory_generation().unwrap_or("RAM");
        let speed = self
            .smbios
            .memory
            .iter()
            .map(|m| if m.configured_speed_mts > 0 { m.configured_speed_mts } else { m.speed_mts })
            .max()
            .unwrap_or(0);
        let gb = total_mb as f64 / 1024.0;
        let size = if gb.fract() == 0.0 { format!("{gb:.0} GB") } else { format!("{gb:.1} GB") };
        if speed > 0 { format!("{size} {generation}-{speed}") } else { format!("{size} {generation}") }
    }

    /// The module in a slot, by slot index in the board's own order (A1, A2, B1, B2).
    pub fn module_for_slot(&self, slot: usize) -> Option<&MemoryDevice> {
        let wanted = slot_name(slot);
        self.smbios.memory.iter().find(|m| slot_from_locator(&m.locator, &m.bank).as_deref() == Some(wanted.as_str()))
    }
}

/// "A1", "A2", "B1", "B2", ... for slot 0, 1, 2, 3: two slots per channel, which is how Intel
/// desktop boards and AM5 boards both order their SMBus addresses (0x50 + slot).
pub fn slot_name(slot: usize) -> String {
    let channel = (b'A' + (slot / 2) as u8) as char;
    format!("{channel}{}", slot % 2 + 1)
}

/// The slot a module's locators name, as "A2".
///
/// Two conventions are in use. Intel boards name the slot outright in the device locator, one-based:
/// "Controller0-DIMMA2" (MSI), "DIMM_B1" (ASUS). AMD and ASRock boards split it, the channel in the
/// bank locator and a zero-based DIMM number in the device locator: "P0 CHANNEL A" + "DIMM 0" is
/// A1. None when neither convention fits, rather than a guess.
pub fn slot_from_locator(locator: &str, bank: &str) -> Option<String> {
    let up = locator.to_uppercase();
    if let Some(i) = up.find("DIMM") {
        let rest = up[i + 4..].trim_start_matches(['_', ' ', '-']);
        let mut chars = rest.chars();
        if let (Some(ch), Some(n)) = (chars.next(), chars.next())
            && ch.is_ascii_alphabetic()
            && n.is_ascii_digit()
        {
            return Some(format!("{ch}{n}"));
        }
    }
    let both = format!("{} {}", bank.to_uppercase(), up);
    let channel = both.find("CHANNEL").and_then(|i| both[i + 7..].trim_start_matches(['_', ' ', '-']).chars().next());
    let dimm = both.find("DIMM").and_then(|i| both[i + 4..].trim_start_matches(['_', ' ', '-']).chars().next());
    match (channel, dimm) {
        (Some(ch), Some(n)) if ch.is_ascii_alphabetic() && n.is_ascii_digit() => {
            Some(format!("{ch}{}", n.to_digit(10)? + 1))
        }
        _ => None,
    }
}

#[cfg(windows)]
fn os_name() -> String {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};
    let key: Vec<u16> = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\0".encode_utf16().collect();
    let read_str = |name: &str| -> String {
        let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut buf = [0u16; 256];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: bounded string read from the registry.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return String::new();
        }
        let n = (len as usize / 2).saturating_sub(1).min(buf.len());
        String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_owned()
    };
    let read_dword = |name: &str| -> Option<u32> {
        let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut value = 0u32;
        let mut len = 4u32;
        // SAFETY: a DWORD read.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut len,
            )
        };
        (rc == 0).then_some(value)
    };
    let mut product = read_str("ProductName");
    let build: u32 = read_str("CurrentBuildNumber").parse().unwrap_or(0);
    // Windows 11 still reports "Windows 10" in ProductName; the build number is what tells them apart.
    if build >= 22000 {
        product = product.replace("Windows 10", "Windows 11");
    }
    let display = read_str("DisplayVersion");
    let ubr = read_dword("UBR");
    let mut s = product;
    if !display.is_empty() {
        s.push_str(&format!(" {display}"));
    }
    if build > 0 {
        match ubr {
            Some(u) => s.push_str(&format!(" (build {build}.{u})")),
            None => s.push_str(&format!(" (build {build})")),
        }
    }
    s
}

#[cfg(not(windows))]
fn os_name() -> String {
    std::env::consts::OS.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_named_in_board_order() {
        assert_eq!(slot_name(0), "A1");
        assert_eq!(slot_name(1), "A2");
        assert_eq!(slot_name(3), "B2");
    }

    #[test]
    fn locators_name_their_slot() {
        // What this machine's SMBIOS says for its two modules.
        assert_eq!(slot_from_locator("Controller0-DIMMA2", "BANK 0").as_deref(), Some("A2"));
        assert_eq!(slot_from_locator("Controller1-DIMMB2", "BANK 0").as_deref(), Some("B2"));
        assert_eq!(slot_from_locator("DIMM_A1", "").as_deref(), Some("A1"));
        // AM5: channel in the bank locator, zero-based DIMM number in the device locator.
        assert_eq!(slot_from_locator("DIMM 1", "P0 CHANNEL B").as_deref(), Some("B2"));
        assert_eq!(slot_from_locator("ChannelA-DIMM0", "BANK 0").as_deref(), Some("A1"));
        assert_eq!(slot_from_locator("Slot 3", "Bank 1"), None);
    }
}
