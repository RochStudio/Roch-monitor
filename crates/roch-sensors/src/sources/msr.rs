//! Per-core readings from the CPU's model-specific registers, the way HWiNFO shows them: each
//! core's temperature, clock and VID, each thread's effective clock, the package temperature and
//! the digital thermal sensor's limit flags (Intel); each thread's effective clock and each core's
//! power (AMD Zen).
//!
//! The registers are read through PawnIO's signed MSR modules (see `hw::pawnio`), each on its own
//! logical processor: the thread is pinned there first, since an MSR read is answered by the core
//! that executes it.
//!
//!   IA32_THERM_STATUS 0x19C          bits 22:16 degrees below TjMax, bit 31 reading valid;
//!                                    bit 0 throttling, bit 4 critical, bit 10 power limited
//!   IA32_PACKAGE_THERM_STATUS 0x1B1  the same for the package, without the valid bit
//!   MSR_TEMPERATURE_TARGET 0x1A2     bits 23:16 TjMax
//!   IA32_PERF_STATUS 0x198           bits 15:8 the core's ratio, bits 47:32 its VID in 1/8192 V
//!   IA32_APERF 0xE8                  cycles at the actual clock while active: its growth over
//!                                    wall time is the effective clock, idle counted as zero
//!
//! A core's clock is its ratio times the bus clock. The bus clock is MCHBAR's measured one when
//! that source is running, else the nominal one CPUID leaf 0x16 reports. The TSC is the last resort
//! only: since Skylake it runs off the crystal, not BCLK (38.4 MHz × 166/2 = 3187.2 MHz on a
//! 14900KF), so TSC / base ratio reads 99.6 MHz and would put every core 0.4 % low.
//!
//! On AMD the read-only aliases APERF_RO 0xC00000E8 and CORE_ENERGY_STAT 0xC001029A, with the unit
//! in PWR_UNIT 0xC0010299, serve the same purposes. Zen has no per-core temperature register; its
//! temperatures come from the SMU (see `amd_smu`).

use std::time::Instant;

use crate::engine::{Batch, Source};
use crate::hw::cpuid::Vendor;
use crate::model::{DeviceInfo, Group, SensorInfo};
use crate::sources::intel_mchbar::{cpu_device, measured_bclk};
use crate::system::SystemInfo;

const IA32_THERM_STATUS: u32 = 0x19C;
const IA32_PACKAGE_THERM_STATUS: u32 = 0x1B1;
const MSR_TEMPERATURE_TARGET: u32 = 0x1A2;
const IA32_PERF_STATUS: u32 = 0x198;
const MSR_PLATFORM_INFO: u32 = 0xCE;
const IA32_APERF: u32 = 0xE8;
const AMD_APERF_RO: u32 = 0xC000_00E8;
const AMD_PWR_UNIT: u32 = 0xC001_0299;
const AMD_CORE_ENERGY: u32 = 0xC001_029A;

/// The status bits HWiNFO lists under the DTS: (bit, key, label). Each core's register, for the
/// core flags, set on any core; the package register for the package flags.
const CORE_FLAGS: [(u32, &str, &str); 3] = [
    (0, "limit/core_thermal", "Core Thermal Throttling"),
    (4, "limit/core_critical", "Core Critical Temperature"),
    (10, "limit/core_power", "Core Power Limit Exceeded"),
];
const PACKAGE_FLAGS: [(u32, &str, &str); 3] = [
    (0, "limit/package_thermal", "Package/Ring Thermal Throttling"),
    (4, "limit/package_critical", "Package/Ring Critical Temperature"),
    (10, "limit/package_power", "Package/Ring Power Limit Exceeded"),
];

const CPU: &str = "cpu";
const BUS_BAND: std::ops::RangeInclusive<f64> = 50.0..=300.0;

/// A core's temperature from IA32_THERM_STATUS: TjMax less the readout, when the reading is valid.
pub fn decode_therm(raw: u64, tjmax: f64) -> Option<f64> {
    let valid = raw >> 31 & 1 == 1;
    (valid && tjmax > 0.0).then_some(tjmax - (raw >> 16 & 0x7F) as f64)
}

