//! The Intel memory controller's register window, MCHBAR: clocks, the hottest core, package power
//! and two rails (Roch Viewer's `intel_timings` sensor rows and `intel_rapl`).
//!
//! The driver has no RDMSR, so the readings HWiNFO takes from MSRs are taken here from where the
//! same silicon also publishes them in MCHBAR, each located by Roch Viewer against a reference
//! tool on the Z790 benches and recorded beside its offset.
//!
//! Read-only: physical reads, nothing else.

use std::time::{Duration, Instant};

use crate::engine::{Batch, Source};
use crate::hw::inpout::{InpOut, bits, driver};
use crate::hw::pci;
use crate::model::{DeviceInfo, DeviceKind, Group, SensorInfo};
use crate::system::SystemInfo;

/// Where Alder Lake, Raptor Lake and Arrow Lake firmware puts MCHBAR when nothing says otherwise.
pub const MCHBAR_DEFAULT: u64 = 0xFEDC_0000;

// Clocks.
const BCLK_KHZ: u64 = 0x5F60;
const RING_RATIO: (u64, u32, u32) = (0x5918, 24, 8);
const RING_RATIO_ARROW_LAKE: (u64, u32, u32) = (0x15918, 24, 8);
const DRAM_RATIO: u64 = 0x5E04;
const DRAM_RATIO_ARROW_LAKE: u64 = 0x13D10;

// The hottest core, from the die. Found by sweeping all of MCHBAR across an idle -> 16-process
// load -> idle cycle twice: 0x5978 and its mirror 0x597C jitter 33 -> 44 -> 34 at idle the way DTS
// does and follow load the instant it starts and stops. One reading available twice (198 of 200
// back-to-back reads agreed), so one row. TjMax is 100 °C on these parts; zero is unread.
const CORE_MAX: u64 = 0x5978;
const CORE_MAX_BAND: (f64, f64) = (5.0, 125.0);

// RAPL, published in MCHBAR as well as the MSRs. Confirmed against HWiNFO through a load cycle:
// package 43.8 -> 193.8 -> 43.3 W against HWiNFO's 42.7 idle; cores 34.2 -> 183.5 -> 33.0 W.
const RAPL_UNIT: u64 = 0x5938;
const RAPL_PACKAGE: u64 = 0x593C;
const RAPL_CORES: u64 = 0x5928;
const POWER_BAND: (f64, f64) = (0.0, 1000.0);
/// Past this the previous sample is too old to subtract from: the counter is 32 bits and wraps in
/// about twenty minutes at a few hundred watts.
const MAX_SAMPLE_AGE: Duration = Duration::from_secs(30);

// Rails the memory controller reports itself.
const VCCSA: (u64, u32, u32) = (0x591C, 8, 23);
const VDDQ_TX: (u64, u32, u32) = (0x5E00, 17, 10);

const CPU: &str = "cpu";
const MEMORY: &str = "memory";

pub fn cpu_device(sys: &SystemInfo) -> DeviceInfo {
    let cores = sys.topology.cores;
    let threads = sys.topology.logical.len();
    let detail = if sys.topology.hybrid {
        let p = sys.topology.logical.iter().filter(|l| l.performance && l.thread == 0).count();
        let e = cores.saturating_sub(p);
        format!("{p}P + {e}E cores, {threads} threads")
    } else if cores > 0 {
        format!("{cores} cores, {threads} threads")
    } else {
        String::new()
    };
    DeviceInfo::new(CPU, DeviceKind::Cpu, sys.cpu_name()).detail(detail)
}

pub fn memory_device(sys: &SystemInfo) -> DeviceInfo {
    let mut parts: Vec<String> = Vec::new();
    for m in &sys.smbios.memory {
        let p = m.part_number.trim();
        if !p.is_empty() && !parts.iter().any(|x| x.starts_with(p)) {
            let n = sys.smbios.memory.iter().filter(|o| o.part_number.trim() == p).count();
            parts.push(if n > 1 { format!("{p} ×{n}") } else { p.to_owned() });
        }
    }
    let name = match sys.memory_summary() {
        s if s.is_empty() => "Memory".to_owned(),
        s => s,
    };
    DeviceInfo::new(MEMORY, DeviceKind::Memory, name).detail(parts.join(", "))
}

pub struct IntelMchbar {
    io: &'static InpOut,
    base: u64,
    arrow_lake: bool,
    cpu: DeviceInfo,
    memory: DeviceInfo,
    rapl: [Option<(u32, Instant)>; 2],
    status: String,
}

