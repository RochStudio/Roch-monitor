//! The board's Super I/O: temperatures, rails and fans (Roch Viewer's `superio_lpc`, `nct679x`,
//! `ite_superio` and `intel_board_sensors`).
//!
//! Three chip families, three different register files, and addressing one with another's protocol
//! does not fault: it answers 0xFFFF and repeated bytes, which a temperature band accepts as
//! -0.004 °C. So each transport identifies its own chip before anything is read, and a chip none of
//! them knows leaves the rows absent rather than invented.
//!
//!   NCT668x  an embedded-controller window at the LDN 0x0B base, addressed page/index/data at
//!            base+4/5/6, sensors as big-endian words in flat blocks at 0x100 (temperatures),
//!            0x120 (voltages), 0x140 (fan speeds) and 0x160 (fan duty).
//!   NCT679x  a bank-switched register file at the same base, index/data at base+5/6 after
//!            selecting a bank through register 0x4E, sensors as single bytes.
//!   ITE      its own unlock sequence and a plain index/data pair at the LDN 4 base.
//!
//! Which channel is which rail is the board vendor's choice, not the chip's, so every map below
//! records the board it was confirmed on and how. A reading outside the band its rail can
//! physically hold is dropped rather than shown.
//!
//! The ports are reached through PawnIO's LpcIO module where it is installed, one handle per
//! config port: the module admits that slot's config pair, and the ports of the chip's BARs once
//! it has found them with the chip in configuration mode. inpoutx64, where PawnIO is not
//! installed, admits every port. The protocols are the same either way.
//!
//! SAFETY: the only writes are the ones the read path needs: the vendor unlock and lock sequences,
//! the logical-device selector, and the page/bank/index registers that address a sensor byte. There
//! is no write to any sensor, fan, PWM or configuration value, and no primitive that could make
//! one: this chip also drives the fans. Config mode is always exited, and every access holds the
//! ISA bus mutex the mainstream tools take.

use crate::engine::{Batch, Source};
use crate::hw::Driver;
use crate::hw::cpuid::Vendor;
use crate::hw::inpout::driver;
use crate::hw::mutex;
use crate::hw::ports::Ports;
use crate::hw::smbios::Smbios;
use crate::model::{DeviceInfo, DeviceKind, Group, SensorInfo};
use crate::system::SystemInfo;

const CONFIG_PORTS: [u16; 2] = [0x2E, 0x4E];
const NUVOTON_UNLOCK: u8 = 0x87;
const NUVOTON_LOCK: u8 = 0xAA;
const REG_LOGICAL_DEVICE: u8 = 0x07;
const REG_CHIP_ID_HIGH: u8 = 0x20;
const REG_CHIP_ID_LOW: u8 = 0x21;
const REG_BASE_HIGH: u8 = 0x60;
const REG_BASE_LOW: u8 = 0x61;
const LDN_EC_SPACE: u8 = 0x0B;
const ITE_UNLOCK: [u8; 3] = [0x87, 0x01, 0x55];
const LDN_ITE_EC: u8 = 0x04;

/// The chips each transport speaks to. An id outside these is declined by name.
fn nct668x_name(id: u16) -> Option<&'static str> {
    match id {
        0xD592 | 0xD591 => Some("NCT6687D"),
        0xC732 => Some("NCT6683D"),
        0xD452 => Some("NCT6686D"),
        _ => None,
    }
}

fn nct679x_name(id: u16) -> Option<&'static str> {
    // 0xD42B is what the ASUS ROG MAXIMUS Z790 APEX answers on 0x2E; HWiNFO names that chip
    // NCT6798D, which is where the name comes from.
    match id {
        0xD42B => Some("NCT6798D"),
        _ => None,
    }
}