/// The package's from IA32_PACKAGE_THERM_STATUS, which has no valid bit.
pub fn decode_package(raw: u64, tjmax: f64) -> Option<f64> {
    (tjmax > 0.0).then_some(tjmax - (raw >> 16 & 0x7F) as f64)
}

pub fn decode_tjmax(raw: u64) -> Option<f64> {
    let t = (raw >> 16 & 0xFF) as f64;
    (60.0..=130.0).contains(&t).then_some(t)
}

/// (ratio, VID volts) from IA32_PERF_STATUS. A VID of zero is a core that reports none.
pub fn decode_perf_status(raw: u64) -> (f64, Option<f64>) {
    let ratio = (raw >> 8 & 0xFF) as f64;
    let vid = (raw >> 32 & 0xFFFF) as f64 / 8192.0;
    (ratio, (0.2..=2.0).contains(&vid).then_some(vid))
}

/// MHz from the growth of a cycle counter over `seconds`.
pub fn counter_mhz(previous: u64, current: u64, seconds: f64) -> Option<f64> {
    if seconds <= 0.0 || current < previous {
        return None;
    }
    let mhz = (current - previous) as f64 / seconds / 1e6;
    (0.0..=10_000.0).contains(&mhz).then_some(mhz)
}

/// Watts from a 32-bit energy counter in units of 1/2^esu J; one wrap between samples is handled.
pub fn energy_watts(previous: u32, current: u32, seconds: f64, esu: u32) -> Option<f64> {
    if seconds <= 0.0 {
        return None;
    }
    let joules = current.wrapping_sub(previous) as f64 / (1u64 << esu.min(31)) as f64;
    let w = joules / seconds;
    (0.0..=500.0).contains(&w).then_some(w)
}

/// The bus clock: measured, else nominal, else TSC / base ratio.
pub fn pick_bus_clock(measured: Option<f64>, nominal_mhz: u32, tsc_based: f64) -> f64 {
    let nominal = nominal_mhz as f64;
    match measured {
        Some(b) if BUS_BAND.contains(&b) => b,
        _ if BUS_BAND.contains(&nominal) => nominal,
        _ if BUS_BAND.contains(&tsc_based) => tsc_based,
        _ => f64::NAN,
    }
}

fn mean(v: &[f64]) -> f64 {
    let (sum, n) = v.iter().filter(|x| x.is_finite()).fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    if n == 0 { f64::NAN } else { sum / n as f64 }
}

fn max(v: &[f64]) -> f64 {
    v.iter().copied().filter(|x| x.is_finite()).fold(f64::NAN, f64::max)
}

fn sum(v: &[f64]) -> f64 {
    let finite: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if finite.is_empty() { f64::NAN } else { finite.iter().sum() }
}

fn tsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    #[allow(unused_unsafe)]
    // SAFETY: RDTSC is available in user mode on every x86-64 processor.
    let t = unsafe { std::arch::x86_64::_rdtsc() };
    #[cfg(not(target_arch = "x86_64"))]
    let t = 0;
    t
}

/// Whether this CPU has a PawnIO MSR module here.
pub fn supported(sys: &SystemInfo) -> bool {
    match sys.vendor() {
        Vendor::Intel => sys.cpu.family >= 6,
        Vendor::Amd => matches!(sys.cpu.family, 0x17 | 0x19 | 0x1A),
        Vendor::Other => false,
    }
}

/// Whether the per-core MSR readings will work: PawnIO installed, this process allowed in, and the
/// module answering. Checked once at start, so the performance-counter clocks can stand aside
/// when these are coming.
pub fn available(sys: &SystemInfo) -> bool {
    #[cfg(windows)]
    {
        use crate::hw::pawnio::{AMD_FAMILY17_MODULE, INTEL_MSR_MODULE, PawnIo};
        if !supported(sys) {
            return false;
        }
        let (module, probe) = match sys.vendor() {
            Vendor::Intel => (INTEL_MSR_MODULE, MSR_TEMPERATURE_TARGET),
            _ => (AMD_FAMILY17_MODULE, AMD_PWR_UNIT),
        };
        PawnIo::open(module).is_ok_and(|io| io.read_msr(probe).is_some())
    }
    #[cfg(not(windows))]
    {
        let _ = sys;
        false
    }
}

struct Thread {
    group: u16,
    number: u8,
    core: usize,
    first_on_core: bool,
    label: String,
}