impl IntelMchbar {
    pub fn detect(sys: &SystemInfo, out: &mut Batch) -> Option<IntelMchbar> {
        if !(sys.cpu.is_lga1700() || sys.cpu.is_arrow_lake()) {
            out.status(format!(
                "MCHBAR: not read on {} (family 0x{:X} model 0x{:X}); its offsets were located on LGA 1700 and LGA 1851 and are not pointed at silicon they were not written for.",
                sys.cpu_name(),
                sys.cpu.family,
                sys.cpu.model
            ));
            return None;
        }
        let io = match driver() {
            Ok(io) => io,
            Err(e) => {
                out.status(format!("MCHBAR unavailable: {e}"));
                return None;
            }
        };
        // The host bridge says where MCHBAR is: 0x48 holds the base with bit 0 as its enable.
        let host = pci::read32(0, 0, 0, 0x00);
        let mut base = MCHBAR_DEFAULT;
        let mut how = "default".to_owned();
        if host.is_some_and(|id| id & 0xFFFF == 0x8086)
            && let (Some(lo), Some(hi)) = (pci::read32(0, 0, 0, 0x48), pci::read32(0, 0, 0, 0x4C))
            && lo & 1 == 1
        {
            let b = (((hi as u64) << 32) | lo as u64) & 0x7F_FFFE_0000;
            if b != 0 {
                base = b;
                how = "host bridge 0x48".into();
            }
        }
        let bclk = io.phys_read32(base + BCLK_KHZ).unwrap_or(0) as f64 / 1000.0;
        if !(50.0..=300.0).contains(&bclk) {
            out.status(format!(
                "MCHBAR at 0x{base:X} did not answer like an LGA 1700/1851 memory controller (BCLK register read {bclk:.3} MHz); nothing read from it."
            ));
            return None;
        }
        let arrow_lake = sys.cpu.is_arrow_lake();
        let status = format!(
            "MCHBAR at 0x{base:X} ({how}){}: clocks, Core Max, RAPL power, VCCSA/VDDQ TX.",
            if arrow_lake { ", Arrow Lake layout" } else { "" }
        );
        Some(IntelMchbar {
            io,
            base,
            arrow_lake,
            cpu: cpu_device(sys),
            memory: memory_device(sys),
            rapl: [None, None],
            status,
        })
    }

    fn read(&self, offset: u64) -> Option<u32> {
        // An all-ones dword is not a value; it is unmapped MMIO answering.
        self.io.phys_read32(self.base + offset).filter(|&v| v != 0xFFFF_FFFF)
    }

    fn field(&self, (offset, start, width): (u64, u32, u32)) -> Option<u64> {
        self.read(offset).map(|v| bits(v as u64, start, width))
    }

    fn core_max(&self) -> f64 {
        if self.arrow_lake {
            // Located on the Raptor Lake bench only.
            return f64::NAN;
        }
        match self.read(CORE_MAX) {
            Some(raw) if (CORE_MAX_BAND.0..=CORE_MAX_BAND.1).contains(&(raw as f64)) => raw as f64,
            _ => f64::NAN,
        }
    }

    /// Watts for one RAPL domain: the energy counter's growth over the time since the last read.
    /// NaN on the first read of a session, which can only be a baseline.
    fn rapl(&mut self, slot: usize, offset: u64, unit_raw: u32) -> f64 {
        let Some(counter) = self.read(offset) else { return f64::NAN };
        let now = Instant::now();
        let previous = self.rapl[slot].replace((counter, now));
        let Some((last, at)) = previous else { return f64::NAN };
        let elapsed = now.duration_since(at);
        if elapsed.is_zero() || elapsed > MAX_SAMPLE_AGE {
            return f64::NAN;
        }
        let watts = rapl_watts(last, counter, elapsed.as_secs_f64(), unit_raw);
        if (POWER_BAND.0..=POWER_BAND.1).contains(&watts) { watts } else { f64::NAN }
    }

    /// (data rate in MT/s, gear) from the controller's own ratio register.
    fn dram_speed(&self, bclk: f64) -> Option<(f64, u32)> {
        if self.arrow_lake {
            // 0x13D10: the ratio in the low byte, the gear in bit 8 (Gear 2 or 4). The ratio counts
            // a 33.334 MHz reference, so the gear multiplies: 132 x 33.334 x 2 and 66 x 33.334 x 4
            // are both the 8800 SMBIOS reports across a Gear 2 to Gear 4 change.
            let raw = self.read(DRAM_RATIO_ARROW_LAKE)? as u64;
            let ratio = bits(raw, 0, 8) as f64;
            let gear = if bits(raw, 8, 1) == 1 { 4 } else { 2 };
            return (ratio > 0.0).then(|| ((ratio * 33.334 * gear as f64).round(), gear));
        }
        let raw = self.read(DRAM_RATIO)? as u64;
        let ratio = bits(raw, 0, 8) as f64;
        let reference = if bits(raw, 8, 4) == 0 { 133.333_333 } else { 100.0 };
        let gear = match bits(raw, 12, 2) {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => return None,
        };
        (ratio > 0.0).then(|| ((ratio * reference / 100.0 * bclk * gear as f64).round(), gear))
    }
}

/// Watts between two samples of a free-running 32-bit energy counter. The subtraction is masked, so
/// one wrap between samples is handled by the arithmetic.
pub fn rapl_watts(previous: u32, current: u32, seconds: f64, unit_raw: u32) -> f64 {
    if seconds <= 0.0 {
        return f64::NAN;
    }
    let energy_unit = 1.0 / (1u64 << ((unit_raw >> 8) & 0x1F)) as f64;
    current.wrapping_sub(previous) as f64 * energy_unit / seconds
}

