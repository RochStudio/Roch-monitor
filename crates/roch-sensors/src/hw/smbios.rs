//! The machine's identity out of the raw SMBIOS block: board, BIOS, processor and each memory
//! module with the slot the board itself names. No WMI: the same structures WMI reads, parsed
//! directly, which takes microseconds instead of the second Roch Viewer measured for a WMI
//! connection.

use super::firmware;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Smbios {
    pub version: (u8, u8),
    pub bios_vendor: String,
    pub bios_version: String,
    pub bios_date: String,
    pub system_manufacturer: String,
    pub system_product: String,
    pub board_manufacturer: String,
    pub board_product: String,
    pub board_version: String,
    pub cpu_socket: String,
    pub cpu_version: String,
    pub memory: Vec<MemoryDevice>,
}

/// One populated memory slot (SMBIOS type 17).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryDevice {
    /// The slot as the board names it: "Controller0-DIMMA2", "DIMM_A1".
    pub locator: String,
    pub bank: String,
    pub size_mb: u64,
    /// SMBIOS memory type: 0x1A DDR4, 0x22 DDR5.
    pub memory_type: u8,
    pub speed_mts: u32,
    pub configured_speed_mts: u32,
    pub manufacturer: String,
    pub part_number: String,
    pub serial: String,
    pub configured_mv: u32,
}

pub const MEMORY_DDR4: u8 = 0x1A;
pub const MEMORY_LPDDR4: u8 = 0x1E;
pub const MEMORY_DDR5: u8 = 0x22;
pub const MEMORY_LPDDR5: u8 = 0x23;

impl MemoryDevice {
    pub fn generation(&self) -> Option<&'static str> {
        match self.memory_type {
            MEMORY_DDR4 | MEMORY_LPDDR4 => Some("DDR4"),
            MEMORY_DDR5 | MEMORY_LPDDR5 => Some("DDR5"),
            0x18 => Some("DDR3"),
            _ => None,
        }
    }
}

impl Smbios {
    pub fn read() -> Option<Smbios> {
        firmware::table(firmware::PROVIDER_RSMB, 0).map(|raw| parse(&raw))
    }

    /// "DDR4", "DDR5" or None, from what the modules themselves declare.
    pub fn memory_generation(&self) -> Option<&'static str> {
        let gens: Vec<&str> = self.memory.iter().filter_map(MemoryDevice::generation).collect();
        if gens.contains(&"DDR5") {
            Some("DDR5")
        } else if gens.contains(&"DDR4") {
            Some("DDR4")
        } else {
            gens.first().copied()
        }
    }

    /// "MSI PRO Z790-P WIFI DDR4 (MS-7E06)": the board as a person names it.
    pub fn board_display(&self) -> String {
        let maker = short_maker(&self.board_manufacturer);
        let product = self.board_product.trim();
        let mut name = if maker.is_empty() || product.to_uppercase().starts_with(&maker.to_uppercase()) {
            product.to_owned()
        } else {
            format!("{maker} {product}")
        };
        // MSI puts the marketing name in the system product and the model code on the board.
        let system = self.system_product.trim();
        if !system.is_empty() && !is_placeholder(system) && system != product && product.starts_with("MS-") {
            name = format!("{maker} {system} ({product})").trim().to_owned();
        }
        if name.is_empty() { "Motherboard".into() } else { name }
    }
}

/// The vendor names SMBIOS spells out in full, shortened to how people say them.
pub fn short_maker(maker: &str) -> String {
    let m = maker.trim();
    let upper = m.to_uppercase();
    if upper.starts_with("MICRO-STAR") {
        "MSI".into()
    } else if upper.starts_with("ASUSTEK") {
        "ASUS".into()
    } else if upper.starts_with("GIGA-BYTE") || upper.starts_with("GIGABYTE") {
        "Gigabyte".into()
    } else if upper.starts_with("ASROCK") {
        "ASRock".into()
    } else {
        m.to_owned()
    }
}

/// Fields firmware fills with filler rather than leaving empty.
pub fn is_placeholder(s: &str) -> bool {
    let u = s.trim().to_uppercase();
    u.is_empty()
        || u.contains("TO BE FILLED")
        || u.contains("DEFAULT STRING")
        || u == "SYSTEM PRODUCT NAME"
        || u == "NOT SPECIFIED"
        || u == "UNKNOWN"
        || u == "NONE"
        || u == "0"
}