pub struct CpuMsr {
    #[cfg(windows)]
    io: crate::hw::pawnio::PawnIo,
    vendor: Vendor,
    tjmax: f64,
    base_ratio: f64,
    nominal_bus: u32,
    /// Energy-status unit (AMD): the counters count 1/2^esu J.
    esu: u32,
    /// One label per core, from its first thread's.
    cores: Vec<String>,
    /// In HWiNFO's order: by core, each core's threads together.
    threads: Vec<Thread>,
    last_aperf: Vec<Option<(u64, Instant)>>,
    last_energy: Vec<Option<(u32, Instant)>>,
    last_tsc: (u64, Instant),
    tsc_bus: f64,
    device: DeviceInfo,
    status: String,
}

impl CpuMsr {
    #[cfg(windows)]
    pub fn detect(sys: &SystemInfo, out: &mut Batch) -> Option<CpuMsr> {
        use crate::hw::pawnio::{AMD_FAMILY17_MODULE, INTEL_MSR_MODULE, PawnIo, pin_to};
        if !supported(sys) {
            out.status(format!("Per-core MSRs: PawnIO has no MSR module for {}.", sys.cpu_name()));
            return None;
        }
        let vendor = sys.vendor();
        let module = if vendor == Vendor::Intel { INTEL_MSR_MODULE } else { AMD_FAMILY17_MODULE };
        let io = match PawnIo::open(module) {
            Ok(io) => io,
            Err(e) => {
                out.status(e);
                return None;
            }
        };
        let topo = &sys.topology;
        if topo.logical.is_empty() {
            out.status("Per-core MSRs: Windows reported no processor topology.");
            return None;
        }
        let mut threads: Vec<Thread> = topo
            .logical
            .iter()
            .map(|l| Thread {
                group: l.group,
                number: l.number,
                core: l.core,
                first_on_core: l.thread == 0,
                label: topo.label(l.group, l.number).unwrap_or_else(|| format!("CPU {}", l.number)),
            })
            .collect();
        threads.sort_by_key(|t| (t.core, !t.first_on_core, t.group, t.number));
        let core_count = threads.iter().map(|t| t.core).max().map_or(0, |m| m + 1);
        let cores: Vec<String> = (0..core_count)
            .map(|c| {
                threads
                    .iter()
                    .find(|t| t.core == c && t.first_on_core)
                    .map(|t| t.label.trim_end_matches(" T0").to_owned())
                    .unwrap_or_else(|| format!("Core {c}"))
            })
            .collect();

        let (tjmax, base_ratio, esu) = {
            let _pin = pin_to(threads[0].group, threads[0].number);
            match vendor {
                Vendor::Intel => (
                    io.read_msr(MSR_TEMPERATURE_TARGET).and_then(decode_tjmax).unwrap_or(f64::NAN),
                    io.read_msr(MSR_PLATFORM_INFO).map_or(0.0, |v| (v >> 8 & 0xFF) as f64),
                    0,
                ),
                _ => (f64::NAN, 0.0, io.read_msr(AMD_PWR_UNIT).map_or(16, |v| (v >> 8 & 0x1F) as u32)),
            }
        };
        let status = match vendor {
            Vendor::Intel => format!(
                "PawnIO IntelMSR: {} cores, {} threads, TjMax {}.",
                cores.len(),
                threads.len(),
                if tjmax.is_finite() { format!("{tjmax:.0} °C") } else { "unread".into() },
            ),
            _ => format!(
                "PawnIO AMDFamily17: {} cores, {} threads; effective clocks and core power (not yet checked on AMD hardware).",
                cores.len(),
                threads.len()
            ),
        };
        let n = threads.len();
        Some(CpuMsr {
            io,
            vendor,
            tjmax,
            base_ratio,
            nominal_bus: sys.cpu.bus_mhz,
            esu,
            last_aperf: vec![None; n],
            last_energy: vec![None; core_count],
            cores,
            threads,
            last_tsc: (tsc(), Instant::now()),
            tsc_bus: f64::NAN,
            device: cpu_device(sys),
            status,
        })
    }

    #[cfg(not(windows))]
    pub fn detect(_sys: &SystemInfo, out: &mut Batch) -> Option<CpuMsr> {
        out.status("Per-core MSRs are read through PawnIO, which is Windows-only.");
        None
    }