fn ite_name(id: u16) -> Option<(&'static str, f64)> {
    // The two parts do not share a voltage LSB, and swapping them misses every rail by 8-10%.
    match id {
        0x8696 => Some(("IT8696E", 0.012)),
        0x8695 => Some(("IT87952E", 0.0109)),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Family {
    Nct668x,
    Nct679x,
}

#[derive(Debug, Clone)]
struct NuvotonChip {
    family: Family,
    port: u16,
    id: u16,
    name: &'static str,
    base: u16,
}

#[derive(Debug, Clone)]
struct IteChip {
    name: &'static str,
    port: u16,
    base: u16,
    step: f64,
}

fn read_config(io: &Ports, port: u16, reg: u8) -> Option<u8> {
    io.outb(port, reg)?;
    io.inb(port + 1)
}

/// A big-endian pair of config registers: the chip id, a base address.
fn read_config_word(io: &Ports, port: u16, (high, low): (u8, u8)) -> Option<u16> {
    Some(((read_config(io, port, high)? as u16) << 8) | read_config(io, port, low)? as u16)
}

/// Probe one config port for a Nuvoton chip: its (id, sensor window base) when one answered,
/// recognised or not. The chip is locked again however the probe ends.
fn probe_nuvoton(io: &Ports, port: u16) -> Option<(u16, u16)> {
    let found = (|| {
        io.outb(port, NUVOTON_UNLOCK)?;
        io.outb(port, NUVOTON_UNLOCK)?;
        let id = read_config_word(io, port, (REG_CHIP_ID_HIGH, REG_CHIP_ID_LOW))?;
        if id == 0x0000 || id == 0xFFFF {
            return None;
        }
        io.admit_bars();
        io.outb(port, REG_LOGICAL_DEVICE)?;
        io.outb(port + 1, LDN_EC_SPACE)?;
        let base = read_config_word(io, port, (REG_BASE_HIGH, REG_BASE_LOW))?;
        (base != 0 && base != 0xFFFF).then_some((id, base))
    })();
    // Never leave the chip unlocked.
    let _ = io.outb(port, NUVOTON_LOCK);
    found
}

fn probe_ite(io: &Ports, port: u16) -> Option<IteChip> {
    let found = (|| {
        for b in ITE_UNLOCK {
            io.outb(port, b)?;
        }
        io.outb(port, if port == 0x2E { 0x55 } else { 0xAA })?;
        let id = read_config_word(io, port, (REG_CHIP_ID_HIGH, REG_CHIP_ID_LOW))?;
        let (name, step) = ite_name(id)?;
        io.admit_bars();
        io.outb(port, REG_LOGICAL_DEVICE)?;
        io.outb(port + 1, LDN_ITE_EC)?;
        let base = read_config_word(io, port, (REG_BASE_HIGH, REG_BASE_LOW))?;
        (base != 0 && base != 0xFFFF).then_some(IteChip { name, port, base, step })
    })();
    // ITE's exit: write 0x02 to configuration register 0x02.
    let _ = io.outb(port, 0x02);
    let _ = io.outb(port + 1, 0x02);
    found
}

/// What one driver's probe came to.
enum Probe {
    Found(Box<SuperIo>),
    /// The driver works; there is just no chip it should read. Final: the other driver would
    /// find the same.
    Absent(String),
    /// The driver is not there; the other may be.
    Unavailable(String),
}

/// How a raw channel becomes a reading.
#[derive(Debug, Clone, Copy)]
enum Decode {
    /// NCT668x word: signed whole degrees in the high byte, 1/256 in the low.
    TempWord,
    /// Single signed byte, whole degrees.
    TempByte,
    /// Unsigned byte in whole degrees, rejected outside 1..125 (an open ITE channel parks at 0xC9).
    TempByteIte,
    /// Word times volts-per-count.
    VoltWord(f64),
    /// Byte times volts-per-count.
    VoltByte(f64),
    /// Word straight in RPM.
    RpmWord,
    /// ITE tachometer: a 16-bit count whose low byte is at the address's low byte and whose high
    /// byte is at its high byte (the two registers are not adjacent), as ITE's 1.35 MHz clock over
    /// two pulses a revolution. A count of 0 or 0xFFFF is a header with nothing turning on it.
    IteTach,
    /// Byte as a share of 255, in percent.
    DutyByte,
}

/// ITE's tachometer clock over two pulses a revolution.
const ITE_TACH_CLOCK: f64 = 1_350_000.0;

#[derive(Debug, Clone)]
struct Channel {
    key: &'static str,
    label: &'static str,
    group: Group,
    unit: &'static str,
    decimals: u8,
    /// NCT668x: EC address. NCT679x: (bank << 8) | register. ITE: register.
    address: u16,
    /// ITE only: which of the two chips.
    chip: Option<&'static str>,
    decode: Decode,
    band: (f64, f64),
    /// Fans that read zero are not shown until they have turned once.
    hide_until_nonzero: bool,
    /// The fan whose speed this duty belongs to.
    pairs_with: Option<&'static str>,
    note: &'static str,
}

/// The board a channel map was confirmed on. A map is picked by chip and CPU vendor, so another
/// board with the same chip gets it too; that board's wiring may differ, and nothing in the
/// readings would show it. So the map says where it was confirmed, and the board heading and
/// Sources say when this is not that board.
struct ConfirmedOn {
    /// As people name it.
    board: &'static str,
    /// SMBIOS board or system product names that identify it, compared whole (ignoring case),
    /// and also against either half of a "NAME (MS-XXXX)" product.
    products: &'static [&'static str],
}

static NCT668X_INTEL_BOARD: ConfirmedOn =
    ConfirmedOn { board: "MSI PRO Z790-P WIFI DDR4 (MS-7E06)", products: &["MS-7E06"] };
static NCT668X_Z790MPOWER_BOARD: ConfirmedOn =
    ConfirmedOn { board: "MSI Z790MPOWER (MS-7E01)", products: &["MS-7E01"] };
static NCT668X_AMD_BOARD: ConfirmedOn = ConfirmedOn { board: "MSI B850MPOWER", products: &["B850MPOWER"] };
static NCT679X_INTEL_BOARD: ConfirmedOn =
    ConfirmedOn { board: "ASUS ROG MAXIMUS Z790 APEX", products: &["ROG MAXIMUS Z790 APEX"] };
static ITE_INTEL_BOARD: ConfirmedOn =
    ConfirmedOn { board: "Gigabyte Z890 AORUS TACHYON ICE", products: &["Z890 AORUS TACHYON ICE"] };
static ITE_AMD_BOARD: ConfirmedOn =
    ConfirmedOn { board: "Gigabyte X870 AORUS TACHYON ICE", products: &["X870 AORUS TACHYON ICE"] };

/// The names this machine's SMBIOS gives its board: the board and system products, each whole and,
/// for MSI's "Z790MPOWER (MS-7E01)", split into the name and the model code.
fn product_names(smbios: &Smbios) -> Vec<String> {
    let mut out = Vec::new();
    for field in [&smbios.board_product, &smbios.system_product] {
        let f = field.trim().to_uppercase();
        if f.is_empty() {
            continue;
        }
        if let (Some(open), Some(close)) = (f.find('('), f.rfind(')'))
            && open < close
        {
            out.push(f[..open].trim().to_owned());
            out.push(f[open + 1..close].trim().to_owned());
        }
        out.push(f);
    }
    out
}

impl ConfirmedOn {
    fn matches(&self, smbios: &Smbios) -> bool {
        let names = product_names(smbios);
        self.products.iter().any(|p| names.iter().any(|n| n.eq_ignore_ascii_case(p)))
    }

    /// The board heading's note and the Sources sentence for a board the map was not confirmed
    /// on; `None` on the board it was.
    fn caveat(&self, smbios: &Smbios) -> Option<(String, String)> {
        if self.matches(smbios) {
            return None;
        }
        Some((
            format!("map from {}, unconfirmed here", self.board),
            format!(
                " Channel map confirmed on the {}, not on this board: which reading is which rail is the board maker's wiring, so check them against another tool before relying on them.",
                self.board
            ),
        ))
    }
}

/// The board heading's detail and the Sources line, with the map's caveat where it applies.
fn describe(chip: &str, status: String, confirmed: &ConfirmedOn, smbios: &Smbios) -> (String, String) {
    match confirmed.caveat(smbios) {
        Some((note, sentence)) => (format!("{chip} · {note}"), status + &sentence),
        None => (chip.to_owned(), status),
    }
}

const fn temp(key: &'static str, label: &'static str, address: u16, decode: Decode, band: (f64, f64)) -> Channel {
    Channel {
        key,
        label,
        group: Group::Temperatures,
        unit: "°C",
        decimals: 1,
        address,
        chip: None,
        decode,
        band,
        hide_until_nonzero: false,
        pairs_with: None,
        note: "",
    }
}

const fn volt(key: &'static str, label: &'static str, address: u16, decode: Decode, band: (f64, f64)) -> Channel {
    Channel {
        key,
        label,
        group: Group::Voltages,
        unit: "V",
        decimals: 3,
        address,
        chip: None,
        decode,
        band,
        hide_until_nonzero: false,
        pairs_with: None,
        note: "",
    }
}

const fn fan(key: &'static str, label: &'static str, address: u16) -> Channel {
    Channel {
        key,
        label,
        group: Group::Fans,
        unit: "RPM",
        decimals: 0,
        address,
        chip: None,
        decode: Decode::RpmWord,
        band: (0.0, 20000.0),
        hide_until_nonzero: true,
        pairs_with: None,
        note: "",
    }
}

const fn duty(key: &'static str, label: &'static str, address: u16, fan_key: &'static str) -> Channel {
    Channel {
        key,
        label,
        group: Group::FanDuty,
        unit: "%",
        decimals: 0,
        address,
        chip: None,
        decode: Decode::DutyByte,
        band: (0.0, 100.0),
        hide_until_nonzero: true,
        pairs_with: Some(fan_key),
        note: "",
    }
}

const fn noted(mut c: Channel, note: &'static str) -> Channel {
    c.note = note;
    c
}

const fn on(mut c: Channel, chip: &'static str) -> Channel {
    c.chip = Some(chip);
    c
}

// NCT668x voltage steps: the low five bits of every word read zero, and a BIOS change of exactly
// -60 mV moved a channel by exactly -480 counts, so a count is 1/8 mV. Channels behind a 2:1
// divider decode at half that.
const NCT668X_STEP: f64 = 0.000125;
const NCT668X_HALF: f64 = NCT668X_STEP / 2.0;

/// The fan headers' speed and duty registers are the chip's, not the board's: eight 16-bit
/// tachometers from 0x140 and eight duty bytes from 0x160. Header names are MSI's, the vendor that
/// fits this chip. Confirmed on the Z790-P bench by reading the block: the pump header at 2040 RPM
/// and the system fans at 1100-1300 RPM, the CPU header at 0 with nothing fitted.
const NCT668X_FANS: [Channel; 16] = [
    fan("fan/cpu", "CPU Fan", 0x140),
    fan("fan/pump", "Pump Fan", 0x142),
    fan("fan/sys1", "System Fan 1", 0x144),
    fan("fan/sys2", "System Fan 2", 0x146),
    fan("fan/sys3", "System Fan 3", 0x148),
    fan("fan/sys4", "System Fan 4", 0x14A),
    fan("fan/sys5", "System Fan 5", 0x14C),
    fan("fan/sys6", "System Fan 6", 0x14E),
    duty("duty/cpu", "CPU Fan", 0x160, "fan/cpu"),
    duty("duty/pump", "Pump Fan", 0x161, "fan/pump"),
    duty("duty/sys1", "System Fan 1", 0x162, "fan/sys1"),
    duty("duty/sys2", "System Fan 2", 0x163, "fan/sys2"),
    duty("duty/sys3", "System Fan 3", 0x164, "fan/sys3"),
    duty("duty/sys4", "System Fan 4", 0x165, "fan/sys4"),
    duty("duty/sys5", "System Fan 5", 0x166, "fan/sys5"),
    duty("duty/sys6", "System Fan 6", 0x167, "fan/sys6"),
];

/// MSI PRO Z790-P WIFI DDR4 (MS-7E06), NCT6687D at 0x4E, window 0x0A20. Each channel matched
/// against HWiNFO reading the same chip on the same boot, and Vcore and SA put through an
/// idle -> all-core load -> idle cycle as well (see Roch Viewer's intel_board_sensors).
fn nct668x_intel() -> Vec<Channel> {
    let mut v = vec![
        temp("temp/cpu", "CPU", 0x100, Decode::TempWord, (-20.0, 120.0)),
        temp("temp/system", "System", 0x102, Decode::TempWord, (-20.0, 100.0)),
        temp("temp/vrm", "VRM MOS", 0x104, Decode::TempWord, (-20.0, 125.0)),
        temp("temp/pch", "PCH", 0x106, Decode::TempWord, (-20.0, 120.0)),
        temp("temp/socket", "CPU Socket", 0x108, Decode::TempWord, (-20.0, 120.0)),
        noted(
            volt("volt/vcore", "Vcore", 0x124, Decode::VoltWord(NCT668X_HALF), (0.40, 1.80)),
            "droops under an all-core load and recovers: the only core rail in the block",
        ),
        noted(
            volt("volt/sa", "CPU SA", 0x12A, Decode::VoltWord(NCT668X_HALF), (0.60, 1.70)),
            "within 2 mV of HWiNFO's CPU SA and 1 mV of the VCCSA MCHBAR reports",
        ),
        noted(volt("volt/dram", "DRAM", 0x128, Decode::VoltWord(NCT668X_STEP), (0.90, 2.10)), "HWiNFO DRAM, exact"),
        noted(
            volt("volt/cpuaux", "CPU AUX", 0x12C, Decode::VoltWord(NCT668X_STEP), (0.80, 2.50)),
            "HWiNFO CPU AUX, 4 mV",
        ),
        // The supply rails sit behind fixed dividers that put each near 1 V at the ADC; the
        // multipliers are the ones LibreHardwareMonitor documents for this chip, and they land
        // the rails on 12.02, 5.01 and 3.35 V on the bench.
        noted(volt("volt/12v", "+12V", 0x120, Decode::VoltWord(NCT668X_HALF * 12.0), (10.0, 14.0)), "divider 12:1"),
        noted(volt("volt/5v", "+5V", 0x122, Decode::VoltWord(NCT668X_HALF * 5.0), (4.0, 6.0)), "divider 5:1"),
        noted(volt("volt/3v3", "+3.3V", 0x130, Decode::VoltWord(NCT668X_HALF), (2.9, 3.7)), "no divider"),
    ];
    v.extend(NCT668X_FANS.iter().cloned());
    v
}

/// MSI Z790MPOWER (MS-7E01), DDR5, NCT6687D at 0x4E, window 0x0A20. The PRO Z790-P's layout with
/// one difference: the input that board feeds from its DRAM rail carries CPU VDD2 here, as DDR5
/// modules make their own VDD and VDDQ on their PMICs (1.440 and 1.410 V on this machine, against
/// 1.376 V on this input). Every channel matched against HWiNFO 8.50 reading the same chip on the
/// same boot, idle and with the GPU loaded.
fn nct668x_z790mpower() -> Vec<Channel> {
    let mut v = vec![
        temp("temp/cpu", "CPU", 0x100, Decode::TempWord, (-20.0, 120.0)),
        temp("temp/system", "System", 0x102, Decode::TempWord, (-20.0, 100.0)),
        temp("temp/vrm", "VRM MOS", 0x104, Decode::TempWord, (-20.0, 125.0)),
        temp("temp/pch", "PCH", 0x106, Decode::TempWord, (-20.0, 120.0)),
        temp("temp/socket", "CPU Socket", 0x108, Decode::TempWord, (-20.0, 120.0)),
        noted(
            volt("volt/vcore", "Vcore", 0x124, Decode::VoltWord(NCT668X_HALF), (0.40, 1.80)),
            "HWiNFO Vcore 1.274, exact",
        ),
        noted(
            volt("volt/sa", "CPU SA", 0x12A, Decode::VoltWord(NCT668X_HALF), (0.60, 1.70)),
            "HWiNFO CPU SA 1.184, exact; 7 mV under the SA VID in HWiNFO too",
        ),
        noted(
            volt("volt/vdd2", "CPU VDD2", 0x128, Decode::VoltWord(NCT668X_STEP), (0.90, 1.60)),
            "HWiNFO CPU VDD2 1.376, exact; DRAM on the DDR4 PRO Z790-P",
        ),
        noted(
            volt("volt/cpuaux", "CPU AUX", 0x12C, Decode::VoltWord(NCT668X_STEP), (0.80, 2.50)),
            "HWiNFO CPU AUX 1.792, exact",
        ),
        noted(
            volt("volt/12v", "+12V", 0x120, Decode::VoltWord(NCT668X_HALF * 12.0), (10.0, 14.0)),
            "HWiNFO +12V 12.024, exact",
        ),
        noted(
            volt("volt/5v", "+5V", 0x122, Decode::VoltWord(NCT668X_HALF * 5.0), (4.0, 6.0)),
            "HWiNFO +5V 5.060, exact",
        ),
        noted(
            volt("volt/3v3", "+3.3V", 0x130, Decode::VoltWord(NCT668X_HALF), (2.9, 3.7)),
            "HWiNFO +3.3V 3.340, exact",
        ),
    ];
    v.extend(NCT668X_FANS.iter().cloned());
    v
}

/// The Nuvoton map for this board and chip, and where it was confirmed: a board's own map where
/// one is confirmed, otherwise the chip family's first one, which the caveat then flags.
fn nuvoton_map(family: Family, vendor: Vendor, smbios: &Smbios) -> (Vec<Channel>, &'static ConfirmedOn) {
    match (family, vendor) {
        (Family::Nct668x, Vendor::Amd) => (nct668x_amd(), &NCT668X_AMD_BOARD),
        (Family::Nct668x, _) if NCT668X_Z790MPOWER_BOARD.matches(smbios) => {
            (nct668x_z790mpower(), &NCT668X_Z790MPOWER_BOARD)
        }
        (Family::Nct668x, _) => (nct668x_intel(), &NCT668X_INTEL_BOARD),
        (Family::Nct679x, _) => (nct679x_intel(), &NCT679X_INTEL_BOARD),
    }
}

/// MSI B850MPOWER (AM5), NCT6687D at 0x4E, window 0x0A20. The three thermistors were each put
/// through a full load cycle (CPU tracked Tctl within 0.5 °C); PCH (flat 29.0 through a 34 °C
/// swing) and the socket (reads 0.0, header not populated) were left out. VDDIO was pinned by a
/// -60 mV BIOS change moving it exactly -480 counts; VTT by HWiNFO's min and max both matching.
fn nct668x_amd() -> Vec<Channel> {
    let mut v = vec![
        temp("temp/cpu", "CPU", 0x100, Decode::TempWord, (-20.0, 120.0)),
        temp("temp/system", "System", 0x102, Decode::TempWord, (-20.0, 100.0)),
        temp("temp/vrm", "VRM MOS", 0x104, Decode::TempWord, (-20.0, 125.0)),
        noted(
            volt("volt/vddio", "CPU VDDIO / MEM", 0x126, Decode::VoltWord(NCT668X_STEP), (0.90, 1.80)),
            "BIOS -60 mV = -480 counts",
        ),
        noted(
            volt("volt/vtt", "VTT", 0x136, Decode::VoltWord(NCT668X_HALF), (1.50, 2.10)),
            "HWiNFO min and max, both exact",
        ),
    ];
    v.extend(NCT668X_FANS.iter().cloned());
    v
}

/// ASUS ROG MAXIMUS Z790 APEX, NCT6798D (0xD42B at 0x2E, window 0x290). Single bytes addressed as
/// (bank << 8) | register. The block is not uniformly 8 mV: Vcore steps 9 mV and VDD2 18 mV, both
/// pinned against HWiNFO's own ranges. VRM and socket are not on this chip on that board.
fn nct679x_intel() -> Vec<Channel> {
    vec![
        temp("temp/cpu", "CPU", 0x491, Decode::TempByte, (-20.0, 120.0)),
        temp("temp/pch", "PCH", 0x401, Decode::TempByte, (-20.0, 120.0)),
        temp("temp/system", "Motherboard", 0x027, Decode::TempByte, (-20.0, 100.0)),
        noted(
            volt("volt/vcore", "Vcore", 0x480, Decode::VoltByte(0.009), (0.40, 1.80)),
            "0x92 -> 1.314 V, HWiNFO exact",
        ),
        noted(volt("volt/vtt", "VTT", 0x489, Decode::VoltByte(0.016), (0.60, 1.60)), "HWiNFO min and max matched"),
        noted(
            volt("volt/vdd2", "VDD2 (IMC VDD)", 0x48A, Decode::VoltByte(0.018), (0.90, 1.60)),
            "two-count range fixes 18 mV",
        ),
        noted(volt("volt/sa", "CPU SA", 0x48D, Decode::VoltByte(0.016), (0.60, 1.70)), "HWiNFO IVR VCCSA, exact"),
        noted(volt("volt/cpuaux", "CPU AUX", 0x48E, Decode::VoltByte(0.016), (0.80, 2.50)), "HWiNFO VCCIN_AUX, exact"),
    ]
}

const fn ite_fan(key: &'static str, label: &'static str, low: u8, high: u8) -> Channel {
    Channel {
        key,
        label,
        group: Group::Fans,
        unit: "RPM",
        decimals: 0,
        address: ((high as u16) << 8) | low as u16,
        chip: None,
        decode: Decode::IteTach,
        band: (0.0, 20000.0),
        hide_until_nonzero: true,
        pairs_with: None,
        note: "",
    }
}

// In the ITE maps a voltage channel's `VoltByte` holds the input's divider, 1.0 for none: the two
// parts do not share an LSB, so the chip's step is only known once the chip has identified, and
// the probe multiplies it in then.

/// Gigabyte Z890 AORUS TACHYON ICE: IT8696E at 0x2E (12 mV) and IT87952E at 0x4E (10.9 mV).
fn ite_intel() -> Vec<Channel> {
    vec![
        on(temp("temp/system", "System 1", 0x29, Decode::TempByteIte, (1.0, 125.0)), "IT8696E"),
        on(temp("temp/pch", "PCH", 0x2A, Decode::TempByteIte, (1.0, 125.0)), "IT8696E"),
        on(temp("temp/cpu", "CPU", 0x2B, Decode::TempByteIte, (1.0, 125.0)), "IT8696E"),
        on(temp("temp/vrm", "VRM MOS", 0x2D, Decode::TempByteIte, (1.0, 125.0)), "IT8696E"),
        on(
            noted(
                volt("volt/vcore", "Vcore", 0x20, Decode::VoltByte(1.0), (0.40, 1.80)),
                "inside HWiNFO's DLVRin range",
            ),
            "IT8696E",
        ),
        on(
            noted(volt("volt/sa", "VCCSA", 0x26, Decode::VoltByte(1.0), (0.60, 1.70)), "HWiNFO VCCSA 1.308, exact"),
            "IT8696E",
        ),
        on(volt("volt/vnnaon", "CPU VNNAON", 0x25, Decode::VoltByte(1.0), (0.40, 1.40)), "IT8696E"),
        on(
            noted(volt("volt/vdd2", "VDD2", 0x21, Decode::VoltByte(1.0), (0.90, 1.60)), "HWiNFO CPU VDD2 1.111"),
            "IT87952E",
        ),
        on(volt("volt/vccio", "VCCIO", 0x24, Decode::VoltByte(1.0), (0.80, 1.80)), "IT87952E"),
    ]
}

/// Gigabyte X870 AORUS TACHYON ICE (AM5): IT8696E alone, at 0x2E, window 0x0A40, 12 mV a count.
///
/// The same chip as the Z890 board's first one and the same five thermistors, but the voltage
/// inputs carry AMD's rails: what the Z890 map calls VCCSA (0x26) and CPU VNNAON (0x25) read
/// 1.428 and 1.116 V here, which HWiNFO 8.52 names CPU VDDIO_MEM and CPU VCORE MISC on the same
/// boot, and 0x24 reads its CPU VCORE SoC (1.272 V). Those two CPU rails sit 8 and 16 mV off the
/// SVI3 telemetry the SMU reports for VDDCR_SOC (1.280) and VDD_MISC (1.100), the board measuring
/// after its own drop. The supplies and the two fitted fans were matched by Roch Viewer on this
/// board (`am5_board_rails`): +12V 168 counts x 6 = 12.096 V, +3.3V x 1.65, +5V x 2.5, 3VSB, VBAT
/// and AVCC3 x 2, each within a count of the reference; the CPU and CPU_OPT headers at 1558 and
/// 3609 RPM in HWiNFO. The other four fan headers read 0xFFFF with nothing fitted, so which System
/// Fan is which has not been seen, and they stay hidden until one turns.
fn ite_x870_tachyon() -> Vec<Channel> {
    const IT: &str = "IT8696E";
    vec![
        on(temp("temp/system", "System 1", 0x29, Decode::TempByteIte, (1.0, 125.0)), IT),
        on(temp("temp/pch", "PCH", 0x2A, Decode::TempByteIte, (1.0, 125.0)), IT),
        on(temp("temp/cpu", "CPU", 0x2B, Decode::TempByteIte, (1.0, 125.0)), IT),
        on(
            noted(temp("temp/pciex16", "PCIEX16", 0x2C, Decode::TempByteIte, (1.0, 125.0)), "HWiNFO PCIEX16 34, exact"),
            IT,
        ),
        on(temp("temp/vrm", "VRM MOS", 0x2D, Decode::TempByteIte, (1.0, 125.0)), IT),
        on(noted(volt("volt/vcore", "Vcore", 0x20, Decode::VoltByte(1.0), (0.30, 1.80)), "HWiNFO Vcore"), IT),
        on(
            noted(
                volt("volt/vcore_soc", "CPU VCORE SoC", 0x24, Decode::VoltByte(1.0), (0.60, 1.50)),
                "HWiNFO 1.272, exact",
            ),
            IT,
        ),
        on(
            noted(
                volt("volt/vcore_misc", "CPU VCORE MISC", 0x25, Decode::VoltByte(1.0), (0.60, 1.50)),
                "HWiNFO 1.116, exact",
            ),
            IT,
        ),
        on(
            noted(
                volt("volt/vddio", "CPU VDDIO_MEM", 0x26, Decode::VoltByte(1.0), (0.90, 1.80)),
                "HWiNFO 1.428, exact; the memory controller's I/O rail, not the DIMMs' own",
            ),
            IT,
        ),
        on(noted(volt("volt/12v", "+12V", 0x22, Decode::VoltByte(6.0), (10.8, 13.2)), "divider 6:1"), IT),
        on(noted(volt("volt/5v", "+5V", 0x23, Decode::VoltByte(2.5), (4.5, 5.5)), "divider 2.5:1"), IT),
        on(noted(volt("volt/3v3", "+3.3V", 0x21, Decode::VoltByte(1.65), (3.0, 3.6)), "divider 1.65:1"), IT),
        on(noted(volt("volt/3vsb", "3VSB", 0x27, Decode::VoltByte(2.0), (3.0, 3.6)), "divider 2:1"), IT),
        on(noted(volt("volt/vbat", "VBAT", 0x28, Decode::VoltByte(2.0), (2.0, 3.6)), "divider 2:1"), IT),
        on(noted(volt("volt/avcc3", "AVCC3", 0x2F, Decode::VoltByte(2.0), (3.0, 3.6)), "divider 2:1"), IT),
        on(ite_fan("fan/cpu", "CPU Fan", 0x0D, 0x18), IT),
        on(ite_fan("fan/sys1", "System Fan 1", 0x0E, 0x19), IT),
        on(ite_fan("fan/sys2", "System Fan 2", 0x0F, 0x1A), IT),
        on(ite_fan("fan/sys3", "System Fan 3", 0x80, 0x81), IT),
        on(ite_fan("fan/cpu_opt", "CPU_OPT", 0x82, 0x83), IT),
        on(ite_fan("fan/fan6", "Fan 6", 0x84, 0x85), IT),
    ]
}

/// The ITE map for this machine, and where it was confirmed. As with the Nuvoton maps the CPU's
/// vendor decides the rails: an AM5 board's inputs carry AMD's, so an AMD machine never gets the
/// Z890 board's Intel names, and each map's caveat flags any board it was not confirmed on.
fn ite_map(vendor: Vendor) -> (Vec<Channel>, &'static ConfirmedOn) {
    match vendor {
        Vendor::Amd => (ite_x870_tachyon(), &ITE_AMD_BOARD),
        _ => (ite_intel(), &ITE_INTEL_BOARD),
    }
}

/// Each chip with the ports that reach it: one PawnIO handle per config slot, or inpoutx64.
enum Transport {
    Nuvoton(NuvotonChip, Ports),
    Ite(Vec<(IteChip, Ports)>),
}

pub struct SuperIo {
    transport: Transport,
    channels: Vec<Channel>,
    /// Fans seen turning at least once, by key.
    spun: std::collections::HashSet<&'static str>,
    device: DeviceInfo,
    /// The chip as each reading's source names it ("Nuvoton NCT6687D"), without the map's caveat.
    chip: String,
    status: String,
}

const DEVICE: &str = "board";

pub fn board_device(sys: &SystemInfo, detail: &str) -> DeviceInfo {
    DeviceInfo::new(DEVICE, DeviceKind::Board, sys.board_name()).detail(detail)
}

impl SuperIo {
    pub fn detect(sys: &SystemInfo, out: &mut Batch, driver_choice: Driver) -> Option<SuperIo> {
        let mut unavailable = Vec::new();
        #[cfg(windows)]
        if driver_choice.pawnio() {
            let open = |port| crate::hw::pawnio::PortModule::lpc(port).map(Ports::PawnIo);
            match Self::probe_with(sys, &open) {
                Probe::Found(s) => return Some(*s),
                Probe::Absent(why) => {
                    out.status(why);
                    return None;
                }
                Probe::Unavailable(e) => unavailable.push(e),
            }
        }
        if driver_choice.inpout() {
            match driver() {
                Ok(io) => match Self::probe_with(sys, &|_| Ok(Ports::InpOut(io))) {
                    Probe::Found(s) => return Some(*s),
                    Probe::Absent(why) => {
                        out.status(why);
                        return None;
                    }
                    Probe::Unavailable(e) => unavailable.push(e),
                },
                Err(e) => unavailable.push(e.to_owned()),
            }
        }
        out.status(format!("Board sensors unavailable: {}.", unavailable.join("; ")));
        None
    }

    /// Probe both config ports through one driver, `open` giving the ports for each.
    fn probe_with(sys: &SystemInfo, open: &dyn Fn(u16) -> Result<Ports, String>) -> Probe {
        let mut handles = Vec::new();
        for port in CONFIG_PORTS {
            match open(port) {
                Ok(io) => handles.push((port, io)),
                Err(e) => return Probe::Unavailable(e),
            }
        }
        let driver = handles[0].1.driver();
        let Some(bus) = mutex::isa().lock(mutex::WAIT_MS) else {
            return Probe::Absent("Board sensors: another tool held the ISA bus through the whole probe.".into());
        };
        let vendor = sys.vendor();
        let mut declined = Vec::new();
        let mut nuvoton = None;
        let mut answered = false;
        for (i, (port, io)) in handles.iter().enumerate() {
            let Some((id, base)) = probe_nuvoton(io, *port) else { continue };
            answered = true;
            let chip = if let Some(name) = nct668x_name(id) {
                NuvotonChip { family: Family::Nct668x, port: *port, id, name, base }
            } else if let Some(name) = nct679x_name(id) {
                NuvotonChip { family: Family::Nct679x, port: *port, id, name, base }
            } else {
                declined.push(format!("0x{id:04X} at 0x{port:02X}"));
                continue;
            };
            nuvoton = Some((i, chip));
            break;
        }
        // Only when no Nuvoton part answered: the ITE unlock is a different byte sequence on the
        // same ports, and there is no reason to send it to a chip that has already identified.
        let mut ite = Vec::new();
        if !answered {
            for (i, (port, io)) in handles.iter().enumerate() {
                if let Some(chip) = probe_ite(io, *port) {
                    ite.push((i, chip));
                }
            }
        }
        drop(bus);

        if let Some((i, chip)) = nuvoton {
            let io = handles.swap_remove(i).1;
            let (channels, confirmed) = nuvoton_map(chip.family, vendor, &sys.smbios);
            let name = format!("Nuvoton {}", chip.name);
            let status = format!(
                "Super I/O: Nuvoton {} (0x{:04X}) at 0x{:02X}, sensor window 0x{:04X}, through {driver}.",
                chip.name, chip.id, chip.port, chip.base
            );
            let (detail, status) = describe(&name, status, confirmed, &sys.smbios);
            return Probe::Found(Box::new(SuperIo {
                device: board_device(sys, &detail),
                chip: name,
                transport: Transport::Nuvoton(chip, io),
                channels,
                spun: Default::default(),
                status,
            }));
        }
        if !ite.is_empty() {
            let names: Vec<&str> = ite.iter().map(|(_, c)| c.name).collect();
            let name = format!("ITE {}", names.join(" + "));
            let where_: Vec<String> =
                ite.iter().map(|(_, c)| format!("{} at 0x{:02X} window 0x{:04X}", c.name, c.port, c.base)).collect();
            let (map, confirmed) = ite_map(vendor);
            let channels = map
                .into_iter()
                .filter(|c| c.chip.is_some_and(|n| names.contains(&n)))
                .map(|mut c| {
                    let step = ite.iter().find(|(_, i)| Some(i.name) == c.chip).map_or(0.0, |(_, i)| i.step);
                    if let Decode::VoltByte(divider) = c.decode {
                        c.decode = Decode::VoltByte(step * divider);
                    }
                    c
                })
                .collect();
            // Move each chip's handle out, highest index first so the others stay where they are.
            let mut chips = Vec::new();
            for (i, chip) in ite.into_iter().rev() {
                chips.push((chip, handles.swap_remove(i).1));
            }
            chips.reverse();
            let status = format!("Super I/O: {}, through {driver}.", where_.join(", "));
            let (detail, status) = describe(&name, status, confirmed, &sys.smbios);
            return Probe::Found(Box::new(SuperIo {
                device: board_device(sys, &detail),
                chip: name,
                status,
                transport: Transport::Ite(chips),
                channels,
                spun: Default::default(),
            }));
        }
        Probe::Absent(if declined.is_empty() {
            "Board sensors: no supported Super I/O answered on 0x2E or 0x4E.".to_owned()
        } else {
            format!(
                "Board sensors: declined Super I/O {} (no confirmed register map; reading it would print plausible numbers that are not measurements).",
                declined.join(", ")
            )
        })
    }

    /// Read every channel's raw value under one acquisition of the bus. A channel whose port
    /// access failed is `None`, not a byte.
    fn read_raw(&self) -> Option<Vec<Option<u32>>> {
        let _bus = mutex::isa().lock(mutex::WAIT_MS)?;
        let raws = match &self.transport {
            Transport::Nuvoton(chip, io) => {
                let read_byte = |address: u16| -> Option<u8> {
                    match chip.family {
                        Family::Nct668x => {
                            // The 0xFF preamble on the page register is required; without it
                            // every read returns the same byte.
                            io.outb(chip.base + 4, 0xFF)?;
                            io.outb(chip.base + 4, (address >> 8) as u8)?;
                            io.outb(chip.base + 5, address as u8)?;
                            io.inb(chip.base + 6)
                        }
                        Family::Nct679x => {
                            io.outb(chip.base + 5, 0x4E)?;
                            io.outb(chip.base + 6, (address >> 8) as u8)?;
                            io.outb(chip.base + 5, address as u8)?;
                            io.inb(chip.base + 6)
                        }
                    }
                };
                self.channels
                    .iter()
                    .map(|c| match c.decode {
                        Decode::TempWord | Decode::VoltWord(_) | Decode::RpmWord => {
                            Some(((read_byte(c.address)? as u32) << 8) | read_byte(c.address + 1)? as u32)
                        }
                        _ => read_byte(c.address).map(u32::from),
                    })
                    .collect()
            }
            Transport::Ite(chips) => self
                .channels
                .iter()
                .map(|c| {
                    let (chip, io) = chips.iter().find(|(i, _)| Some(i.name) == c.chip)?;
                    let read = |register: u8| -> Option<u32> {
                        io.outb(chip.base + 5, register)?;
                        io.inb(chip.base + 6).map(u32::from)
                    };
                    match c.decode {
                        Decode::IteTach => {
                            let low = read(c.address as u8)?;
                            Some((read((c.address >> 8) as u8)? << 8) | low)
                        }
                        _ => read(c.address as u8),
                    }
                })
                .collect(),
        };
        Some(raws)
    }
}

fn decode(raw: u32, d: Decode) -> f64 {
    match d {
        Decode::TempWord => {
            let whole = ((raw >> 8) & 0xFF) as u8 as i8 as f64;
            whole + (raw & 0xFF) as f64 / 256.0
        }
        Decode::TempByte => (raw & 0xFF) as u8 as i8 as f64,
        Decode::TempByteIte => (raw & 0xFF) as f64,
        Decode::VoltWord(step) => (raw & 0xFFFF) as f64 * step,
        Decode::VoltByte(step) => (raw & 0xFF) as f64 * step,
        Decode::RpmWord => {
            let v = raw & 0xFFFF;
            if v == 0xFFFF { f64::NAN } else { v as f64 }
        }
        Decode::IteTach => {
            let count = raw & 0xFFFF;
            if count == 0 || count == 0xFFFF { f64::NAN } else { (ITE_TACH_CLOCK / (2.0 * count as f64)).round() }
        }
        Decode::DutyByte => (raw & 0xFF) as f64 / 255.0 * 100.0,
    }
}

impl Source for SuperIo {
    fn name(&self) -> &str {
        "Super I/O"
    }

    fn poll(&mut self, out: &mut Batch) {
        out.device(self.device.clone());
        out.status(self.status.clone());
        let Some(raws) = self.read_raw() else {
            out.status(format!("{} (bus busy: another tool held the ISA mutex, sample skipped)", self.status));
            return;
        };
        let chip = self.chip.clone();
        for (c, raw) in self.channels.iter().zip(raws) {
            let Some(raw) = raw else { continue };
            let mut value = decode(raw, c.decode);
            if !(c.band.0..=c.band.1).contains(&value) {
                value = f64::NAN;
            }
            if c.hide_until_nonzero {
                let gate = c.pairs_with.unwrap_or(c.key);
                if c.pairs_with.is_none() && value.is_finite() && value > 0.0 {
                    self.spun.insert(c.key);
                }
                if !self.spun.contains(gate) {
                    continue;
                }
            }
            let where_ = match c.chip {
                Some(n) => format!("{n} 0x{:02X}", c.address),
                None => format!("{chip} 0x{:04X}", c.address),
            };
            let source = if c.note.is_empty() { where_ } else { format!("{where_} — {}", c.note) };
            let info = SensorInfo::new(DEVICE, c.key, c.group, c.label, c.unit, c.decimals).source(source);
            out.put(&info, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_z790_p_capture_decodes_to_hwinfos_numbers() {
        // Words read off this machine's NCT6687D (see Roch Viewer's STEP_REFERENCE_CAPTURE).
        assert!((decode(0x51A0, Decode::VoltWord(NCT668X_HALF)) - 1.306).abs() < 0.001); // Vcore
        assert!((decode(0x3120, Decode::VoltWord(NCT668X_STEP)) - 1.572).abs() < 0.001); // DRAM
        assert!((decode(0x38C0, Decode::VoltWord(NCT668X_STEP)) - 1.816).abs() < 0.001); // CPU AUX
        assert!((decode(0x3EA0, Decode::VoltWord(NCT668X_HALF * 12.0)) - 12.024).abs() < 0.001); // +12V
        assert!((decode(0xD180, Decode::VoltWord(NCT668X_HALF)) - 3.352).abs() < 0.001); // +3.3V
        assert_eq!(decode(0x2680, Decode::TempWord), 38.5);
        assert_eq!(decode(0x07F8, Decode::RpmWord), 2040.0);
        assert!((decode(0x99, Decode::DutyByte) - 60.0).abs() < 0.01);
    }

    #[test]
    fn negative_and_absent_readings_are_not_numbers() {
        assert_eq!(decode(0xFB00, Decode::TempWord), -5.0);
        assert_eq!(decode(0xFE, Decode::TempByte), -2.0);
        assert!(decode(0xFFFF, Decode::RpmWord).is_nan());
        // An NCT679x answering the NCT668x protocol reads 0xFFFF: -0.004 °C, which the band
        // would accept as a temperature. That is why the chip gate exists, not the band.
        assert!((decode(0xFFFF, Decode::TempWord) + 0.004).abs() < 0.001);
    }

    #[test]
    fn the_nct6798d_steps_differ_per_channel() {
        assert!((decode(0x92, Decode::VoltByte(0.009)) - 1.314).abs() < 1e-9);
        assert!((decode(0x4C, Decode::VoltByte(0.018)) - 1.368).abs() < 1e-9);
    }

    #[test]
    fn only_known_chips_are_spoken_to() {
        assert_eq!(nct668x_name(0xD592), Some("NCT6687D"));
        assert_eq!(nct668x_name(0xD42B), None);
        assert_eq!(nct679x_name(0xD42B), Some("NCT6798D"));
        assert_eq!(ite_name(0x8696).map(|c| c.0), Some("IT8696E"));
        assert_eq!(ite_name(0x8688), None);
    }

    fn board(board_product: &str, system_product: &str) -> Smbios {
        Smbios { board_product: board_product.into(), system_product: system_product.into(), ..Default::default() }
    }

    #[test]
    fn a_map_knows_the_board_it_was_confirmed_on() {
        // MSI writes "NAME (MS-XXXX)" on the board and the bare code as the system.
        assert!(NCT668X_INTEL_BOARD.matches(&board("PRO Z790-P WIFI DDR4(MS-7E06)", "MS-7E06")));
        assert!(NCT668X_INTEL_BOARD.matches(&board("pro z790-p wifi ddr4 (ms-7e06)", "")));
        assert!(NCT668X_AMD_BOARD.matches(&board("B850MPOWER (MS-0000)", "MS-0000")));
        assert!(NCT679X_INTEL_BOARD.matches(&board("ROG MAXIMUS Z790 APEX", "System Product Name")));
        assert!(ITE_INTEL_BOARD.matches(&board("Z890 AORUS TACHYON ICE", "Z890 AORUS TACHYON ICE")));
    }

    #[test]
    fn another_board_with_the_same_chip_is_told_the_map_is_borrowed() {
        // The same NCT6687D, an MSI board no map was confirmed on: whole names only, so neither the
        // code nor a longer name that contains the confirmed one passes.
        let other = board("MAG Z790 TOMAHAWK WIFI (MS-7D91)", "MS-7D91");
        let (channels, confirmed) = nuvoton_map(Family::Nct668x, Vendor::Intel, &other);
        assert!(std::ptr::eq(confirmed, &NCT668X_INTEL_BOARD));
        assert!(channels.iter().any(|c| c.key == "volt/dram"));
        let (detail, status) = describe("Nuvoton NCT6687D", "Super I/O: found.".into(), confirmed, &other);
        assert_eq!(detail, "Nuvoton NCT6687D · map from MSI PRO Z790-P WIFI DDR4 (MS-7E06), unconfirmed here");
        assert!(status.starts_with("Super I/O: found. Channel map confirmed on the MSI PRO Z790-P"), "{status}");
        assert!(!NCT679X_INTEL_BOARD.matches(&board("ROG MAXIMUS Z790 APEX ENCORE", "")));
        assert!(!NCT668X_INTEL_BOARD.matches(&Smbios::default()), "an unnamed board is not the confirmed one");

        let home = board("PRO Z790-P WIFI DDR4 (MS-7E06)", "MS-7E06");
        let (detail, status) = describe("Nuvoton NCT6687D", "Super I/O: found.".into(), &NCT668X_INTEL_BOARD, &home);
        assert_eq!((detail.as_str(), status.as_str()), ("Nuvoton NCT6687D", "Super I/O: found."));
    }

    #[test]
    fn the_z790mpower_gets_its_own_map_with_vdd2_where_the_ddr4_board_has_dram() {
        let z790m = board("Z790MPOWER (MS-7E01)", "MS-7E01");
        let (channels, confirmed) = nuvoton_map(Family::Nct668x, Vendor::Intel, &z790m);
        assert!(std::ptr::eq(confirmed, &NCT668X_Z790MPOWER_BOARD));
        assert!(confirmed.caveat(&z790m).is_none(), "its own board: no caveat");
        let vdd2 = channels.iter().find(|c| c.address == 0x128).unwrap();
        assert_eq!((vdd2.key, vdd2.label), ("volt/vdd2", "CPU VDD2"));
        assert!(!channels.iter().any(|c| c.key == "volt/dram"));
        // HWiNFO's 1.376 V on that input, through the same decode.
        assert!((decode(0x2B00, vdd2.decode) - 1.376).abs() < 0.001);
        // Every other address is the PRO Z790-P's, with the same keys.
        let pro: Vec<(&str, u16)> =
            nct668x_intel().iter().filter(|c| c.address != 0x128).map(|c| (c.key, c.address)).collect();
        let mine: Vec<(&str, u16)> =
            channels.iter().filter(|c| c.address != 0x128).map(|c| (c.key, c.address)).collect();
        assert_eq!(pro, mine);
        // An AMD machine with the same board code still gets the AMD map.
        assert!(std::ptr::eq(nuvoton_map(Family::Nct668x, Vendor::Amd, &z790m).1, &NCT668X_AMD_BOARD));
    }

    /// An ITE map with the IT8696E's 12 mV step multiplied in, as the probe does.
    fn resolved(map: Vec<Channel>) -> Vec<Channel> {
        map.into_iter()
            .map(|mut c| {
                if let Decode::VoltByte(divider) = c.decode {
                    c.decode = Decode::VoltByte(0.012 * divider);
                }
                c
            })
            .collect()
    }

    #[test]
    fn the_x870_tachyon_decodes_to_hwinfos_numbers() {
        let tachyon = board("X870 AORUS TACHYON ICE", "X870 AORUS TACHYON ICE");
        let (map, confirmed) = ite_map(Vendor::Amd);
        assert!(std::ptr::eq(confirmed, &ITE_AMD_BOARD));
        assert!(confirmed.caveat(&tachyon).is_none(), "its own board: no caveat");
        let channels = resolved(map);
        let at = |key: &str, raw: u32| {
            let c = channels.iter().find(|c| c.key == key).unwrap();
            decode(raw, c.decode)
        };
        // Raw counts off the bench beside HWiNFO 8.52's IT8696E block.
        assert!((at("volt/vcore_soc", 106) - 1.272).abs() < 1e-9);
        assert!((at("volt/vcore_misc", 93) - 1.116).abs() < 1e-9);
        assert!((at("volt/vddio", 119) - 1.428).abs() < 1e-9);
        assert!((at("volt/12v", 168) - 12.096).abs() < 1e-9);
        assert!((at("volt/3v3", 167) - 3.3066).abs() < 1e-9);
        assert!((at("volt/5v", 161) - 4.830).abs() < 1e-9);
        assert!((at("volt/3vsb", 139) - 3.336).abs() < 1e-9);
        assert!((at("volt/vbat", 126) - 3.024).abs() < 1e-9);
        assert!((at("volt/avcc3", 128) - 3.072).abs() < 1e-9);
        assert_eq!(at("temp/pciex16", 34), 34.0);
        // CPU_OPT: 187 counts is HWiNFO's 3609 RPM; the CPU header's 433 counts its 1558.
        assert_eq!(at("fan/cpu_opt", 187), 3610.0);
        assert_eq!(at("fan/cpu", (1 << 8) | 177), 1559.0);
        assert!(at("fan/sys1", 0xFFFF).is_nan(), "a header with nothing fitted");
        assert!(at("fan/sys1", 0).is_nan());
        // No Intel rail name survives on an AMD board.
        for intel in ["VCCSA", "CPU VNNAON", "VDD2", "VCCIO"] {
            assert!(!channels.iter().any(|c| c.label == intel), "{intel}");
        }
    }

    #[test]
    fn each_vendor_gets_its_own_ite_map_and_keeps_the_caveat_elsewhere() {
        let (intel, confirmed) = ite_map(Vendor::Intel);
        assert!(std::ptr::eq(confirmed, &ITE_INTEL_BOARD));
        assert!(intel.iter().any(|c| c.label == "VCCSA"));
        // The Intel map's voltages still resolve to the chip's own step.
        let sa = resolved(intel).into_iter().find(|c| c.key == "volt/sa").unwrap();
        assert!((decode(109, sa.decode) - 1.308).abs() < 1e-9);
        let other_am5 = board("X870E AORUS MASTER", "X870E AORUS MASTER");
        let (detail, _) = describe("ITE IT8696E", "Super I/O: found.".into(), &ITE_AMD_BOARD, &other_am5);
        assert_eq!(detail, "ITE IT8696E · map from Gigabyte X870 AORUS TACHYON ICE, unconfirmed here");
        // Every ITE fan's two registers are distinct and both inside the chip's file.
        for c in ite_x870_tachyon().iter().filter(|c| matches!(c.decode, Decode::IteTach)) {
            assert_ne!(c.address & 0xFF, c.address >> 8, "{}", c.key);
        }
    }

    #[test]
    fn every_fan_duty_pairs_with_a_fan() {
        let fans: Vec<&str> = NCT668X_FANS.iter().filter(|c| c.group == Group::Fans).map(|c| c.key).collect();
        for d in NCT668X_FANS.iter().filter(|c| c.group == Group::FanDuty) {
            assert!(fans.contains(&d.pairs_with.unwrap()));
        }
    }
}