impl Source for IntelMchbar {
    fn name(&self) -> &str {
        "MCHBAR"
    }

    fn poll(&mut self, out: &mut Batch) {
        out.device(self.cpu.clone());
        out.device(self.memory.clone());
        out.status(self.status.clone());

        let bclk = self.read(BCLK_KHZ).map(|v| v as f64 / 1000.0).filter(|b| (50.0..=300.0).contains(b));
        let b = bclk.unwrap_or(f64::NAN);
        out.put(&SensorInfo::new(CPU, "clk/bclk", Group::Clocks, "Bus Clock", "MHz", 2).source("MCHBAR 0x5F60"), b);

        let ring_field = if self.arrow_lake { RING_RATIO_ARROW_LAKE } else { RING_RATIO };
        let ring = self.field(ring_field).filter(|&r| r > 0).map_or(f64::NAN, |r| r as f64 * b);
        out.put(
            &SensorInfo::new(CPU, "clk/ring", Group::Clocks, "Ring/LLC Clock", "MHz", 0)
                .source(format!("MCHBAR 0x{:X}[31:24] × BCLK", ring_field.0)),
            ring,
        );

        if !self.arrow_lake {
            out.put(
                &SensorInfo::new(CPU, "temp/coremax", Group::Temperatures, "Core Max", "°C", 1)
                    .source("MCHBAR 0x5978 — the die's hottest core, sampled every 200 ms for its peaks"),
                self.core_max(),
            );
        }

        if let Some(unit) = self.read(RAPL_UNIT).filter(|&u| u != 0) {
            let pkg = self.rapl(0, RAPL_PACKAGE, unit);
            let cores = self.rapl(1, RAPL_CORES, unit);
            out.put(
                &SensorInfo::new(CPU, "power/package", Group::Power, "CPU Package Power", "W", 1)
                    .source("RAPL energy counter, MCHBAR 0x593C"),
                pkg,
            );
            out.put(
                &SensorInfo::new(CPU, "power/cores", Group::Power, "IA Cores Power", "W", 1)
                    .source("RAPL energy counter, MCHBAR 0x5928"),
                cores,
            );
        }

        if !self.arrow_lake {
            // VCCSA reads 0 on Arrow Lake, and VDDQ TX is not a rail on that socket.
            let sa = self.field(VCCSA).map_or(f64::NAN, |v| v as f64 / 8192.0);
            out.put(
                &SensorInfo::new(CPU, "volt/vccsa", Group::Voltages, "VCCSA", "V", 3)
                    .source("MCHBAR 0x591C[30:8] / 8192"),
                if (0.5..=1.8).contains(&sa) { sa } else { f64::NAN },
            );
            let tx = self.field(VDDQ_TX).map_or(f64::NAN, |v| v as f64 / 200.0);
            out.put(
                &SensorInfo::new(CPU, "volt/vddqtx", Group::Voltages, "VDDQ TX", "V", 3)
                    .source("MCHBAR 0x5E00[26:17] / 200"),
                if (0.5..=2.0).contains(&tx) { tx } else { f64::NAN },
            );
        }

        if let Some((rate, gear)) = bclk.and_then(|bclk| self.dram_speed(bclk)) {
            let src = if self.arrow_lake { "MCHBAR 0x13D10" } else { "MCHBAR 0x5E04" };
            out.put(&SensorInfo::new(MEMORY, "clk/rate", Group::Clocks, "Data Rate", "MT/s", 0).source(src), rate);
            let mclk = rate / 2.0;
            out.put(
                &SensorInfo::new(MEMORY, "clk/mclk", Group::Clocks, "Memory Clock (MCLK)", "MHz", 0).source(src),
                mclk,
            );
            // The controller runs at MCLK divided by the gear: Gear 2 at 4400 MCLK is 2200 UCLK.
            out.put(
                &SensorInfo::new(MEMORY, "clk/uclk", Group::Clocks, "Controller Clock (UCLK)", "MHz", 0)
                    .source(format!("{src}, MCLK / Gear {gear}")),
                mclk / gear as f64,
            );
        }
    }

    fn fast_interval(&self) -> Option<Duration> {
        (!self.arrow_lake).then_some(Duration::from_millis(200))
    }

    fn fast_poll(&mut self, out: &mut Batch) {
        out.value(format!("{CPU}/temp/coremax"), self.core_max());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapl_counts_energy_in_the_units_the_register_states() {
        // 0x000A0E03 on the Z790 bench: energy unit 1/2^14 J.
        let w = rapl_watts(0, 16384 * 50, 1.0, 0x000A_0E03);
        assert!((w - 50.0).abs() < 1e-9);
        // One wrap between samples is handled by the masked subtraction.
        let w = rapl_watts(0xFFFF_FFFF - 16383, 0, 1.0, 0x000A_0E03);
        assert!((w - 1.0).abs() < 1e-9);
        assert!(rapl_watts(0, 1, 0.0, 0x000A_0E03).is_nan());
    }
}
