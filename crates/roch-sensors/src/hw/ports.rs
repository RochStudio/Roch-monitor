//! Byte port I/O through whichever driver the transport got: a PawnIO port module (LpcIO for a
//! Super I/O slot, LpcACPIEC for the ACPI EC) or inpoutx64. The register protocols on top are the
//! same either way; what differs is what the driver admits. PawnIO's modules admit only their own
//! ports and answer anything else with an error, which comes back here as `None` rather than as a
//! byte that could be decoded.

use super::inpout::InpOut;
#[cfg(windows)]
use super::pawnio::PortModule;

pub enum Ports {
    InpOut(&'static InpOut),
    #[cfg(windows)]
    PawnIo(PortModule),
}

impl Ports {
    pub fn inb(&self, port: u16) -> Option<u8> {
        match self {
            Ports::InpOut(io) => Some(io.inb(port)),
            #[cfg(windows)]
            Ports::PawnIo(m) => m.inb(port),
        }
    }

    pub fn outb(&self, port: u16, value: u8) -> Option<()> {
        match self {
            Ports::InpOut(io) => {
                io.outb(port, value);
                Some(())
            }
            #[cfg(windows)]
            Ports::PawnIo(m) => m.outb(port, value),
        }
    }

    /// With a Super I/O in configuration mode and identified, make its BARs' ports reachable.
    /// LpcIO admits them only once asked; inpoutx64 admits every port, so there is nothing to do.
    pub fn admit_bars(&self) -> bool {
        match self {
            Ports::InpOut(_) => true,
            #[cfg(windows)]
            Ports::PawnIo(m) => m.find_bars(),
        }
    }

    /// "PawnIO LpcIO" or "inpoutx64", for status lines.
    pub fn driver(&self) -> String {
        match self {
            Ports::InpOut(_) => "inpoutx64".into(),
            #[cfg(windows)]
            Ports::PawnIo(m) => format!("PawnIO {}", m.module),
        }
    }
}
