//! AMD's SMU PM table on AM5 (Roch Viewer's `smu_power`, `power_metrics`, `smu_voltages` and
//! `smu_clocks`): Tctl against its limit, the I/O die, L3 and VRM temperatures, PPT/TDC/EDC against
//! their limits and the PBO scalar, VDDCR_VDD/SOC, VDDG CCD/IOD, VDDP and VDD_MISC, and FCLK/UCLK.
//!
//! Like Roch Viewer's, this follows ZenStates-Core and ZenTimings by irusanov
//! (https://github.com/irusanov), both GPL-3.0: the mailbox addresses, the message numbers, the
//! table-version gate and the bounds on decoded values were taken from or checked against that
//! work, and copyright in those parts remains with their author. Every table offset was then
//! confirmed by Roch Viewer on one bench, MSI B850MPOWER + Ryzen 7 9850X3D, BIOS 1.A21, PM table
//! 0x620105, against HWiNFO, ZenStates and ZenTimings; the evidence is recorded beside each offset.
//!
//! One reading is one SMN session (`hw::pci`): the PCI mutex held throughout, and the SMN index put
//! back at the end whatever happened. Inside it:
//!
//!   1. 0x05 TableVersion. Anything but 0x620105 ends the read there, before a table is asked for:
//!      the offsets were confirmed on that layout and are not pointed at another.
//!   2. 0x04 TableBase with arguments (1, 1, 0, 0, 0, 0): where the SMU keeps the table. Zero, all
//!      ones, or a table reaching past 48 address bits is refused.
//!   3. 0x03 TransferTable: the SMU refreshes the table there.
//!   4. Physical reads of the dwords below, each a little-endian float.
//!   5. 0x6D PboScalar, only when a power figure decoded: the scalar has its own getter, and an
//!      answer other than OK costs the scalar's row and nothing else.
//!
//! Those four are the only messages that exist (`pci::RsmuCommand`), and all four are reads:
//! nothing here sets a limit, a voltage or a clock. A decoded value outside the band its sensor can
//! physically report is dropped rather than shown, because a wrong offset printing a plausible
//! number is worse than a dash.
//!
//! Gated twice before the mailbox is touched: by CPUID, an AMD family 19h or 1Ah processor; and by
//! Roch Viewer's own test, a desktop Ryzen 9000 by name, the one part the mailbox and the offsets
//! were validated on. A Ryzen 7000 is family 19h but fails the second gate on purpose: Roch Viewer
//! never ran the mailbox there, and its table is another version.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::engine::{Batch, Source};
use crate::hw::cpuid::Vendor;
use crate::hw::inpout::{InpOut, driver};
use crate::hw::pci::{self, RootComplex, RsmuCommand, Smn, SmnError};
use crate::model::{DeviceInfo, Group, SensorInfo};
use crate::sources::intel_mchbar::cpu_device;
use crate::system::SystemInfo;

const CPU: &str = "cpu";
const VOLT_DECIMALS: u8 = 3;

// ---------------------------------------------------------------------------------------------
// The mailbox sequence (Roch Viewer's `smu_clocks`, which the voltage and power readers share).

/// The table layout every offset below was confirmed on.
pub const EXPECTED_TABLE_VERSION: u32 = 0x0062_0105;
/// That layout's length: the room a table base has to leave below the address limit.
pub const PM_TABLE_LENGTH: u64 = 0x724;
/// What 0x04 TableBase is asked with.
pub const ADDRESS_REQUEST_ARGUMENTS: [u32; 6] = [1, 1, 0, 0, 0, 0];
const NO_ARGUMENTS: [u32; 6] = [0; 6];
/// A table base past 48 address bits is not a physical address on these parts.
const PHYSICAL_LIMIT: u64 = 1 << 48;
/// The SMU's answer to a message it carried out.
const RSMU_OK: u32 = 1;

/// Roch Viewer's mailbox timing: a quarter second for any one answer, a second for a whole run.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_millis(250);
pub const RUN_TIMEOUT: Duration = Duration::from_secs(1);
const POLL_STEP: Duration = Duration::from_millis(1);

/// How long one run's readings stand before the mailbox is asked again: Roch Viewer's
/// LIVE_CACHE_SECONDS. A poll sooner than this adds nothing rather than repeating the sequence, so
/// a 250 ms poll interval does not quadruple the traffic on a mailbox other tools share.
const RUN_SPACING: Duration = Duration::from_millis(750);
/// Failed runs in a row, before any good one, after which detection stops asking: Roch Viewer's
/// LAZY_READ_ATTEMPTS.
const ATTEMPTS: u32 = 3;

// ---------------------------------------------------------------------------------------------
// Clocks: FCLK and UCLK, with MCLK as their cross-check (Roch Viewer's `smu_clocks`, offsets from
// the MIT gnr-smu map, confirmed against ZenTimings).

pub const OFFSET_FCLK: u64 = 0x11C;
pub const OFFSET_UCLK: u64 = 0x12C;
pub const OFFSET_MCLK: u64 = 0x13C;
pub const FCLK_MIN_MHZ: f64 = 600.0;
pub const FCLK_MAX_MHZ: f64 = 3000.0;
pub const UCLK_MIN_MHZ: f64 = 600.0;
pub const UCLK_MAX_MHZ: f64 = 4200.0;

// ---------------------------------------------------------------------------------------------
// Rails (Roch Viewer's `voltage_rails` and `smu_voltages`).

/// Which transport serves a rail. Only the PM-table rails are read here; the others are listed
/// because the evidence that put them elsewhere belongs with the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailSource {
    PmTable,
    /// The board's Super I/O (`superio`): proven absent from both the table and the PMIC.
    SuperIo,
    /// Each DIMM's own PMIC (`dimm`): not in this table at any offset.
    Pmic,
}

/// One rail, and the band that makes a reading of it believable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rail {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub min_volts: f64,
    pub max_volts: f64,
    pub source: RailSource,
}

/// Roch Viewer's RAILS, in its order: core, SoC, fabric, memory I/O, the DIMM rails, misc.
pub static RAILS: [Rail; 11] = [
    Rail {
        key: "vddcr_vdd",
        label: "VDDCR_VDD",
        group: "Core",
        min_volts: 0.40,
        max_volts: 1.55,
        source: RailSource::PmTable,
    },
    Rail {
        key: "vddcr_soc",
        label: "VDDCR_SOC",
        group: "SoC",
        min_volts: 0.70,
        max_volts: 1.40,
        source: RailSource::PmTable,
    },
    Rail {
        key: "cldo_vddq",
        label: "VDDP",
        group: "Fabric",
        min_volts: 0.60,
        max_volts: 1.35,
        source: RailSource::PmTable,
    },
    Rail {
        key: "vddg_ccd",
        label: "VDDG CCD",
        group: "Fabric",
        min_volts: 0.60,
        max_volts: 1.35,
        source: RailSource::PmTable,
    },
    Rail {
        key: "vddg_iod",
        label: "VDDG IOD",
        group: "Fabric",
        min_volts: 0.60,
        max_volts: 1.35,
        source: RailSource::PmTable,
    },
    Rail {
        key: "vddio_mem",
        label: "VDDIO",
        group: "Memory",
        min_volts: 0.90,
        max_volts: 1.80,
        source: RailSource::SuperIo,
    },
    Rail { key: "vtt", label: "VTT", group: "Memory", min_volts: 1.50, max_volts: 2.10, source: RailSource::SuperIo },
    Rail {
        key: "dram_vdd",
        label: "DRAM VDD",
        group: "Memory",
        min_volts: 0.90,
        max_volts: 1.65,
        source: RailSource::Pmic,
    },
    Rail {
        key: "dram_vddq",
        label: "DRAM VDDQ",
        group: "Memory",
        min_volts: 0.90,
        max_volts: 1.65,
        source: RailSource::Pmic,
    },
    // VPP is the 1.8 V wordline rail, hence the higher band.
    Rail {
        key: "dram_vpp",
        label: "DRAM VPP",
        group: "Memory",
        min_volts: 1.50,
        max_volts: 2.10,
        source: RailSource::Pmic,
    },
    // Named as HWiNFO names it: its "CPU VDD_MISC Voltage (SVI3 TFN)" is this rail to the millivolt.
    Rail {
        key: "cdd_misc",
        label: "VDD_MISC",
        group: "Misc",
        min_volts: 0.40,
        max_volts: 1.35,
        source: RailSource::PmTable,
    },
];

/// The widest band any rail on this platform can have; it has to clear DRAM VPP's 1.8 V.
pub const CANDIDATE_MIN_VOLTS: f64 = 0.40;
pub const CANDIDATE_MAX_VOLTS: f64 = 2.10;

