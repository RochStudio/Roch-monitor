//! The SMBus host controller the DIMMs hang off: Intel's PCH (Roch Viewer's `intel_pch_smbus`) or
//! AMD's FCH (`fch_smbus`). Both are the PIIX4 register layout; only how the I/O base is found
//! differs. Intel publishes it in the SMBus function's PCI config space (00:1F.4 BAR4), AMD
//! through the PMIO index/data ports, and AMD has a second controller 0x20 above the first.
//!
//! SAFETY, as Roch Viewer draws it:
//!
//!   * Every read assembles the address byte with the read direction bit set, from a path that has
//!     no direction parameter.
//!   * Two device classes, two read protocols, two allowlists: Word Data for the JEDEC thermal
//!     sensors at 0x18-0x1F, Byte Data for the DDR5 PMIC and SPD hub at 0x48-0x57.
//!   * Exactly one writable target: a PMIC's ADC channel selector, R30h, which picks what R31h
//!     converts and stores nothing. The VID registers that move VDD/VDDQ/VPP (0x21, 0x25, 0x27) and
//!     the mode register (0x2B) are in no allowlist. Nothing writes to an SPD hub at all.
//!   * The controller's status is cleared on the way out, including on error, so the next owner
//!     of the mutex inherits a clean controller.

use std::time::{Duration, Instant};

use crate::hw::inpout::InpOut;
use crate::hw::{mutex, pci};

const REG_STATUS: u16 = 0x00;
const REG_CONTROL: u16 = 0x02;
const REG_COMMAND: u16 = 0x03;
const REG_ADDRESS: u16 = 0x04;
const REG_DATA0: u16 = 0x05;
const REG_DATA1: u16 = 0x06;

const PROTOCOL_BYTE_DATA: u8 = 0x08;
const PROTOCOL_WORD_DATA: u8 = 0x0C;
const START: u8 = 0x40;

const BUSY: u8 = 0x01;
const INTR: u8 = 0x02;
const ERROR_MASK: u8 = 0x04 | 0x08 | 0x10;
const CLEAR: u8 = 0x1F;

pub const THERMAL_SENSORS: std::ops::Range<u8> = 0x18..0x20;
pub const PMICS: std::ops::Range<u8> = 0x48..0x50;
pub const SPD_HUBS: std::ops::Range<u8> = 0x50..0x58;
pub const PMIC_ADC_SELECT: u8 = 0x30;
/// Registers that must never be writable: they set the DRAM rails.
pub const PMIC_RAIL_CONTROL: [u8; 4] = [0x21, 0x25, 0x27, 0x2B];

const TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    IntelPch,
    AmdFch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmbusError {
    Refused,
    Timeout,
    Device,
    Busy,
}

pub struct Smbus {
    io: &'static InpOut,
    pub base: u16,
    pub platform: Platform,
}

impl Smbus {
    /// Intel: identify 00:1F.4 (then .3 on older chipsets) as an Intel SMBus function before its
    /// BAR is trusted, so something else at that address can never be driven as a bus.
    pub fn intel(io: &'static InpOut) -> Result<Smbus, String> {
        let mut reasons = Vec::new();
        for function in [4u8, 3] {
            let Some(id) = pci::read32(0, 0x1F, function, 0x00) else {
                reasons.push(format!("00:1F.{function} unreadable"));
                continue;
            };
            if id == 0xFFFF_FFFF || id & 0xFFFF != 0x8086 {
                reasons.push(format!("00:1F.{function} id 0x{id:08X}"));
                continue;
            }
            let class = pci::read32(0, 0x1F, function, 0x08).unwrap_or(0) >> 16;
            if class != 0x0C05 {
                reasons.push(format!("00:1F.{function} class 0x{class:04X}"));
                continue;
            }
            let command = pci::read32(0, 0x1F, function, 0x04).unwrap_or(0);
            if command & 1 == 0 {
                reasons.push(format!("00:1F.{function} I/O decode off"));
                continue;
            }
            let bar = pci::read32(0, 0x1F, function, 0x20).unwrap_or(0);
            if bar & 1 == 0 || bar & 0xFFE0 == 0 {
                reasons.push(format!("00:1F.{function} BAR4 0x{bar:08X}"));
                continue;
            }
            return Ok(Smbus { io, base: (bar & 0xFFE0) as u16, platform: Platform::IntelPch });
        }
        Err(format!("no Intel SMBus controller ({})", reasons.join("; ")))
    }

    /// AMD: the base from PMIO 0x2C/0x2D, falling back to the FCH's well-known 0x0B00 when the read
    /// did not land (zero or all ones).
    pub fn amd(io: &'static InpOut) -> Smbus {
        io.outb(0xCD6, 0x2C);
        let low = io.inb(0xCD7) as u16;
        io.outb(0xCD6, 0x2D);
        let high = io.inb(0xCD7) as u16;
        let base = ((high << 8) | low) & 0xFFE0;
        let base = if base == 0 || base == 0xFFE0 { 0x0B00 } else { base };
        Smbus { io, base, platform: Platform::AmdFch }
    }