/// Parse a `RawSMBIOSData` block (an 8-byte header, then the structure table).
pub fn parse(raw: &[u8]) -> Smbios {
    let mut s = Smbios::default();
    if raw.len() < 8 {
        return s;
    }
    s.version = (raw[1], raw[2]);
    let length = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    let table = &raw[8..(8 + length).min(raw.len())];
    let mut at = 0usize;
    while at + 4 <= table.len() {
        let kind = table[at];
        let len = table[at + 1] as usize;
        if len < 4 || at + len > table.len() {
            break;
        }
        let formatted = &table[at..at + len];
        // The string set follows the formatted area and ends at a double NUL.
        let mut end = at + len;
        while end + 1 < table.len() && !(table[end] == 0 && table[end + 1] == 0) {
            end += 1;
        }
        let strings: Vec<String> = table[at + len..end.min(table.len())]
            .split(|&b| b == 0)
            .map(|b| String::from_utf8_lossy(b).trim().to_owned())
            .collect();
        let text = |offset: usize| -> String {
            let index = *formatted.get(offset).unwrap_or(&0) as usize;
            if index == 0 { String::new() } else { strings.get(index - 1).cloned().unwrap_or_default() }
        };
        let byte = |offset: usize| -> u8 { *formatted.get(offset).unwrap_or(&0) };
        let word = |offset: usize| -> u16 {
            formatted.get(offset..offset + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0)
        };
        let dword = |offset: usize| -> u32 {
            formatted.get(offset..offset + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0)
        };
        match kind {
            0 => {
                s.bios_vendor = text(0x04);
                s.bios_version = text(0x05);
                s.bios_date = text(0x08);
            }
            1 => {
                s.system_manufacturer = text(0x04);
                s.system_product = text(0x05);
            }
            2 => {
                s.board_manufacturer = text(0x04);
                s.board_product = text(0x05);
                s.board_version = text(0x06);
            }
            4 if s.cpu_version.is_empty() => {
                s.cpu_socket = text(0x04);
                s.cpu_version = text(0x10);
            }
            17 => {
                let size = word(0x0C);
                let size_mb = match size {
                    0 | 0xFFFF => 0,
                    0x7FFF => dword(0x1C) as u64 & 0x7FFF_FFFF,
                    s if s & 0x8000 != 0 => ((s & 0x7FFF) as u64) / 1024,
                    s => s as u64,
                };
                if size_mb > 0 {
                    s.memory.push(MemoryDevice {
                        locator: text(0x10),
                        bank: text(0x11),
                        size_mb,
                        memory_type: byte(0x12),
                        speed_mts: word(0x15) as u32,
                        manufacturer: text(0x17),
                        serial: text(0x18),
                        part_number: text(0x1A),
                        configured_speed_mts: if len > 0x21 { word(0x20) as u32 } else { 0 },
                        configured_mv: if len > 0x27 { word(0x26) as u32 } else { 0 },
                    });
                }
            }
            127 => break,
            _ => {}
        }
        at = end + 2;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structure(kind: u8, formatted: &[u8], strings: &[&str]) -> Vec<u8> {
        let mut v = vec![kind, (formatted.len() + 4) as u8, 0, 0];
        v.extend_from_slice(formatted);
        for s in strings {
            v.extend_from_slice(s.as_bytes());
            v.push(0);
        }
        if strings.is_empty() {
            v.push(0);
        }
        v.push(0);
        v
    }

    #[test]
    fn board_bios_and_modules_parse() {
        let mut table = Vec::new();
        // Type 0: vendor(1) version(2) date(3) at 0x04, 0x05, 0x08.
        let mut bios = vec![0u8; 0x14];
        bios[0] = 1;
        bios[1] = 2;
        bios[4] = 3;
        table.extend(structure(0, &bios, &["American Megatrends International, LLC.", "A.D0", "08/22/2024"]));
        let mut sys = vec![0u8; 0x15];
        sys[0] = 1;
        sys[1] = 2;
        table.extend(structure(1, &sys, &["Micro-Star International Co., Ltd.", "MS-7E06"]));
        let mut board = vec![0u8; 0x0B];
        board[0] = 1;
        board[1] = 2;
        board[2] = 3;
        table.extend(structure(
            2,
            &board,
            &["Micro-Star International Co., Ltd.", "PRO Z790-P WIFI DDR4 (MS-7E06)", "1.0"],
        ));
        let mut dimm = vec![0u8; 0x24];
        dimm[0x0C - 4..0x0E - 4].copy_from_slice(&16384u16.to_le_bytes());
        dimm[0x10 - 4] = 1;
        dimm[0x11 - 4] = 2;
        dimm[0x12 - 4] = MEMORY_DDR4;
        dimm[0x15 - 4..0x17 - 4].copy_from_slice(&3600u16.to_le_bytes());
        dimm[0x17 - 4] = 3;
        dimm[0x1A - 4] = 4;
        dimm[0x20 - 4..0x22 - 4].copy_from_slice(&3800u16.to_le_bytes());
        table.extend(structure(17, &dimm, &["Controller0-DIMMA2", "BANK 0", "G.Skill", "F4-3600C14-16GVKA"]));
        table.extend(structure(127, &[], &[]));

        let mut raw = vec![0, 3, 6, 0];
        raw.extend_from_slice(&(table.len() as u32).to_le_bytes());
        raw.extend(table);
        let s = parse(&raw);
        assert_eq!(s.bios_version, "A.D0");
        assert_eq!(s.board_display(), "MSI PRO Z790-P WIFI DDR4 (MS-7E06)");
        assert_eq!(s.memory.len(), 1);
        let m = &s.memory[0];
        assert_eq!(
            (m.locator.as_str(), m.size_mb, m.speed_mts, m.configured_speed_mts),
            ("Controller0-DIMMA2", 16384, 3600, 3800)
        );
        assert_eq!(m.part_number, "F4-3600C14-16GVKA");
        assert_eq!(s.memory_generation(), Some("DDR4"));
    }

    #[test]
    fn makers_are_shortened() {
        assert_eq!(short_maker("ASUSTeK COMPUTER INC."), "ASUS");
        assert_eq!(short_maker("Gigabyte Technology Co., Ltd."), "Gigabyte");
        assert!(is_placeholder("To be filled by O.E.M."));
    }
}