/// rail key -> PM-table offset: Roch Viewer's CONFIRMED_VOLTAGE_OFFSETS, which holds only rails
/// confirmed on the bench. The table was captured idle and under an all-core load, every rail
/// ZenTimings 1.43.0 showed was matched within 2.5 mV, and each was checked over eight back-to-back
/// captures: a real rail holds steady or drifts by tens of millivolts, never by volts.
pub static VOLTAGE_OFFSETS: [(&str, u64); 6] = [
    // VSOC as delivered. Switching the memory profile is the one test that separates the SoC
    // offsets: 0x0D4/0x14C read the requested 1.30000 at 8200 MT/s and 1.02501 at JEDEC, which is
    // what ZenTimings shows; 0x0D8 reads 1.20287 and 1.01555, what the chip actually gets. Not
    // 0x04C: it moved the wrong way across the same change, 1.24517 -> 1.27690.
    ("vddcr_soc", 0x0D8),
    // SVI3 telemetry, HWiNFO's "VDDCR_VDD Voltage (SVI3 TFN)". The rails sit in five-dword groups
    // whose ends the VRM temperatures pin: VDD 0x0C0-0x0D0 (0x0D0 = 33.0 °C, HWiNFO 32), SOC
    // 0x0D4-0x0E4 (0x0E4 = 35.8 °C, HWiNFO 36). The SoC rail is the second dword of its group, so
    // the core rail is the second of its own. Not 0x044, which ranged 1.34-4.21 V when sampled.
    ("vddcr_vdd", 0x0C4),
    // VDDP: ZenTimings' CLDO VDDP 1.0478 V, the only offset at that value. A BIOS change to 1.10 V
    // moved it 1.04776 -> 1.09806.
    ("cldo_vddq", 0x434),
    // VDDG IOD and CCD both read 1.04921 V until they were set apart in BIOS (IOD 1.08, CCD 1.06):
    // 0x40C went to 1.08216 and 0x414 to 1.06250, the reverse of the order assumed while they
    // matched.
    ("vddg_iod", 0x40C),
    ("vddg_ccd", 0x414),
    // VDD_MISC: ZenTimings' 1.1000 V, 0x0E8 (duplicated at 0x0EC) at 1.09999 with zero spread. These
    // two were once taken for the VDDG pair only because 1.100 V looked like a VDDG value.
    ("cdd_misc", 0x0E8),
    // Not here, proven by BIOS changes: DRAM VDD/VDDQ/VPP, which moved no dword of the table (they
    // are the DIMM PMIC's), and VDDIO, which a +30 mV change moved nowhere in it (the Super I/O's).
];

// ---------------------------------------------------------------------------------------------
// Power and current (Roch Viewer's `power_metrics` and `smu_power`).

/// One power figure, and the band that makes a reading of it believable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metric {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub min_value: f64,
    pub max_value: f64,
    pub decimals: u8,
}

/// Roch Viewer's METRICS, in the order ZenStates' Power page shows them.
pub static METRICS: [Metric; 4] = [
    Metric { key: "ppt", label: "PPT", unit: "W", min_value: 1.0, max_value: 1000.0, decimals: 1 },
    Metric { key: "tdc", label: "TDC", unit: "A", min_value: 1.0, max_value: 1000.0, decimals: 1 },
    Metric { key: "edc", label: "EDC", unit: "A", min_value: 1.0, max_value: 1000.0, decimals: 1 },
    Metric { key: "scalar", label: "Scalar", unit: "x", min_value: 0.1, max_value: 10.0, decimals: 2 },
];

/// A metric's two PM-table offsets: (its reading, its limit), either of which a table may lack.
pub type MetricOffsets = (Option<u64>, Option<u64>);

/// metric key -> (value offset, limit offset): Roch Viewer's CONFIRMED_POWER_OFFSETS. Matched
/// against ZenStates 2.0.0's limits page on the bench, and each value offset then watched under an
/// all-core load, the documented Zen (limit, value) pairing:
///
/// ```text
///   PPT  limit 0x008 = 162 W exactly   value 0x00C   41.0 -> 143.7 W
///   TDC  limit 0x020 = 120 A exactly   value 0x024    9.7 ->  98.6 A
///   EDC  limit 0x0FC = 180 A exactly   value 0x100   38.0 -> 100.0 A
/// ```
///
/// The scalar is absent on purpose: 0x060 and 0x064 both hold exactly 1.0, far too common a float
/// to claim on a value match, and it has its own getter, 0x6D.
pub static POWER_OFFSETS: [(&str, MetricOffsets); 3] =
    [("ppt", (Some(0x00C), Some(0x008))), ("tdc", (Some(0x024), Some(0x020))), ("edc", (Some(0x100), Some(0x0FC)))];

// ---------------------------------------------------------------------------------------------
// Temperatures (Roch Viewer's `smu_power`).

/// sensor key -> offset, in °C: Roch Viewer's CONFIRMED_TEMPERATURE_OFFSETS. Each was matched on
/// its rise under load as well as at both ends, because absolute values alone once picked the
/// wrong offset: 0x458 peaks within 0.1 of L3's maximum, but climbs 3.5 where L3 climbs 16.8. Read
/// in the same episode, the rows already confirmed land within 0.7 °C of HWiNFO's own.
pub static TEMPERATURE_OFFSETS: [(&str, u64); 8] = [
    // Tctl: 43.7 -> 81.8 under an all-core load, paired with its limit at 0x028.
    ("cpu", 0x02C),
    // The VRM sensors that pinned the SVI3 group boundaries: 33.0 against HWiNFO's 32, and 35.8
    // against its 36.
    ("vdd_vrm", 0x0D0),
    ("soc_vrm", 0x0E4),
    // HWiNFO's own row three times over: 32.00/32.0 idle, 35.00/35.0 at two load peaks. The
    // flattest sensor in the table, which is why one match was not enough.
    ("misc_vrm", 0x0F8),
    // 38.92 against HWiNFO's 38.1 on a cooled idle, rising 4.0 against its 3.3. The weakest of the
    // three: its loaded peak read 43.5 against 41.4.
    ("iod_average", 0x1A8),
    // 42.21 against 41.8 cooled, 46.1 against 45.8 loaded.
    ("iod_hotspot", 0x458),
    // 29.80 against 29.2 cooled, 46.2 against 46.0 loaded: a climb nothing else in the table nears.
    ("l3", 0x700),
    // The limit Tctl is held to, read rather than assumed: this bench reports 90.0, and HWiNFO's
    // "Thermal Limit 45.3%" is 40.8/90 to the decimal. A constant 95 would show five degrees of
    // headroom that are not there.
    ("cpu_limit", 0x028),
];

/// A silicon sensor outside this band is a decode error, not a reading.
pub const TEMPERATURE_MIN_C: f64 = 0.0;
pub const TEMPERATURE_MAX_C: f64 = 125.0;

// ---------------------------------------------------------------------------------------------
// Decoding.

/// Table reads by absolute physical address. `None` is a read the driver refused.
pub type TableRead<'a> = &'a dyn Fn(u64) -> Option<u32>;

/// One PM-table dword as the little-endian float it holds. Non-finite is no reading.
pub fn decode_table_float(raw: u32) -> Option<f64> {
    let value = f32::from_bits(raw);
    value.is_finite().then_some(value as f64)
}

fn table_float(read: TableRead<'_>, base: u64, offset: u64) -> Option<f64> {
    read(base.checked_add(offset)?).and_then(decode_table_float)
}

pub fn rail(key: &str) -> Option<&'static Rail> {
    RAILS.iter().find(|r| r.key == key)
}

pub fn metric(key: &str) -> Option<&'static Metric> {
    METRICS.iter().find(|m| m.key == key)
}

/// The reading, if it is a voltage this rail can have.
pub fn validate_voltage(rail: &Rail, volts: f64) -> Option<f64> {
    (volts.is_finite() && (rail.min_volts..=rail.max_volts).contains(&volts)).then_some(volts)
}

/// The reading, if it is one this figure can have.
pub fn validate_power(metric: &Metric, value: f64) -> Option<f64> {
    (value.is_finite() && (metric.min_value..=metric.max_value).contains(&value)).then_some(value)
}

/// Roch Viewer's `validate_clocks`: both clocks inside their bands and near a whole MHz (telemetry
/// is a float, the clocks are discrete), and UCLK at MCLK or half of it, against the memory
/// controller's own MCLK when there is one and the table's otherwise. Both, rounded, or neither.
pub fn validate_clocks(fclk: f64, uclk: f64, mclk: Option<f64>, umc_mclk: Option<f64>) -> Option<(f64, f64)> {
    if !(FCLK_MIN_MHZ..=FCLK_MAX_MHZ).contains(&fclk) || !(UCLK_MIN_MHZ..=UCLK_MAX_MHZ).contains(&uclk) {
        return None;
    }
    // Python's round() is half-to-even, so these are too.
    if (fclk - fclk.round_ties_even()).abs() > 1.5 || (uclk - uclk.round_ties_even()).abs() > 1.5 {
        return None;
    }
    if let Some(reference) = umc_mclk.or(mclk)
        && reference > 0.0
    {
        let off = (uclk - reference).abs().min((uclk - reference / 2.0).abs());
        if off > (reference * 0.08).max(50.0) {
            return None;
        }
    }
    Some((fclk.round_ties_even(), uclk.round_ties_even()))
}

/// (FCLK, UCLK) in MHz, with the table's MCLK as the cross-check; all three have to decode.
pub fn decode_clocks(read: TableRead<'_>, base: u64, umc_mclk: Option<f64>) -> Option<(f64, f64)> {
    let fclk = table_float(read, base, OFFSET_FCLK)?;
    let uclk = table_float(read, base, OFFSET_UCLK)?;
    let mclk = table_float(read, base, OFFSET_MCLK)?;
    validate_clocks(fclk, uclk, Some(mclk), umc_mclk)
}