    /// The controllers DIMMs may answer on.
    pub fn controllers(&self) -> &'static [u16] {
        match self.platform {
            Platform::IntelPch => &[0x00],
            Platform::AmdFch => &[0x00, 0x20],
        }
    }

    /// Hold the bus for a sequence of transfers. Multi-step reads (select, settle, read) hold it
    /// throughout so another tool cannot change the state they depend on midway.
    pub fn session(&self) -> Option<Session<'_>> {
        let guard = mutex::smbus().lock(mutex::WAIT_MS)?;
        Some(Session { bus: self, _guard: guard })
    }
}

pub struct Session<'a> {
    bus: &'a Smbus,
    _guard: mutex::Guard<'static>,
}

impl Session<'_> {
    fn wait_idle(&self, base: u16) -> Result<(), SmbusError> {
        let deadline = Instant::now() + TIMEOUT;
        while self.bus.io.inb(base + REG_STATUS) & BUSY != 0 {
            if Instant::now() > deadline {
                return Err(SmbusError::Busy);
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        Ok(())
    }

    fn wait_done(&self, base: u16) -> Result<(), SmbusError> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let status = self.bus.io.inb(base + REG_STATUS);
            if status & ERROR_MASK != 0 {
                return Err(SmbusError::Device);
            }
            if status & INTR != 0 && status & BUSY == 0 {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(SmbusError::Timeout);
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    /// One read transfer. The address byte always carries the read bit.
    fn read(&self, controller: u16, address: u8, command: u8, protocol: u8) -> Result<(u8, u8), SmbusError> {
        let io = self.bus.io;
        let base = self.bus.base + controller;
        self.wait_idle(base)?;
        io.outb(base + REG_STATUS, CLEAR);
        io.outb(base + REG_ADDRESS, (address << 1) | 1);
        io.outb(base + REG_COMMAND, command);
        io.outb(base + REG_CONTROL, protocol | START);
        let result = self.wait_done(base).map(|()| (io.inb(base + REG_DATA0), io.inb(base + REG_DATA1)));
        io.outb(base + REG_STATUS, CLEAR);
        result
    }

    /// Read Word Data from a JEDEC thermal sensor, bytes in wire order (a JC-42.4 sensor sends its
    /// most significant byte first, the opposite of the SMBus word convention).
    pub fn read_thermal_word(&self, controller: u16, address: u8, register: u8) -> Result<(u8, u8), SmbusError> {
        if !THERMAL_SENSORS.contains(&address) {
            return Err(SmbusError::Refused);
        }
        self.read(controller, address, register, PROTOCOL_WORD_DATA)
    }

    /// Read Byte Data from a DDR5 PMIC or SPD hub.
    pub fn read_ddr5_byte(&self, controller: u16, address: u8, register: u8) -> Result<u8, SmbusError> {
        if !(PMICS.contains(&address) || SPD_HUBS.contains(&address)) {
            return Err(SmbusError::Refused);
        }
        self.read(controller, address, register, PROTOCOL_BYTE_DATA).map(|(b, _)| b)
    }

    /// The one write: a PMIC's ADC channel selector. Refused for any other (address, register).
    pub fn write_pmic_adc_select(&self, controller: u16, address: u8, value: u8) -> Result<(), SmbusError> {
        if !is_writable(address, PMIC_ADC_SELECT) {
            return Err(SmbusError::Refused);
        }
        let io = self.bus.io;
        let base = self.bus.base + controller;
        self.wait_idle(base)?;
        io.outb(base + REG_STATUS, CLEAR);
        // The only transfer on this transport that clears the direction bit.
        io.outb(base + REG_ADDRESS, address << 1);
        io.outb(base + REG_COMMAND, PMIC_ADC_SELECT);
        io.outb(base + REG_DATA0, value);
        io.outb(base + REG_CONTROL, PROTOCOL_BYTE_DATA | START);
        let result = self.wait_done(base);
        io.outb(base + REG_STATUS, CLEAR);
        result
    }
}

/// The complete set of writable targets: R30h on a PMIC address. An equality, not a range.
pub fn is_writable(address: u8, register: u8) -> bool {
    PMICS.contains(&address) && register == PMIC_ADC_SELECT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_adc_selector_is_writable() {
        for address in 0u8..=0x7F {
            for register in 0u8..=0xFF {
                let allowed = is_writable(address, register);
                assert_eq!(allowed, PMICS.contains(&address) && register == 0x30, "0x{address:02X} R{register:02X}");
            }
        }
        for address in PMICS {
            for r in PMIC_RAIL_CONTROL {
                assert!(!is_writable(address, r));
            }
        }
        for hub in SPD_HUBS {
            assert!(!is_writable(hub, 0x0B));
        }
    }
}