    /// The bus clock the core ratios multiply.
    fn bus_clock(&mut self) -> f64 {
        let (t, now) = (tsc(), Instant::now());
        let seconds = now.duration_since(self.last_tsc.1).as_secs_f64();
        // A window under a tenth of a second is too short to time the TSC by.
        if seconds >= 0.1 {
            if let Some(mhz) = counter_mhz(self.last_tsc.0, t, seconds)
                && self.base_ratio > 0.0
            {
                self.tsc_bus = mhz / self.base_ratio;
            }
            self.last_tsc = (t, now);
        }
        pick_bus_clock(measured_bclk(), self.nominal_bus, self.tsc_bus)
    }

    fn label(&self, what: &str) -> String {
        format!("PawnIO {}, {what}", if self.vendor == Vendor::Intel { "IntelMSR" } else { "AMDFamily17" })
    }

    /// One parent row and a child per core (or per thread) under it.
    fn put_family(
        &self,
        out: &mut Batch,
        parent: SensorInfo,
        value: f64,
        children: impl Iterator<Item = (String, String, f64)>,
        child_source: &str,
    ) {
        out.put(&parent, value);
        for (key, label, v) in children {
            let info = SensorInfo::new(CPU, key, parent.group, label, &parent.unit, parent.decimals)
                .parent(parent.key.clone())
                .source(self.label(child_source));
            out.put(&info, v);
        }
    }
}

impl Source for CpuMsr {
    fn name(&self) -> &str {
        "Per-core MSRs"
    }