/// The rails among `offsets` that decode inside their bands. Unknown keys are skipped.
pub fn decode_voltages(read: TableRead<'_>, base: u64, offsets: &[(&str, u64)]) -> BTreeMap<&'static str, f64> {
    let mut values = BTreeMap::new();
    for &(key, offset) in offsets {
        let Some(rail) = rail(key) else { continue };
        if let Some(volts) = table_float(read, base, offset).and_then(|v| validate_voltage(rail, v)) {
            values.insert(rail.key, volts);
        }
    }
    values
}

/// (figures, limits) for the metrics among `offsets`, each kept only inside its band.
pub fn decode_power(
    read: TableRead<'_>,
    base: u64,
    offsets: &[(&str, MetricOffsets)],
) -> (BTreeMap<&'static str, f64>, BTreeMap<&'static str, f64>) {
    let (mut values, mut limits) = (BTreeMap::new(), BTreeMap::new());
    for &(key, (value_offset, limit_offset)) in offsets {
        let Some(metric) = metric(key) else { continue };
        let decode = |offset: Option<u64>| table_float(read, base, offset?).and_then(|v| validate_power(metric, v));
        if let Some(v) = decode(value_offset) {
            values.insert(metric.key, v);
        }
        if let Some(v) = decode(limit_offset) {
            limits.insert(metric.key, v);
        }
    }
    (values, limits)
}

/// The sensors among `offsets` that decode inside the silicon band.
pub fn decode_temperatures(
    read: TableRead<'_>,
    base: u64,
    offsets: &[(&'static str, u64)],
) -> BTreeMap<&'static str, f64> {
    offsets
        .iter()
        .filter_map(|&(key, offset)| {
            let celsius = table_float(read, base, offset)?;
            (TEMPERATURE_MIN_C..=TEMPERATURE_MAX_C).contains(&celsius).then_some((key, celsius))
        })
        .collect()
}

/// What one run read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub version: u32,
    pub base: u64,
    /// (FCLK, UCLK) in MHz: both or neither.
    pub clocks: Option<(f64, f64)>,
    pub voltages: BTreeMap<&'static str, f64>,
    /// Live figures by metric key, the PBO scalar among them when its getter answered.
    pub power: BTreeMap<&'static str, f64>,
    pub limits: BTreeMap<&'static str, f64>,
    pub temperatures: BTreeMap<&'static str, f64>,
}

impl Snapshot {
    fn is_empty(&self) -> bool {
        self.clocks.is_none()
            && self.voltages.is_empty()
            && self.power.is_empty()
            && self.limits.is_empty()
            && self.temperatures.is_empty()
    }
}

/// Everything the confirmed offsets hold, from a table already transferred to `base`.
pub fn decode_table(read: TableRead<'_>, version: u32, base: u64) -> Snapshot {
    let (power, limits) = decode_power(read, base, &POWER_OFFSETS);
    // Temperatures ride on the power figures, as they do in Roch Viewer's reader: a table in which
    // no power figure is believable is not trusted for temperatures, whose band includes the 0.0
    // an unwritten table holds.
    let temperatures = if power.is_empty() && limits.is_empty() {
        BTreeMap::new()
    } else {
        decode_temperatures(read, base, &TEMPERATURE_OFFSETS)
    };
    Snapshot {
        version,
        base,
        clocks: decode_clocks(read, base, None),
        voltages: decode_voltages(read, base, &VOLTAGE_OFFSETS),
        power,
        limits,
        temperatures,
    }
}

/// Roch Viewer's bounds on the address firmware gives: not zero, not all ones, and the whole table
/// inside 48 address bits.
pub fn check_table_base(base: u64) -> Result<u64, RsmuError> {
    if base == 0 || base == u64::MAX || base > PHYSICAL_LIMIT || base + PM_TABLE_LENGTH > PHYSICAL_LIMIT {
        Err(RsmuError::Base(base))
    } else {
        Ok(base)
    }
}

// ---------------------------------------------------------------------------------------------
// The mailbox.

/// A monotonic clock and a way to wait, so the mailbox's timing can be tested without waiting.
pub trait Clock {
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

/// The real clock.
pub struct WallClock(Instant);

impl WallClock {
    pub fn new() -> WallClock {
        WallClock(Instant::now())
    }
}

impl Default for WallClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for WallClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Why a run read nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RsmuError {
    Smn(SmnError),
    /// The mailbox gave no answer within [`RESPONSE_TIMEOUT`].
    Timeout,
    /// The run as a whole took longer than [`RUN_TIMEOUT`].
    RunBudget,
    ClockBackward,
    /// The SMU answered a message with something other than OK.
    Refused(RsmuCommand, u32),
    /// The table is not the layout the offsets were confirmed on.
    Version(u32),
    /// Firmware gave a table address that cannot be one.
    Base(u64),
    /// The table was transferred, and nothing in it decoded inside its band.
    Nothing,
}

impl From<SmnError> for RsmuError {
    fn from(e: SmnError) -> Self {
        RsmuError::Smn(e)
    }
}

impl std::fmt::Display for RsmuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RsmuError::Smn(e) => write!(f, "{e}"),
            RsmuError::Timeout => {
                write!(f, "the RSMU mailbox did not answer within {} ms", RESPONSE_TIMEOUT.as_millis())
            }
            RsmuError::RunBudget => write!(f, "the RSMU run took longer than its {} s budget", RUN_TIMEOUT.as_secs()),
            RsmuError::ClockBackward => write!(f, "the monotonic clock moved backward"),
            RsmuError::Refused(c, r) => write!(f, "RSMU message 0x{:02X} returned 0x{r:08X}", c.code()),
            RsmuError::Version(v) => {
                write!(f, "PM-table version 0x{v:06X} is not the approved 0x{EXPECTED_TABLE_VERSION:06X}")
            }
            RsmuError::Base(b) => write!(f, "firmware returned an impossible PM-table base 0x{b:X}"),
            RsmuError::Nothing => write!(f, "no reading in the PM table decoded inside its band"),
        }
    }
}

impl std::error::Error for RsmuError {}

impl RsmuError {
    /// Nothing on this machine will change the answer while it runs: a table of another version,
    /// a root complex that is not AMD's, no driver, or an SMN index that could not be put back.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            RsmuError::Version(_)
                | RsmuError::Smn(
                    SmnError::Vendor(_)
                        | SmnError::RestoreFailed(_)
                        | SmnError::Unavailable(_)
                        | SmnError::DriverClosed
                )
        )
    }
}

struct Mailbox<'s, 'r, R: RootComplex + ?Sized, C: Clock + ?Sized> {
    smn: &'s mut Smn<'r, R>,
    clock: &'s C,
    last: Option<Duration>,
    deadline: Option<Duration>,
}

impl<R: RootComplex + ?Sized, C: Clock + ?Sized> Mailbox<'_, '_, R, C> {
    fn now(&mut self) -> Result<Duration, RsmuError> {
        let now = self.clock.now();
        if self.last.is_some_and(|last| now < last) {
            return Err(RsmuError::ClockBackward);
        }
        self.last = Some(now);
        if self.deadline.is_some_and(|deadline| now > deadline) {
            return Err(RsmuError::RunBudget);
        }
        Ok(now)
    }

    /// Wait for a non-zero response: the mailbox idle, or the answer to what was just posted.
    fn wait(&mut self) -> Result<u32, RsmuError> {
        let deadline = self.now()? + RESPONSE_TIMEOUT;
        loop {
            if self.now()? > deadline {
                return Err(RsmuError::Timeout);
            }
            let response = self.smn.response()?;
            let after = self.now()?;
            if after > deadline {
                return Err(RsmuError::Timeout);
            }
            if response != 0 {
                return Ok(response);
            }
            if after >= deadline {
                return Err(RsmuError::Timeout);
            }
            self.clock.sleep(POLL_STEP.min(deadline - after));
        }
    }

    /// Wait for the mailbox, clear the response, write the arguments, post, and wait for OK.
    fn command(&mut self, command: RsmuCommand, arguments: &[u32; 6]) -> Result<(), RsmuError> {
        self.wait()?;
        self.smn.clear_response()?;
        self.smn.write_arguments(arguments)?;
        self.smn.issue(command)?;
        match self.wait()? {
            RSMU_OK => Ok(()),
            response => Err(RsmuError::Refused(command, response)),
        }
    }

    fn table_base(&mut self) -> Result<u64, RsmuError> {
        self.command(RsmuCommand::TableBase, &ADDRESS_REQUEST_ARGUMENTS)?;
        let low = self.smn.argument(0)? as u64;
        let high = self.smn.argument(1)? as u64;
        check_table_base(low | (high << 32))
    }

    /// The PBO scalar from its getter. Any failure, a firmware that does not know the message
    /// included, blanks the scalar and nothing else, and never decodes whatever arg0 held.
    fn pbo_scalar(&mut self) -> Option<f64> {
        self.command(RsmuCommand::PboScalar, &NO_ARGUMENTS).ok()?;
        let raw = self.smn.argument(0).ok()?;
        decode_table_float(raw).and_then(|v| validate_power(metric("scalar")?, v))
    }
}

/// One run of the sequence in the module documentation, inside an SMN session that is already
/// open. `table` reads the transferred table by physical address.
pub fn read_pm_table<R: RootComplex + ?Sized, C: Clock + ?Sized>(
    smn: &mut Smn<'_, R>,
    clock: &C,
    table: TableRead<'_>,
) -> Result<Snapshot, RsmuError> {
    let mut mailbox = Mailbox { smn, clock, last: None, deadline: None };
    mailbox.deadline = Some(mailbox.now()? + RUN_TIMEOUT);
    mailbox.command(RsmuCommand::TableVersion, &NO_ARGUMENTS)?;
    let version = mailbox.smn.argument(0)?;
    if version != EXPECTED_TABLE_VERSION {
        return Err(RsmuError::Version(version));
    }
    let base = mailbox.table_base()?;
    mailbox.command(RsmuCommand::TransferTable, &NO_ARGUMENTS)?;
    let mut snapshot = decode_table(table, version, base);
    // Asked for last, with the table already in hand, so a firmware that will not answer the
    // getter costs the scalar and nothing else.
    if (!snapshot.power.is_empty() || !snapshot.limits.is_empty())
        && let Some(scalar) = mailbox.pbo_scalar()
    {
        snapshot.power.insert("scalar", scalar);
    }
    if snapshot.is_empty() {
        return Err(RsmuError::Nothing);
    }
    Ok(snapshot)
}

