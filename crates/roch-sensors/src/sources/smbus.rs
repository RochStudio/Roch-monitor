//! The SMBus host controller the DIMMs hang off: Intel's PCH (Roch Viewer's `intel_pch_smbus`) or
//! AMD's FCH (`fch_smbus`). Both are the PIIX4 register layout; only how the I/O base is found
//! differs. Intel publishes it in the SMBus function's PCI config space (00:1F.4 BAR4), AMD
//! through the PMIO index/data ports, and AMD has a second controller 0x20 above the first.
//!
//! Two ways to the controller. PawnIO's SmbusI801 and SmbusPIIX4 modules, where PawnIO is
//! installed, find the controller themselves and carry out whole transfers; the I801 module also
//! waits out the firmware's hold on the bus (INUSE_STS) first. inpoutx64, where PawnIO is not,
//! drives the controller's registers from here. The rules below sit above both.
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

/// How transfers reach the controller.
enum Backend {
    InpOut {
        io: &'static InpOut,
        base: u16,
    },
    #[cfg(windows)]
    PawnIo(crate::hw::pawnio::SmbusModule),
}

/// A PawnIO transfer's Win32 error as this transport's error.
#[cfg(windows)]
fn pawnio_error(code: u32) -> SmbusError {
    match code {
        // ERROR_SEM_TIMEOUT, WAIT_TIMEOUT, ERROR_TIMEOUT: STATUS_IO_TIMEOUT and kin.
        121 | 258 | 1460 => SmbusError::Timeout,
        // ERROR_BUSY: STATUS_DEVICE_BUSY, the host still busy from another owner.
        170 => SmbusError::Busy,
        _ => SmbusError::Device,
    }
}

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
/// Intel's host status bit 6, INUSE_STS: a semaphore among everything that drives the controller
/// (firmware, drivers, monitoring tools). Any read of the status register sets it; writing a 1
/// releases it. This path reads status in every wait, so it must release the semaphore when a
/// transfer ends, as Linux's i2c-i801 does: Roch Viewer's original never did, which left the bus
/// looking permanently claimed to anything that honours it, PawnIO's SmbusI801 among them (every
/// transfer through it then timed out after 90 ms on the Z790-P bench). AMD's FCH has no such bit.
const INUSE_STS: u8 = 0x40;

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
    backend: Backend,
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
            return Ok(Smbus {
                backend: Backend::InpOut { io, base: (bar & 0xFFE0) as u16 },
                platform: Platform::IntelPch,
            });
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
        Smbus { backend: Backend::InpOut { io, base }, platform: Platform::AmdFch }
    }

    /// Through PawnIO: SmbusI801 on Intel, SmbusPIIX4 on AMD. Each module finds and checks the
    /// controller itself, and declines to load where there is none.
    #[cfg(windows)]
    pub fn pawnio(platform: Platform) -> Result<Smbus, String> {
        use crate::hw::pawnio::SmbusModule;
        let module = match platform {
            Platform::IntelPch => SmbusModule::i801()?,
            Platform::AmdFch => SmbusModule::piix4()?,
        };
        Ok(Smbus { backend: Backend::PawnIo(module), platform })
    }

    /// Which controller, where, and through which driver, for the status line.
    pub fn describe(&self) -> String {
        let what = match self.platform {
            Platform::IntelPch => "Intel PCH SMBus",
            Platform::AmdFch => "AMD FCH SMBus",
        };
        match &self.backend {
            Backend::InpOut { base, .. } => format!("{what} at 0x{base:04X} through inpoutx64"),
            #[cfg(windows)]
            Backend::PawnIo(m) => match m.io_base() {
                Some(base) => format!("{what} at 0x{base:04X} through PawnIO {}", m.module),
                None => format!("{what} through PawnIO {}", m.module),
            },
        }
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

fn wait_idle(io: &InpOut, base: u16) -> Result<(), SmbusError> {
    let deadline = Instant::now() + TIMEOUT;
    while io.inb(base + REG_STATUS) & BUSY != 0 {
        if Instant::now() > deadline {
            return Err(SmbusError::Busy);
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    Ok(())
}

fn wait_done(io: &InpOut, base: u16) -> Result<(), SmbusError> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let status = io.inb(base + REG_STATUS);
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

impl Session<'_> {
    /// What the inpoutx64 path writes to the status register as a transfer ends: the status flags
    /// cleared and, on Intel, the INUSE_STS semaphore released.
    fn release(&self) -> u8 {
        if self.bus.platform == Platform::IntelPch { CLEAR | INUSE_STS } else { CLEAR }
    }

    /// One read transfer. The address byte always carries the read bit: inpoutx64's path sets it
    /// here, PawnIO's path passes the read direction, and neither takes a direction parameter.
    fn read(&self, controller: u16, address: u8, command: u8, protocol: u8) -> Result<(u8, u8), SmbusError> {
        match &self.bus.backend {
            Backend::InpOut { io, base } => {
                let base = base + controller;
                wait_idle(io, base)?;
                io.outb(base + REG_STATUS, CLEAR);
                io.outb(base + REG_ADDRESS, (address << 1) | 1);
                io.outb(base + REG_COMMAND, command);
                io.outb(base + REG_CONTROL, protocol | START);
                let result = wait_done(io, base).map(|()| (io.inb(base + REG_DATA0), io.inb(base + REG_DATA1)));
                io.outb(base + REG_STATUS, self.release());
                result
            }
            #[cfg(windows)]
            Backend::PawnIo(m) => {
                use crate::hw::pawnio::{SMBUS_BYTE_DATA, SMBUS_WORD_DATA};
                let protocol = if protocol == PROTOCOL_WORD_DATA { SMBUS_WORD_DATA } else { SMBUS_BYTE_DATA };
                // A word comes back as DATA0 | DATA1 << 8, the controller's two data registers.
                let v = m.read(controller, address, command, protocol).map_err(pawnio_error)?;
                Ok((v as u8, (v >> 8) as u8))
            }
        }
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
        match &self.bus.backend {
            Backend::InpOut { io, base } => {
                let base = base + controller;
                wait_idle(io, base)?;
                io.outb(base + REG_STATUS, CLEAR);
                // The only transfer on this transport that clears the direction bit.
                io.outb(base + REG_ADDRESS, address << 1);
                io.outb(base + REG_COMMAND, PMIC_ADC_SELECT);
                io.outb(base + REG_DATA0, value);
                io.outb(base + REG_CONTROL, PROTOCOL_BYTE_DATA | START);
                let result = wait_done(io, base);
                io.outb(base + REG_STATUS, self.release());
                result
            }
            // The only call on this transport that passes the write direction.
            #[cfg(windows)]
            Backend::PawnIo(m) => m.write_byte(controller, address, PMIC_ADC_SELECT, value).map_err(pawnio_error),
        }
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

    /// `cargo test -p roch-sensors pawnio_thermal -- --ignored --nocapture`, elevated, with PawnIO.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs PawnIO, an elevated process and an Intel SMBus"]
    fn pawnio_thermal_sensor_reads() {
        let m = crate::hw::pawnio::SmbusModule::i801().expect("SmbusI801");
        let _bus = mutex::smbus().lock(mutex::WAIT_MS).expect("the SMBus mutex");
        for address in THERMAL_SENSORS {
            let t = Instant::now();
            let r = m.read(0, address, 0x05, crate::hw::pawnio::SMBUS_WORD_DATA);
            println!("0x{address:02X}: {r:?} in {:?}", t.elapsed());
        }
    }
}