    #[cfg(windows)]
    fn poll(&mut self, out: &mut Batch) {
        use crate::hw::pawnio::{pin_to, raise_priority};
        out.device(self.device.clone());
        out.status(self.status.clone());
        let intel = self.vendor == Vendor::Intel;
        let bus = if intel { self.bus_clock() } else { f64::NAN };
        let mut temps = vec![f64::NAN; self.cores.len()];
        let mut clocks = vec![f64::NAN; self.cores.len()];
        let mut vids = vec![f64::NAN; self.cores.len()];
        let mut powers = vec![f64::NAN; self.cores.len()];
        let mut effective = vec![f64::NAN; self.threads.len()];
        // Bits set on any core, and whether any core answered at all.
        let (mut core_flags, mut core_read) = (0u64, false);
        let mut package: Option<u64> = None;
        // Only for the sweep: each pinned read then runs as soon as its core can take it.
        let raised = raise_priority();
        for (i, t) in self.threads.iter().enumerate() {
            let Some(_pin) = pin_to(t.group, t.number) else { continue };
            let now = Instant::now();
            if let Some(a) = self.io.read_msr(if intel { IA32_APERF } else { AMD_APERF_RO }) {
                if let Some((last, at)) = self.last_aperf[i] {
                    effective[i] = counter_mhz(last, a, now.duration_since(at).as_secs_f64()).unwrap_or(f64::NAN);
                }
                self.last_aperf[i] = Some((a, now));
            }
            if !t.first_on_core {
                continue;
            }
            let c = t.core;
            if intel {
                if let Some(raw) = self.io.read_msr(IA32_THERM_STATUS) {
                    temps[c] = decode_therm(raw, self.tjmax).unwrap_or(f64::NAN);
                    core_flags |= raw;
                    core_read = true;
                }
                if let Some(raw) = self.io.read_msr(IA32_PERF_STATUS) {
                    let (ratio, vid) = decode_perf_status(raw);
                    if ratio > 0.0 {
                        clocks[c] = ratio * bus;
                    }
                    vids[c] = vid.unwrap_or(f64::NAN);
                }
                if package.is_none() {
                    package = self.io.read_msr(IA32_PACKAGE_THERM_STATUS);
                }
            } else if let Some(raw) = self.io.read_msr(AMD_CORE_ENERGY) {
                let e = raw as u32;
                if let Some((last, at)) = self.last_energy[c] {
                    powers[c] =
                        energy_watts(last, e, now.duration_since(at).as_secs_f64(), self.esu).unwrap_or(f64::NAN);
                }
                self.last_energy[c] = Some((e, now));
            }
        }
        drop(raised);

        let per_core = |values: &[f64], key: &str, suffix: &str| -> Vec<(String, String, f64)> {
            self.cores
                .iter()
                .enumerate()
                .map(|(c, name)| (format!("{key}{c}"), format!("{name}{suffix}"), values[c]))
                .collect()
        };
        if intel {
            out.put(
                &SensorInfo::new(CPU, "temp/package", Group::Temperatures, "CPU Package", "°C", 1)
                    .source(self.label("IA32_PACKAGE_THERM_STATUS 0x1B1, TjMax less bits 22:16")),
                package.and_then(|raw| decode_package(raw, self.tjmax)).unwrap_or(f64::NAN),
            );
            self.put_family(
                out,
                SensorInfo::new(CPU, "temp/cores", Group::Temperatures, "Core Temperatures (avg)", "°C", 1)
                    .source(self.label("IA32_THERM_STATUS 0x19C on each core")),
                mean(&temps),
                per_core(&temps, "temp/core", "").into_iter(),
                "IA32_THERM_STATUS 0x19C, TjMax less bits 22:16",
            );
            self.put_family(
                out,
                SensorInfo::new(CPU, "volt/vid", Group::Voltages, "Core VID (max)", "V", 3)
                    .source(self.label("IA32_PERF_STATUS 0x198 bits 47:32 on each core")),
                max(&vids),
                per_core(&vids, "volt/vid", " VID").into_iter(),
                "IA32_PERF_STATUS 0x198 bits 47:32 / 8192",
            );
            self.put_family(
                out,
                SensorInfo::new(CPU, "clk/cores", Group::Clocks, "Core Clocks (avg)", "MHz", 0)
                    .source(self.label("IA32_PERF_STATUS 0x198 ratio × bus clock, on each core")),
                mean(&clocks),
                per_core(&clocks, "clk/core", " Clock").into_iter(),
                "IA32_PERF_STATUS 0x198 bits 15:8 × the bus clock (MCHBAR's, else CPUID's nominal)",
            );
            for (bit, key, label) in CORE_FLAGS {
                out.put(
                    &SensorInfo::new(CPU, key, Group::Limiters, label, "state", 0)
                        .source(self.label(&format!("IA32_THERM_STATUS 0x19C bit {bit}, on any core"))),
                    if core_read { (core_flags >> bit & 1) as f64 } else { f64::NAN },
                );
            }
            for (bit, key, label) in PACKAGE_FLAGS {
                out.put(
                    &SensorInfo::new(CPU, key, Group::Limiters, label, "state", 0)
                        .source(self.label(&format!("IA32_PACKAGE_THERM_STATUS 0x1B1 bit {bit}"))),
                    package.map_or(f64::NAN, |raw| (raw >> bit & 1) as f64),
                );
            }
        } else {
            self.put_family(
                out,
                SensorInfo::new(CPU, "power/msr-cores", Group::Power, "Core Powers (sum)", "W", 2)
                    .source(self.label("CORE_ENERGY_STAT 0xC001029A on each core")),
                sum(&powers),
                per_core(&powers, "power/msr-core", " Power").into_iter(),
                "CORE_ENERGY_STAT 0xC001029A, units from PWR_UNIT 0xC0010299",
            );
        }
        let threads = self
            .threads
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("clk/aperf{i}"), format!("{} Effective Clock", t.label), effective[i]));
        self.put_family(
            out,
            SensorInfo::new(CPU, "clk/aperf", Group::Clocks, "Effective Clocks (avg)", "MHz", 0)
                .source(self.label("APERF growth over wall time on each thread, idle counted as zero")),
            mean(&effective),
            threads,
            "APERF growth over wall time, idle counted as zero",
        );
    }

    #[cfg(not(windows))]
    fn poll(&mut self, _out: &mut Batch) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_core_reading_is_tjmax_less_the_readout() {
        // Valid, 55 below a TjMax of 100, throttling.
        let raw = (1u64 << 31) | (55u64 << 16) | 1;
        assert_eq!(decode_therm(raw, 100.0), Some(45.0));
        // Not valid: no reading, not a guess.
        assert_eq!(decode_therm(55u64 << 16, 100.0), None);
        assert_eq!(decode_therm(raw, f64::NAN), None, "no TjMax, no temperature");
        assert_eq!(decode_tjmax(100u64 << 16), Some(100.0));
        assert_eq!(decode_tjmax((0x0Au64 << 24) | (100u64 << 16)), Some(100.0), "the TCC offset is not TjMax");
        assert_eq!(decode_tjmax(0), None);
    }

    #[test]
    fn the_package_register_has_no_valid_bit() {
        assert_eq!(decode_package(62u64 << 16, 100.0), Some(38.0));
        assert_eq!(decode_package(0, 100.0), Some(100.0), "a readout of zero is at TjMax");
    }

    #[test]
    fn perf_status_carries_the_ratio_and_vid() {
        // Ratio 57, VID 1.300 V (10650 / 8192).
        let raw = (10650u64 << 32) | (57u64 << 8);
        let (ratio, vid) = decode_perf_status(raw);
        assert_eq!(ratio, 57.0);
        assert!((vid.unwrap() - 1.300).abs() < 0.001);
        assert_eq!(decode_perf_status(57u64 << 8).1, None, "no VID reported");
    }

    #[test]
    fn the_bus_clock_prefers_a_measured_one() {
        // This machine: MCHBAR reads 100.0, CPUID says 100, the TSC divides to 99.6.
        assert_eq!(pick_bus_clock(Some(100.02), 100, 99.6), 100.02);
        assert_eq!(pick_bus_clock(None, 100, 99.6), 100.0);
        assert_eq!(pick_bus_clock(None, 0, 99.6), 99.6, "the TSC only as the last resort");
        assert_eq!(pick_bus_clock(Some(0.0), 100, f64::NAN), 100.0, "an unread MCHBAR is ignored");
        assert!(pick_bus_clock(None, 0, f64::NAN).is_nan());
    }

    #[test]
    fn counters_and_energy_become_megahertz_and_watts() {
        assert_eq!(counter_mhz(0, 5_700_000_000, 1.0), Some(5700.0));
        assert_eq!(counter_mhz(10, 5, 1.0), None, "a counter that went backwards is no reading");
        assert_eq!(counter_mhz(0, 1, 0.0), None);
        // 2^14 units per joule, 50 J over 2 s.
        assert_eq!(energy_watts(0, 50 << 14, 2.0, 14), Some(25.0));
        // A wrap between samples is handled.
        assert!((energy_watts(u32::MAX - 16383, 0, 1.0, 14).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn aggregates_skip_missing_cores() {
        assert_eq!(mean(&[40.0, f64::NAN, 50.0]), 45.0);
        assert_eq!(max(&[1.1, f64::NAN, 1.3]), 1.3);
        assert_eq!(sum(&[1.5, f64::NAN, 2.5]), 4.0);
        assert!(mean(&[f64::NAN]).is_nan());
        assert!(sum(&[]).is_nan());
    }

    #[test]
    fn this_machine_is_supported_or_says_why() {
        let sys = SystemInfo::detect();
        let mut out = Batch::default();
        // Without PawnIO (CI, or before it is installed) the source declines with a reason; with it,
        // it starts. Either way it never panics, and a decline is never silent.
        match CpuMsr::detect(&sys, &mut out) {
            Some(mut msr) => {
                msr.poll(&mut out);
                assert!(out.values.iter().any(|(k, _)| k == "cpu/clk/aperf"));
            }
            None => assert!(out.status.is_some()),
        }
    }

    /// What a sweep costs on this machine: `cargo test -p roch-sensors poll_cost -- --ignored
    /// --nocapture`, with PawnIO installed and from an elevated prompt.
    #[test]
    #[ignore = "needs PawnIO and an elevated process"]
    fn poll_cost() {
        let sys = SystemInfo::detect();
        let mut msr = CpuMsr::detect(&sys, &mut Batch::default()).expect("PawnIO and its IntelMSR module");
        let mut took: Vec<f64> = (0..60)
            .map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let started = Instant::now();
                msr.poll(&mut Batch::default());
                started.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        took.sort_by(f64::total_cmp);
        println!(
            "{} polls: median {:.2} ms, 95th {:.2} ms, worst {:.2} ms",
            took.len(),
            took[took.len() / 2],
            took[took.len() * 95 / 100],
            took[took.len() - 1]
        );
    }
}