// ---------------------------------------------------------------------------------------------
// The CPU gate (Roch Viewer's `platform_profiles`).

/// Roch Viewer's `is_granite_ridge_cpu`: a desktop AM5 Ryzen whose model number starts with 9.
pub fn is_granite_ridge_cpu(name: &str) -> bool {
    let name = name.trim().to_lowercase();
    desktop_am5_model(&name).is_some_and(|model| model.starts_with('9'))
}

/// Roch Viewer's `_is_desktop_am5_cpu`: a Ryzen 7000 or 9000, not Threadripper, with no suffix
/// but X, X3D, F or XT. The 8000 desktop parts are Phoenix APUs (the 8700F included), and G, GE
/// and mobile parts use other layouts.
fn desktop_am5_model(name: &str) -> Option<&str> {
    if !name.contains("ryzen") || name.contains("threadripper") {
        return None;
    }
    let (model, suffix) = ryzen_model(name)?;
    (model.starts_with(['7', '9']) && matches!(suffix, "" | "x" | "x3d" | "f" | "xt")).then_some(model)
}

/// `re.search(r"ryzen\s+(?:[3579]\s+)?(?:pro\s+)?(\d{4})([a-z0-9]*)", name)`: the four-digit model
/// number and its suffix, from the first place the pattern matches.
fn ryzen_model(name: &str) -> Option<(&str, &str)> {
    let mut from = 0;
    while let Some(found) = name[from..].find("ryzen") {
        let at = from + found;
        if let Some(model) = model_after(&name[at + "ryzen".len()..]) {
            return Some(model);
        }
        from = at + 1;
    }
    None
}

fn model_after(rest: &str) -> Option<(&str, &str)> {
    let rest = whitespace(rest)?;
    let rest = optional(rest, |r| whitespace(r.strip_prefix(['3', '5', '7', '9'])?));
    let rest = optional(rest, |r| whitespace(r.strip_prefix("pro")?));
    let model = rest.get(..4).filter(|d| d.bytes().all(|b| b.is_ascii_digit()))?;
    let tail = &rest[4..];
    let suffix = tail.bytes().take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit()).count();
    Some((model, &tail[..suffix]))
}

/// `\s+`: at least one.
fn whitespace(s: &str) -> Option<&str> {
    let t = s.trim_start();
    (t.len() < s.len()).then_some(t)
}

/// `(?:...)?`: the part when it matches, and nothing consumed when it does not.
fn optional<'a>(s: &'a str, part: impl Fn(&'a str) -> Option<&'a str>) -> &'a str {
    part(s).unwrap_or(s)
}

