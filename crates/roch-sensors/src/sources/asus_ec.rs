//! The ACPI embedded controller, for the board sensors nothing else carries (Roch Viewer's
//! `asus_ec`). On the ASUS Z790 bench the VRM temperature lives here and nowhere on the Super I/O.
//!
//! The EC protocol is the ACPI standard; what is in its RAM is the board vendor's, so the map is
//! gated on the vendor. An unknown board's byte at the same address would decode to a plausible
//! temperature rather than an error, which is the failure the gate exists for.
//!
//! Through PawnIO's LpcACPIEC module where it is installed, which admits ports 0x62 and 0x66 and
//! nothing else; through inpoutx64 otherwise.
//!
//! SAFETY: only RD_EC (0x80) is defined here. There is no WR_EC constant, no data-write primitive
//! and no path that could assemble one: on many boards the EC owns fan control and charging. The
//! writes that do happen are the read protocol itself (the command byte and the register
//! address). Every wait is bounded, and the EC mutex is held for each transaction.

use std::time::{Duration, Instant};

use crate::engine::{Batch, Source};
use crate::hw::Driver;
use crate::hw::inpout::driver;
use crate::hw::mutex;
use crate::hw::ports::Ports;
use crate::model::{Group, SensorInfo};
use crate::sources::superio::board_device;
use crate::system::SystemInfo;

const DATA: u16 = 0x62;
const COMMAND: u16 = 0x66;
const OUTPUT_FULL: u8 = 1 << 0;
const INPUT_FULL: u8 = 1 << 1;
const RD_EC: u8 = 0x80;
const TIMEOUT: Duration = Duration::from_millis(20);

/// Confirmed on ASUS ROG MAXIMUS Z790 APEX against HWiNFO's own "ASUS EC" section and through a
/// load cycle: 40 °C idle, 43 loaded, 40 recovered. Four other registers swing 33 -> 65 °C across
/// the same cycle, a die sensor mirrored; a VRM has far more thermal mass and moves a few degrees.
const VRM: u8 = 0x33;

pub struct AsusEc {
    io: Ports,
    sys: SystemInfo,
}

impl AsusEc {
    pub fn detect(sys: &SystemInfo, out: &mut Batch, driver_choice: Driver) -> Option<AsusEc> {
        if !sys.is_asus() {
            out.status(format!(
                "ASUS EC: not read; this board is {}'s, and the EC register map is ASUS's.",
                sys.board_maker()
            ));
            return None;
        }
        let mut reasons = Vec::new();
        #[cfg(windows)]
        if driver_choice.pawnio() {
            match crate::hw::pawnio::PortModule::acpi_ec() {
                Ok(m) => return Some(AsusEc { io: Ports::PawnIo(m), sys: sys.clone() }),
                Err(e) => reasons.push(e),
            }
        }
        if driver_choice.inpout() {
            match driver() {
                Ok(io) => return Some(AsusEc { io: Ports::InpOut(io), sys: sys.clone() }),
                Err(e) => reasons.push(e.to_owned()),
            }
        }
        out.status(format!("ASUS EC unavailable: {}.", reasons.join("; ")));
        None
    }

    fn wait(&self, bit: u8, set: bool) -> Option<()> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if (self.io.inb(COMMAND)? & bit != 0) == set {
                return Some(());
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    fn read(&self, register: u8) -> Option<u8> {
        let _ec = mutex::ec().lock(mutex::WAIT_MS)?;
        // A byte another owner left in the output buffer would come back as ours.
        if self.io.inb(COMMAND)? & OUTPUT_FULL != 0 {
            self.io.inb(DATA)?;
        }
        self.wait(INPUT_FULL, false)?;
        self.io.outb(COMMAND, RD_EC)?;
        self.wait(INPUT_FULL, false)?;
        self.io.outb(DATA, register)?;
        self.wait(OUTPUT_FULL, true)?;
        self.io.inb(DATA)
    }
}

impl Source for AsusEc {
    fn name(&self) -> &str {
        "ASUS EC"
    }

    fn poll(&mut self, out: &mut Batch) {
        out.guest_device(board_device(&self.sys, "ASUS EC"));
        out.status(format!("ASUS EC: VRM temperature at EC register 0x33, through {}.", self.io.driver()));
        let vrm = self.read(VRM).map(|raw| raw as i8 as f64).filter(|t| (-20.0..=125.0).contains(t));
        out.put(
            &SensorInfo::new("board", "temp/vrm-ec", Group::Temperatures, "VRM (EC)", "°C", 1)
                .source("ASUS EC register 0x33"),
            vrm.unwrap_or(f64::NAN),
        );
    }
}