/// Whether this machine's processor may be asked for its PM table, and if not, why not.
pub fn cpu_gate(sys: &SystemInfo) -> Result<(), String> {
    let name = sys.cpu_name();
    let (family, model) = (sys.cpu.family, sys.cpu.model);
    if sys.vendor() != Vendor::Amd {
        return Err(format!("SMU PM table: not read on {name}; the RSMU mailbox is AMD's."));
    }
    if !matches!(family, 0x19 | 0x1A) {
        return Err(format!(
            "SMU PM table: not read on {name} (family 0x{family:X} model 0x{model:X}); the mailbox map is for families 19h and 1Ah."
        ));
    }
    if !is_granite_ridge_cpu(&name) {
        return Err(format!(
            "SMU PM table: not read on {name} (family 0x{family:X} model 0x{model:X}); Roch Viewer validated the mailbox and every table offset on desktop Ryzen 9000 (Granite Ridge) only, and they are not pointed at silicon they were not written for."
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The source.

fn offset_of(table: &[(&str, u64)], key: &str) -> u64 {
    table.iter().find(|(k, _)| *k == key).map_or(0, |&(_, o)| o)
}

fn table_source(offset: u64) -> String {
    format!("SMU PM table 0x{EXPECTED_TABLE_VERSION:06X} +0x{offset:03X}")
}

/// Temperature rows in display order: (sensor key, row key, label).
static TEMPERATURE_ROWS: [(&str, &str, &str); 8] = [
    ("cpu", "temp/tctl", "CPU (Tctl)"),
    ("cpu_limit", "temp/tctl_limit", "Thermal Limit"),
    ("iod_average", "temp/iod_average", "IOD Average"),
    ("iod_hotspot", "temp/iod_hotspot", "IOD Hotspot"),
    ("l3", "temp/l3", "L3"),
    ("vdd_vrm", "temp/vrm_vdd", "VDDCR_VDD VRM"),
    ("soc_vrm", "temp/vrm_soc", "VDDCR_SOC VRM"),
    ("misc_vrm", "temp/vrm_misc", "VDD_MISC VRM"),
];

/// Every row this source can show, with this snapshot's reading for it.
fn rows(s: Option<&Snapshot>) -> Vec<(SensorInfo, Option<f64>)> {
    let mut rows = Vec::new();
    for (key, row, label) in TEMPERATURE_ROWS {
        let mut source = table_source(offset_of(&TEMPERATURE_OFFSETS, key));
        if key == "cpu_limit" {
            source.push_str(", the limit Tctl is held to");
        }
        rows.push((
            SensorInfo::new(CPU, row, Group::Temperatures, label, "°C", 1).source(source),
            s.and_then(|s| s.temperatures.get(key).copied()),
        ));
    }
    for rail in RAILS.iter().filter(|r| r.source == RailSource::PmTable) {
        let slug = rail.label.to_lowercase().replace(' ', "_");
        rows.push((
            SensorInfo::new(CPU, format!("volt/{slug}"), Group::Voltages, rail.label, "V", VOLT_DECIMALS)
                .source(table_source(offset_of(&VOLTAGE_OFFSETS, rail.key))),
            s.and_then(|s| s.voltages.get(rail.key).copied()),
        ));
    }
    rows.push((
        SensorInfo::new(CPU, "clk/fclk", Group::Clocks, "Infinity Fabric Clock (FCLK)", "MHz", 0)
            .source(table_source(OFFSET_FCLK)),
        s.and_then(|s| s.clocks).map(|(fclk, _)| fclk),
    ));
    rows.push((
        SensorInfo::new(CPU, "clk/uclk", Group::Clocks, "Memory Controller Clock (UCLK)", "MHz", 0)
            .source(format!("{}, checked against MCLK at +0x{OFFSET_MCLK:03X}", table_source(OFFSET_UCLK))),
        s.and_then(|s| s.clocks).map(|(_, uclk)| uclk),
    ));
    for &(key, (value_offset, limit_offset)) in &POWER_OFFSETS {
        let Some(m) = metric(key) else { continue };
        let (group, prefix) = if m.unit == "A" { (Group::Current, "current") } else { (Group::Power, "power") };
        rows.push((
            SensorInfo::new(CPU, format!("{prefix}/{key}"), group, m.label, m.unit, m.decimals)
                .source(table_source(value_offset.unwrap_or(0))),
            s.and_then(|s| s.power.get(key).copied()),
        ));
        rows.push((
            SensorInfo::new(
                CPU,
                format!("{prefix}/{key}_limit"),
                group,
                format!("{} Limit", m.label),
                m.unit,
                m.decimals,
            )
            .source(table_source(limit_offset.unwrap_or(0))),
            s.and_then(|s| s.limits.get(key).copied()),
        ));
    }
    if let Some(m) = metric("scalar") {
        rows.push((
            SensorInfo::new(CPU, "power/scalar", Group::Other, "PBO Scalar", m.unit, m.decimals)
                .source("RSMU message 0x6D, GetPBOScalar"),
            s.and_then(|s| s.power.get("scalar").copied()),
        ));
    }
    rows
}

fn describe(s: &Snapshot) -> String {
    format!(
        "RSMU PM table 0x{:06X} at 0x{:X}: {} temperatures, {} power and current figures, {} rails{}.",
        s.version,
        s.base,
        s.temperatures.len(),
        s.power.len() + s.limits.len(),
        s.voltages.len(),
        if s.clocks.is_some() { ", FCLK/UCLK" } else { "" }
    )
}

pub struct AmdSmu {
    driver: &'static InpOut,
    cpu: DeviceInfo,
    clock: WallClock,
    last_run: Instant,
    status: String,
    stopped: bool,
}

impl AmdSmu {
    pub fn detect(sys: &SystemInfo, out: &mut Batch, driver_choice: crate::hw::Driver) -> Option<AmdSmu> {
        if let Err(why) = cpu_gate(sys) {
            out.status(why);
            return None;
        }
        // PawnIO's RyzenSMU module would serve this too, but its mailbox sequence has not been
        // run against the B850M bench these offsets were confirmed on, so the table is still read
        // the way it was confirmed: through inpoutx64.
        if !driver_choice.inpout() {
            out.status("SMU PM table: read through inpoutx64 only, which --driver pawnio rules out.");
            return None;
        }
        let driver = match driver() {
            Ok(io) => io,
            Err(e) => {
                out.status(format!("SMU PM table unavailable: {e}"));
                return None;
            }
        };
        let mut smu = AmdSmu {
            driver,
            cpu: cpu_device(sys),
            clock: WallClock::new(),
            last_run: Instant::now(),
            status: String::new(),
            stopped: false,
        };
        let mut last = None;
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(RUN_SPACING);
            }
            match smu.run() {
                Ok(snapshot) => {
                    smu.publish(out, Some(&snapshot));
                    return Some(smu);
                }
                Err(e) if e.is_final() => {
                    out.status(format!("SMU PM table: {e}. Nothing was decoded, and the mailbox is not asked again."));
                    return None;
                }
                Err(e) => last = Some(e),
            }
        }
        let why = last.map_or_else(String::new, |e| e.to_string());
        out.status(format!("SMU PM table: {why} ({ATTEMPTS} attempts); not asked again this session."));
        None
    }

    fn run(&mut self) -> Result<Snapshot, RsmuError> {
        self.last_run = Instant::now();
        let driver = self.driver;
        let clock = &self.clock;
        pci::amd_smn(|smn| read_pm_table(smn, clock, &|address| driver.phys_read32(address)))
    }

    /// Readings for every row that has one; NaN for rows already shown that this run missed. A
    /// row is declared the first time it reads, so a sensor this board lacks gets no row at all.
    fn publish(&mut self, out: &mut Batch, snapshot: Option<&Snapshot>) {
        out.device(self.cpu.clone());
        for (info, value) in rows(snapshot) {
            match value {
                Some(v) => out.put(&info, v),
                None => out.value(info.key, f64::NAN),
            }
        }
        if let Some(s) = snapshot {
            self.status = describe(s);
        }
        out.status(self.status.clone());
    }
}

impl Source for AmdSmu {
    fn name(&self) -> &str {
        "SMU PM table"
    }

    fn poll(&mut self, out: &mut Batch) {
        if self.stopped || self.last_run.elapsed() < RUN_SPACING {
            out.device(self.cpu.clone());
            out.status(self.status.clone());
            return;
        }
        match self.run() {
            Ok(snapshot) => self.publish(out, Some(&snapshot)),
            Err(e) => {
                self.stopped = e.is_final();
                self.status = if self.stopped {
                    format!("SMU PM table: stopped, {e}.")
                } else {
                    format!("SMU PM table: {e}; trying again next poll.")
                };
                self.publish(out, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::cpuid::CpuId;
    use crate::hw::pci::{RSMU_ARG0, RSMU_MSG, RSMU_RSP};
    use crate::model::format_value;
    use std::cell::{Cell, RefCell};
    use std::collections::{HashMap, HashSet};

    fn dword(value: f64) -> u32 {
        (value as f32).to_bits()
    }

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() < tolerance
    }

    fn voltage_offset(key: &str) -> u64 {
        VOLTAGE_OFFSETS.iter().find(|(k, _)| *k == key).unwrap().1
    }

    // -- The mailbox, against a fake SMU behind a fake 00:00.0 (Roch Viewer's PboScalarTest and
    //    the sequence its readers share).

    /// Where the bench's firmware put the table; any plausible DRAM address does.
    const BASE: u64 = 0xDE7C_0000;
    /// What the index held before the session.
    const PRIOR: u32 = 0xABCD_EF00;

    /// The bench's idle capture at the confirmed offsets, from Roch Viewer's recorded evidence.
    fn bench_table() -> HashMap<u64, u32> {
        let mut t = HashMap::new();
        for (offset, value) in [
            (0x008, 162.0),
            (0x00C, 41.0244),
            (0x020, 120.0),
            (0x024, 9.6954),
            (0x0FC, 180.0),
            (0x100, 38.0),
            (0x028, 90.0),
            (0x02C, 43.7),
            (0x0D0, 33.0),
            (0x0E4, 35.8),
            (0x0F8, 32.0),
            (0x1A8, 38.92),
            (0x458, 42.21),
            (0x700, 29.80),
            (0x0C4, 1.25),
            (OFFSET_FCLK, 2000.0),
            (OFFSET_UCLK, 2050.0),
            (OFFSET_MCLK, 4100.0),
        ] {
            t.insert(BASE + offset, dword(value));
        }
        // The raw dwords Roch Viewer recorded at the rail offsets.
        for (offset, raw) in [
            (0x0D8, 0x3F99_F7C5),
            (0x434, 0x3F86_1D28),
            (0x414, 0x3F86_4C5D),
            (0x40C, 0x3F86_4C5D),
            (0x0E8, 0x3F8C_CC55),
        ] {
            t.insert(BASE + offset, raw);
        }
        t
    }

    struct FakeSmu {
        vendor: u32,
        version: u32,
        base: u64,
        /// What 0x6D leaves in arg0, or `None` for a firmware that answers it 0xFE (unknown).
        scalar: Option<u32>,
        /// False: the SMU takes messages and never answers them.
        answers: bool,
        /// (message, response): the SMU refuses this one.
        refuse: Option<(u32, u32)>,
        table: HashMap<u64, u32>,
        /// Every table read answers zero: a table nobody wrote.
        zero_fill: bool,
        index: Cell<u32>,
        regs: RefCell<HashMap<u32, u32>>,
        sent: RefCell<Vec<(u32, [u32; 6])>>,
        index_writes: RefCell<Vec<u32>>,
        data_writes: RefCell<Vec<(u32, u32)>>,
    }

    impl FakeSmu {
        fn new() -> FakeSmu {
            FakeSmu {
                vendor: 0x1022,
                version: EXPECTED_TABLE_VERSION,
                base: BASE,
                scalar: Some(dword(1.0)),
                answers: true,
                refuse: None,
                table: bench_table(),
                zero_fill: false,
                index: Cell::new(PRIOR),
                // An idle mailbox shows the last message's answer.
                regs: RefCell::new(HashMap::from([(RSMU_RSP, RSMU_OK)])),
                sent: RefCell::new(Vec::new()),
                index_writes: RefCell::new(Vec::new()),
                data_writes: RefCell::new(Vec::new()),
            }
        }

        fn commands(&self) -> Vec<u32> {
            self.sent.borrow().iter().map(|&(c, _)| c).collect()
        }

        fn read_table(&self, address: u64) -> Option<u32> {
            self.table.get(&address).copied().or(self.zero_fill.then_some(0))
        }

        fn execute(&self, command: u32) {
            let mut regs = self.regs.borrow_mut();
            let args: [u32; 6] = std::array::from_fn(|i| regs.get(&(RSMU_ARG0 + 4 * i as u32)).copied().unwrap_or(0));
            self.sent.borrow_mut().push((command, args));
            if !self.answers {
                return;
            }
            if let Some((refused, response)) = self.refuse
                && refused == command
            {
                regs.insert(RSMU_RSP, response);
                return;
            }
            let response = match command {
                0x05 => {
                    regs.insert(RSMU_ARG0, self.version);
                    RSMU_OK
                }
                0x04 => {
                    regs.insert(RSMU_ARG0, self.base as u32);
                    regs.insert(RSMU_ARG0 + 4, (self.base >> 32) as u32);
                    RSMU_OK
                }
                0x03 => RSMU_OK,
                0x6D => match self.scalar {
                    Some(raw) => {
                        regs.insert(RSMU_ARG0, raw);
                        RSMU_OK
                    }
                    None => 0xFE,
                },
                _ => 0xFE,
            };
            regs.insert(RSMU_RSP, response);
        }
    }

    impl RootComplex for FakeSmu {
        type Guard = ();

        fn driver_open(&self) -> bool {
            true
        }

        fn lock(&self) -> Option<()> {
            Some(())
        }

        fn read_config(&self, register: u16) -> Option<u32> {
            match register {
                pci::VENDOR_REGISTER => Some(0x14D8_0000 | self.vendor),
                pci::SMN_INDEX => Some(self.index.get()),
                pci::SMN_DATA => Some(self.regs.borrow().get(&self.index.get()).copied().unwrap_or(0)),
                _ => None,
            }
        }

        fn write_index(&self, value: u32) -> bool {
            self.index_writes.borrow_mut().push(value);
            self.index.set(value);
            true
        }

        fn write_data(&self, value: u32) -> bool {
            let at = self.index.get();
            self.data_writes.borrow_mut().push((at, value));
            self.regs.borrow_mut().insert(at, value);
            if at == RSMU_MSG {
                self.execute(value);
            }
            true
        }
    }

    /// Each reading of the clock moves it on by `step`; a sleep moves it by what was asked.
    struct TestClock {
        t: Cell<Duration>,
        step: Duration,
    }

    impl TestClock {
        fn new(step: Duration) -> TestClock {
            TestClock { t: Cell::new(Duration::ZERO), step }
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Duration {
            self.t.set(self.t.get() + self.step);
            self.t.get()
        }

        fn sleep(&self, duration: Duration) {
            self.t.set(self.t.get() + duration);
        }
    }

    fn run_with(smu: &FakeSmu, clock: &TestClock) -> Result<Snapshot, RsmuError> {
        pci::session(smu, |smn| read_pm_table(smn, clock, &|address| smu.read_table(address)))
    }

    fn run(smu: &FakeSmu) -> Result<Snapshot, RsmuError> {
        run_with(smu, &TestClock::new(Duration::from_micros(10)))
    }

    #[test]
    fn a_run_sends_the_four_read_messages_in_order_and_writes_only_the_mailbox() {
        let smu = FakeSmu::new();
        run(&smu).unwrap();
        assert_eq!(
            *smu.sent.borrow(),
            vec![(0x05, [0; 6]), (0x04, [1, 1, 0, 0, 0, 0]), (0x03, [0; 6]), (0x6D, [0; 6])]
        );
        let mailbox: Vec<u32> = [RSMU_MSG, RSMU_RSP].into_iter().chain((0..6).map(|i| RSMU_ARG0 + 4 * i)).collect();
        assert!(smu.data_writes.borrow().iter().all(|(at, _)| mailbox.contains(at)));
        let messages: Vec<u32> =
            smu.data_writes.borrow().iter().filter(|(at, _)| *at == RSMU_MSG).map(|&(_, v)| v).collect();
        let permitted: Vec<u32> = RsmuCommand::ALL.iter().map(|c| c.code()).collect();
        assert!(messages.iter().all(|m| permitted.contains(m)));
        assert!(smu.index_writes.borrow().iter().all(|a| mailbox.contains(a) || *a == PRIOR));
        assert_eq!(smu.index_writes.borrow().last(), Some(&PRIOR));
        assert_eq!(smu.index.get(), PRIOR);
    }

    #[test]
    fn the_bench_table_decodes_to_the_bench_readings() {
        let s = run(&FakeSmu::new()).unwrap();
        assert_eq!((s.version, s.base), (0x0062_0105, BASE));
        assert_eq!(s.clocks, Some((2000.0, 2050.0)));
        assert_eq!(s.voltages.len(), 6);
        assert!(close(s.voltages["vddcr_soc"], 1.2029, 5e-4));
        assert!(close(s.voltages["cldo_vddq"], 1.0478, 5e-4));
        assert!(close(s.voltages["cdd_misc"], 1.1000, 5e-4));
        assert_eq!(s.limits, BTreeMap::from([("edc", 180.0), ("ppt", 162.0), ("tdc", 120.0)]));
        assert!(close(s.power["ppt"], 41.0244, 5e-4));
        assert_eq!(s.power.get("scalar"), Some(&1.0));
        assert_eq!(s.temperatures.len(), 8);
        assert!(close(s.temperatures["cpu"], 43.7, 5e-4));
        assert_eq!(s.temperatures["cpu_limit"], 90.0);
        assert!(close(s.temperatures["l3"], 29.80, 5e-4));
    }

    #[test]
    fn a_foreign_table_version_stops_after_the_version_message() {
        let mut smu = FakeSmu::new();
        smu.version = 0x0054_0104;
        let err = run(&smu).unwrap_err();
        assert_eq!(err, RsmuError::Version(0x0054_0104));
        assert!(err.is_final());
        assert!(err.to_string().contains("0x540104"));
        assert_eq!(smu.commands(), vec![0x05]);
        assert_eq!(smu.index.get(), PRIOR);
    }

    #[test]
    fn an_impossible_table_base_is_refused_before_the_transfer() {
        for base in [0, PHYSICAL_LIMIT + 0x1000, PHYSICAL_LIMIT - 0x10] {
            let mut smu = FakeSmu::new();
            smu.base = base;
            assert_eq!(run(&smu), Err(RsmuError::Base(base)), "base 0x{base:X}");
            assert_eq!(smu.commands(), vec![0x05, 0x04]);
        }
        assert!(check_table_base(u64::MAX).is_err());
        assert_eq!(check_table_base(0x1_0000_0000), Ok(0x1_0000_0000));
    }

    #[test]
    fn a_mailbox_that_never_answers_times_out_and_the_index_goes_back() {
        let mut smu = FakeSmu::new();
        smu.answers = false;
        assert_eq!(run(&smu), Err(RsmuError::Timeout));
        assert_eq!(smu.commands(), vec![0x05]);
        assert_eq!(smu.index.get(), PRIOR);
    }

    #[test]
    fn a_refused_message_is_an_error_not_a_reading() {
        let mut smu = FakeSmu::new();
        smu.refuse = Some((0x04, 0xFF));
        assert_eq!(run(&smu), Err(RsmuError::Refused(RsmuCommand::TableBase, 0xFF)));
        assert_eq!(smu.commands(), vec![0x05, 0x04]);
    }

    #[test]
    fn the_run_budget_bounds_the_whole_sequence() {
        // A tenth of a second per look at the clock: each answer arrives in time, the run does not.
        let smu = FakeSmu::new();
        assert_eq!(run_with(&smu, &TestClock::new(Duration::from_millis(100))), Err(RsmuError::RunBudget));
        assert_eq!(smu.commands(), vec![0x05, 0x04]);
        assert_eq!(smu.index.get(), PRIOR);
    }

    #[test]
    fn a_foreign_root_complex_gets_no_message() {
        let mut smu = FakeSmu::new();
        smu.vendor = 0x8086;
        let err = run(&smu).unwrap_err();
        assert_eq!(err, RsmuError::Smn(SmnError::Vendor(0x8086)));
        assert!(err.is_final());
        assert!(smu.commands().is_empty());
        assert!(smu.index_writes.borrow().is_empty());
    }

    #[test]
    fn the_scalar_getter_answer_is_decoded_as_a_float() {
        let mut smu = FakeSmu::new();
        smu.scalar = Some(dword(3.5));
        assert_eq!(run(&smu).unwrap().power.get("scalar"), Some(&3.5));
    }

    #[test]
    fn a_rejected_scalar_getter_costs_the_scalar_and_nothing_else() {
        // A firmware that does not know the message answers non-OK, and arg0 still holds the table
        // base's low dword: the row must go blank rather than show that.
        let mut smu = FakeSmu::new();
        smu.scalar = None;
        let s = run(&smu).unwrap();
        assert!(!s.power.contains_key("scalar"));
        assert!(s.power.contains_key("ppt") && s.limits.contains_key("ppt"));
        assert_eq!(s.temperatures.len(), 8);
        assert_eq!(smu.commands(), vec![0x05, 0x04, 0x03, 0x6D]);
    }

    #[test]
    fn a_scalar_outside_its_band_is_dropped() {
        let mut smu = FakeSmu::new();
        smu.scalar = Some(dword(99.0));
        assert!(!run(&smu).unwrap().power.contains_key("scalar"));
    }

    #[test]
    fn without_a_power_figure_there_is_no_scalar_message_and_no_temperature() {
        let mut smu = FakeSmu::new();
        for offset in [0x008, 0x00C, 0x020, 0x024, 0x0FC, 0x100] {
            smu.table.remove(&(BASE + offset));
        }
        let s = run(&smu).unwrap();
        assert!(s.power.is_empty() && s.limits.is_empty());
        assert!(s.temperatures.is_empty());
        assert_eq!(s.voltages.len(), 6);
        assert_eq!(smu.commands(), vec![0x05, 0x04, 0x03]);
    }

    #[test]
    fn a_table_nobody_wrote_is_no_reading() {
        let mut smu = FakeSmu::new();
        smu.table.clear();
        smu.zero_fill = true;
        assert_eq!(run(&smu), Err(RsmuError::Nothing));
        assert_eq!(smu.commands(), vec![0x05, 0x04, 0x03]);
    }

    #[test]
    fn the_reader_makes_no_write_of_its_own() {
        // Roch Viewer's PowerIsReadOnlyTest. Every write is pci.rs's, closed over the mailbox.
        let source = include_str!("amd_smu.rs");
        let reader = source.split("#[cfg(test)]").next().unwrap();
        for raw in ["SetPhysLong", "write_index(", "write_data(", "phys_write"] {
            assert!(!reader.contains(raw), "{raw}");
        }
    }

    // -- Rows.

    #[test]
    fn every_row_sits_under_the_cpu_with_its_own_key() {
        let s = run(&FakeSmu::new()).unwrap();
        let full = rows(Some(&s));
        let keys: HashSet<&str> = full.iter().map(|(i, _)| i.key.as_str()).collect();
        assert_eq!(keys.len(), full.len());
        assert!(full.iter().all(|(i, _)| i.device == CPU && i.key.starts_with("cpu/")));
        // 8 temperatures, 6 rails, 2 clocks, 3 figures and 3 limits, the scalar.
        assert_eq!(full.len(), 23);
        assert!(full.iter().all(|(_, v)| v.is_some()), "the bench table fills every row");
        assert!(rows(None).iter().all(|(_, v)| v.is_none()));
        let labels: Vec<&str> = full.iter().map(|(i, _)| i.label.as_str()).collect();
        for wanted in ["CPU (Tctl)", "Thermal Limit", "PPT", "PPT Limit", "TDC", "EDC Limit", "VDDP", "VDDG CCD"] {
            assert!(labels.contains(&wanted), "{wanted}");
        }
        let tdc = full.iter().find(|(i, _)| i.key == "cpu/current/tdc").unwrap();
        assert_eq!((tdc.0.group, tdc.0.unit.as_str()), (Group::Current, "A"));
    }

    // -- The gate (Roch Viewer's test_platform_profiles and SmuClockHelpersTest).

    #[test]
    fn granite_ridge_is_told_apart_from_raphael() {
        assert!(is_granite_ridge_cpu("AMD Ryzen 7 9850X3D"));
        assert!(is_granite_ridge_cpu("AMD Ryzen 9 9950X"));
        assert!(!is_granite_ridge_cpu("AMD Ryzen 7 7800X3D"));
        assert!(!is_granite_ridge_cpu("AMD Ryzen 7 8700G"));
    }

    #[test]
    fn the_name_gate_takes_the_whole_brand_string_and_refuses_intel() {
        assert!(is_granite_ridge_cpu("AMD Ryzen 7 9850X3D 8-Core Processor"));
        assert!(!is_granite_ridge_cpu("Intel(R) Core(TM) Ultra 7 270K Plus"));
        assert!(!is_granite_ridge_cpu(""));
    }

    #[test]
    fn mobile_apu_and_workstation_parts_are_refused() {
        for name in [
            "AMD Ryzen 9 9955HX",
            "AMD Ryzen 5 9600G",
            "AMD Ryzen AI 9 HX 370",
            "AMD Ryzen Threadripper PRO 9995WX",
            "AMD Ryzen 7 7840HS with Radeon 780M Graphics",
            "AMD Ryzen 7 8700F",
        ] {
            assert!(!is_granite_ridge_cpu(name), "{name}");
        }
        for name in ["AMD Ryzen 5 9600", "AMD Ryzen 7 9700X", "AMD Ryzen 9 9900X3D", "AMD Ryzen 5 9600F"] {
            assert!(is_granite_ridge_cpu(name), "{name}");
        }
    }

    fn system(vendor: &str, family: u32, model: u32, brand: &str) -> SystemInfo {
        SystemInfo {
            cpu: CpuId { vendor: vendor.into(), brand: brand.into(), family, model, ..Default::default() },
            ..Default::default()
        }
    }

    #[test]
    fn cpuid_gates_the_family_before_the_name_is_read() {
        assert!(cpu_gate(&system("AuthenticAMD", 0x1A, 0x44, "AMD Ryzen 7 9850X3D")).is_ok());
        let raphael = cpu_gate(&system("AuthenticAMD", 0x19, 0x61, "AMD Ryzen 9 7950X")).unwrap_err();
        assert!(raphael.contains("Granite Ridge"), "{raphael}");
        // A name that would pass, on a family the mailbox map is not for.
        let zen2 = cpu_gate(&system("AuthenticAMD", 0x17, 0x71, "AMD Ryzen 9 9950X")).unwrap_err();
        assert!(zen2.contains("family 0x17"), "{zen2}");
        assert!(cpu_gate(&system("GenuineIntel", 6, 0xB7, "Intel Core i9-14900KF")).is_err());
    }

    // -- Clocks (Roch Viewer's SmuClockHelpersTest).

    #[test]
    fn the_zentimings_shape_validates() {
        let fclk = decode_table_float(dword(2000.0)).unwrap();
        let uclk = decode_table_float(dword(2050.0)).unwrap();
        assert_eq!(validate_clocks(fclk, uclk, None, Some(4100.0)), Some((2000.0, 2050.0)));
    }

    #[test]
    fn nonsense_clocks_are_rejected() {
        assert_eq!(validate_clocks(50.0, 2050.0, None, Some(4100.0)), None);
        assert_eq!(validate_clocks(2000.0, 5000.0, None, None), None);
        assert_eq!(validate_clocks(f64::NAN, 2050.0, None, None), None);
    }

    #[test]
    fn uclk_has_to_sit_at_mclk_or_half_of_it() {
        assert_eq!(validate_clocks(2000.0, 3000.0, Some(4100.0), None), None);
        assert_eq!(validate_clocks(2000.0, 3000.0, Some(4100.0), Some(3000.0)), Some((2000.0, 3000.0)));
        assert_eq!(validate_clocks(2000.0, 2050.0, Some(4100.0), None), Some((2000.0, 2050.0)));
        // No reference to check against: the bands alone decide.
        assert_eq!(validate_clocks(2000.0, 3000.0, Some(0.0), None), Some((2000.0, 3000.0)));
    }

    #[test]
    fn clocks_round_half_to_even_as_roch_viewer_does() {
        assert_eq!(validate_clocks(2000.5, 2051.5, None, None), Some((2000.0, 2052.0)));
    }

    #[test]
    fn the_expected_version_is_the_benchs() {
        assert_eq!(EXPECTED_TABLE_VERSION, 0x620105);
    }

    #[test]
    fn every_offset_lies_inside_the_table_and_none_is_shared() {
        let mut all: Vec<u64> = vec![OFFSET_FCLK, OFFSET_UCLK, OFFSET_MCLK];
        all.extend(VOLTAGE_OFFSETS.iter().map(|&(_, o)| o));
        all.extend(TEMPERATURE_OFFSETS.iter().map(|&(_, o)| o));
        for &(_, (value, limit)) in &POWER_OFFSETS {
            all.extend(value.into_iter().chain(limit));
        }
        assert!(all.iter().all(|&o| o % 4 == 0 && o + 4 <= PM_TABLE_LENGTH));
        assert_eq!(all.iter().collect::<HashSet<_>>().len(), all.len());
    }

    // -- Power (Roch Viewer's test_amd_smu_power).

    #[test]
    fn metrics_render_in_the_zenstates_order() {
        assert_eq!(METRICS.map(|m| m.label), ["PPT", "TDC", "EDC", "Scalar"]);
        assert_eq!(METRICS.map(|m| m.unit), ["W", "A", "A", "x"]);
    }

    #[test]
    fn metric_keys_are_unique() {
        let keys: HashSet<&str> = METRICS.iter().map(|m| m.key).collect();
        assert_eq!(keys.len(), METRICS.len());
    }

    #[test]
    fn validate_power_rejects_out_of_range() {
        let ppt = metric("ppt").unwrap();
        assert_eq!(validate_power(ppt, 162.0), Some(162.0));
        for bad in [0.0, 5000.0, f64::NAN] {
            assert_eq!(validate_power(ppt, bad), None);
        }
    }

    #[test]
    fn figures_and_limits_print_as_rows_of_their_own() {
        // Roch Viewer prints "41.0 / 162.0 W" in one cell; here the figure and the limit are two
        // rows, each with its own minimum and maximum.
        let ppt = metric("ppt").unwrap();
        assert_eq!(format_value(41.0, ppt.unit, ppt.decimals), "41.0 W");
        assert_eq!(format_value(162.0, ppt.unit, ppt.decimals), "162.0 W");
    }

    #[test]
    fn the_scalar_prints_two_decimals() {
        let scalar = metric("scalar").unwrap();
        assert_eq!(format_value(1.0, scalar.unit, scalar.decimals), "1.00 x");
    }

    #[test]
    fn confirmed_power_offsets_match_the_zenstates_reference() {
        // PPT 162 W, TDC 120 A, EDC 180 A on MSI B850MPOWER + 9850X3D.
        assert_eq!(
            POWER_OFFSETS,
            [
                ("ppt", (Some(0x00C), Some(0x008))),
                ("tdc", (Some(0x024), Some(0x020))),
                ("edc", (Some(0x100), Some(0x0FC)))
            ]
        );
    }

    #[test]
    fn the_scalar_is_never_claimed_as_a_table_offset() {
        assert!(!POWER_OFFSETS.iter().any(|(key, _)| *key == "scalar"));
    }

    #[test]
    fn every_confirmed_power_key_names_a_real_metric() {
        for (key, _) in &POWER_OFFSETS {
            assert!(metric(key).is_some(), "{key}");
        }
    }

    #[test]
    fn decode_power_float_rejects_non_finite() {
        assert_eq!(decode_table_float(dword(f64::INFINITY)), None);
        assert_eq!(decode_table_float(f32::NAN.to_bits()), None);
    }

    #[test]
    fn decode_power_returns_captured_values_and_limits() {
        let table = HashMap::from([
            (0x008u64, dword(162.0)),
            (0x00C, dword(41.0244)),
            (0x020, dword(120.0)),
            (0x024, dword(9.6954)),
            (0x0FC, dword(180.0)),
            (0x100, dword(38.0)),
        ]);
        let (values, limits) = decode_power(&|a| table.get(&a).copied(), 0, &POWER_OFFSETS);
        assert_eq!(limits, BTreeMap::from([("ppt", 162.0), ("tdc", 120.0), ("edc", 180.0)]));
        assert!(close(values["ppt"], 41.0244, 5e-4));
        assert!(close(values["tdc"], 9.6954, 5e-4));
        assert!(close(values["edc"], 38.0, 5e-4));
    }

    #[test]
    fn loaded_values_stay_inside_their_ranges() {
        // Readings under an all-core load: a band too tight would blank exactly when it matters.
        for (key, value) in [("ppt", 143.6983), ("tdc", 98.5523), ("edc", 100.0)] {
            assert!(validate_power(metric(key).unwrap(), value).is_some(), "{key}");
        }
    }

    #[test]
    fn an_out_of_range_reading_is_dropped_not_shown() {
        let table = HashMap::from([(0x008u64, dword(162.0)), (0x00C, dword(9e9))]);
        let (values, limits) = decode_power(&|a| table.get(&a).copied(), 0, &[("ppt", (Some(0x00C), Some(0x008)))]);
        assert_eq!(limits, BTreeMap::from([("ppt", 162.0)]));
        assert!(!values.contains_key("ppt"));
    }

    #[test]
    fn a_failing_read_decodes_nothing() {
        let (values, limits) = decode_power(&|_| None, 0, &[("ppt", (Some(0x00C), Some(0x008)))]);
        assert!(values.is_empty() && limits.is_empty());
    }

    #[test]
    fn unknown_metric_keys_are_ignored() {
        let (values, limits) = decode_power(&|_| Some(dword(1.0)), 0, &[("not_a_metric", (Some(0x10), Some(0x14)))]);
        assert!(values.is_empty() && limits.is_empty());
    }

    // -- Temperatures.

    #[test]
    fn confirmed_temperature_offsets_match_the_bench() {
        assert_eq!(
            TEMPERATURE_OFFSETS,
            [
                ("cpu", 0x02C),
                ("vdd_vrm", 0x0D0),
                ("soc_vrm", 0x0E4),
                ("misc_vrm", 0x0F8),
                ("iod_average", 0x1A8),
                ("iod_hotspot", 0x458),
                ("l3", 0x700),
                ("cpu_limit", 0x028),
            ]
        );
    }

    #[test]
    fn a_temperature_outside_the_silicon_band_is_dropped() {
        let table = HashMap::from([
            (0x10u64, dword(43.7)),
            (0x14, dword(130.0)),
            (0x18, dword(-5.0)),
            (0x1C, dword(f64::INFINITY)),
        ]);
        let t = decode_temperatures(
            &|a| table.get(&a).copied(),
            0,
            &[("cpu", 0x10), ("l3", 0x14), ("iod_average", 0x18), ("vdd_vrm", 0x1C), ("soc_vrm", 0x20)],
        );
        assert_eq!(t.keys().copied().collect::<Vec<_>>(), vec!["cpu"]);
    }

    // -- Rails (Roch Viewer's test_amd_smu_voltages).

    #[test]
    fn rail_keys_and_labels_are_unique() {
        let keys: HashSet<&str> = RAILS.iter().map(|r| r.key).collect();
        let labels: HashSet<&str> = RAILS.iter().map(|r| r.label).collect();
        assert_eq!((keys.len(), labels.len()), (RAILS.len(), RAILS.len()));
    }

    #[test]
    fn every_rail_has_a_sane_range() {
        for r in &RAILS {
            assert!(r.min_volts < r.max_volts, "{}", r.label);
            assert!(r.min_volts >= CANDIDATE_MIN_VOLTS && r.max_volts <= CANDIDATE_MAX_VOLTS, "{}", r.label);
        }
    }

    #[test]
    fn the_requested_rail_groups_are_all_present() {
        let groups: HashSet<&str> = RAILS.iter().map(|r| r.group).collect();
        assert_eq!(groups, HashSet::from(["Core", "SoC", "Fabric", "Memory", "Misc"]));
    }

    #[test]
    fn rails_render_in_the_requested_order() {
        assert_eq!(
            RAILS.map(|r| r.label),
            [
                "VDDCR_VDD",
                "VDDCR_SOC",
                "VDDP",
                "VDDG CCD",
                "VDDG IOD",
                "VDDIO",
                "VTT",
                "DRAM VDD",
                "DRAM VDDQ",
                "DRAM VPP",
                "VDD_MISC"
            ]
        );
    }

    #[test]
    fn the_dram_vpp_band_clears_its_1v8_nominal() {
        assert!(validate_voltage(rail("dram_vpp").unwrap(), 1.80).is_some());
        assert!((CANDIDATE_MIN_VOLTS..=CANDIDATE_MAX_VOLTS).contains(&1.80));
    }

    #[test]
    fn validate_voltage_rejects_out_of_range() {
        let soc = rail("vddcr_soc").unwrap();
        assert_eq!(validate_voltage(soc, 1.20), Some(1.20));
        for bad in [1.90, 0.10, f64::NAN] {
            assert_eq!(validate_voltage(soc, bad), None);
        }
    }

    #[test]
    fn rails_print_to_the_millivolt() {
        assert_eq!(format_value(1.4, "V", VOLT_DECIMALS), "1.400 V");
        assert_eq!(format_value(1.23456, "V", VOLT_DECIMALS), "1.235 V");
    }

    #[test]
    fn confirmed_voltage_offsets_match_the_b850mpower_capture() {
        // Pinned to the idle/load diff on MSI B850MPOWER + 9850X3D, BIOS 1.A21, PM table 0x620105.
        // Changing one of these means re-running the probe.
        assert_eq!(
            VOLTAGE_OFFSETS,
            [
                ("vddcr_soc", 0x0D8),
                ("vddcr_vdd", 0x0C4),
                ("cldo_vddq", 0x434),
                ("vddg_iod", 0x40C),
                ("vddg_ccd", 0x414),
                ("cdd_misc", 0x0E8),
            ]
        );
    }

    #[test]
    fn the_svi3_rails_sit_in_their_own_five_dword_groups() {
        assert_eq!(voltage_offset("vddcr_vdd"), 0x0C4);
        assert_eq!(voltage_offset("vddcr_soc"), 0x0D8);
        assert_eq!(voltage_offset("vddcr_soc") - voltage_offset("vddcr_vdd"), 0x14);
    }

    #[test]
    fn the_vddg_order_matches_the_bios_separation_test() {
        assert_eq!(voltage_offset("vddg_iod"), 0x40C);
        assert_eq!(voltage_offset("vddg_ccd"), 0x414);
    }

    #[test]
    fn the_soc_rail_is_the_delivered_value_not_the_request() {
        assert_eq!(voltage_offset("vddcr_soc"), 0x0D8);
        assert!(![0x0D4, 0x14C].contains(&voltage_offset("vddcr_soc")));
    }

    #[test]
    fn vddio_is_never_mapped_to_the_pm_table() {
        assert!(!VOLTAGE_OFFSETS.iter().any(|(k, _)| *k == "vddio_mem"));
        for offset in [0x048, 0x09C, 0x0A0, 0x0A8, 0x0B4, 0x0B8, 0x0BC] {
            assert!(!VOLTAGE_OFFSETS.iter().any(|&(_, o)| o == offset), "0x{offset:03X}");
        }
    }

    #[test]
    fn offset_0x04c_is_not_treated_as_the_soc_rail() {
        assert!(!VOLTAGE_OFFSETS.iter().any(|&(_, o)| o == 0x04C));
    }

    #[test]
    fn the_vdd_misc_offsets_are_not_reused_as_the_vddg_pair() {
        assert_eq!(voltage_offset("cdd_misc"), 0x0E8);
        for vddg in ["vddg_ccd", "vddg_iod"] {
            assert!(![0x0E8, 0x0EC].contains(&voltage_offset(vddg)), "{vddg}");
        }
    }

    #[test]
    fn confirmed_offsets_survive_a_memory_profile_change() {
        // Read at 8200 MT/s and again at JEDEC 4800, against ZenTimings 1.43.0 at both.
        for (key, at_8200, at_jedec) in [
            ("vddcr_soc", 1.20287, 1.01555),
            ("cldo_vddq", 1.04776, 0.79920),
            ("vddg_ccd", 1.04921, 0.90310),
            ("vddg_iod", 1.04921, 0.90310),
            ("cdd_misc", 1.09999, 1.10000),
        ] {
            let r = rail(key).unwrap();
            assert!(validate_voltage(r, at_8200).is_some() && validate_voltage(r, at_jedec).is_some(), "{key}");
        }
    }

    #[test]
    fn dimm_pmic_rails_are_never_mapped_to_this_table() {
        for key in ["dram_vdd", "dram_vddq", "dram_vpp"] {
            assert!(!VOLTAGE_OFFSETS.iter().any(|(k, _)| *k == key), "{key}");
            assert_eq!(rail(key).unwrap().source, RailSource::Pmic);
        }
    }

    #[test]
    fn offset_0x044_is_not_treated_as_the_core_rail() {
        assert!(!VOLTAGE_OFFSETS.iter().any(|&(_, o)| o == 0x044));
    }

    #[test]
    fn every_confirmed_offset_names_a_real_rail() {
        for (key, _) in &VOLTAGE_OFFSETS {
            assert_eq!(rail(key).map(|r| r.source), Some(RailSource::PmTable), "{key}");
        }
    }

    #[test]
    fn unresolved_rails_stay_out_of_the_map() {
        for key in ["vddio_mem", "vtt"] {
            assert!(!VOLTAGE_OFFSETS.iter().any(|(k, _)| *k == key), "{key}");
        }
    }

    #[test]
    fn captured_values_decode_inside_their_declared_ranges() {
        // The raw dwords at each confirmed offset in the idle capture.
        for (key, raw, expected) in [
            ("vddcr_soc", 0x3F99_F7C5, 1.2029),
            ("cldo_vddq", 0x3F86_1D28, 1.0478),
            ("vddg_ccd", 0x3F86_4C5D, 1.0492),
            ("vddg_iod", 0x3F86_4C5D, 1.0492),
            ("cdd_misc", 0x3F8C_CC55, 1.1000),
        ] {
            let volts = decode_table_float(raw).unwrap();
            assert!(close(volts, expected, 5e-4), "{key}: {volts}");
            assert!(validate_voltage(rail(key).unwrap(), volts).is_some(), "{key}");
        }
    }

    #[test]
    fn the_observed_rail_swing_stays_inside_the_declared_ranges() {
        // The widest values seen across the idle, loaded and eight-sample captures.
        for (key, lo, hi) in [
            ("vddcr_soc", 1.1356, 1.2631),
            ("cldo_vddq", 1.0478, 1.0478),
            ("vddg_ccd", 1.0492, 1.0492),
            ("vddg_iod", 1.0492, 1.0492),
            ("cdd_misc", 1.1000, 1.1000),
        ] {
            let r = rail(key).unwrap();
            assert!(validate_voltage(r, lo).is_some() && validate_voltage(r, hi).is_some(), "{key}");
        }
    }

    #[test]
    fn decode_voltage_float_round_trips() {
        assert!(close(decode_table_float(dword(1.35)).unwrap(), 1.35, 5e-6));
    }

    #[test]
    fn decode_voltage_float_rejects_non_finite() {
        assert_eq!(decode_table_float(dword(f64::INFINITY)), None);
    }

    #[test]
    fn decode_voltages_keeps_only_in_range_rails() {
        let table = HashMap::from([(0x10u64, dword(1.40)), (0x14, dword(9.90))]);
        let values = decode_voltages(&|a| table.get(&a).copied(), 0, &[("vddio_mem", 0x10), ("vddcr_soc", 0x14)]);
        assert!(close(values["vddio_mem"], 1.40, 5e-6));
        assert!(!values.contains_key("vddcr_soc"));
    }

    #[test]
    fn decode_voltages_ignores_unknown_rail_keys() {
        assert!(decode_voltages(&|_| Some(dword(1.10)), 0, &[("not_a_rail", 0x10)]).is_empty());
    }

    #[test]
    fn decode_voltages_survives_a_failing_read() {
        assert!(decode_voltages(&|_| None, 0, &[("vddio_mem", 0x10)]).is_empty());
    }
}
