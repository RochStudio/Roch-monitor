//! What the monitor shows: devices, the sensors under them, and each sensor's running figures.
//!
//! The shape is HWiNFO's sensor list with HWMonitor's grouping: one heading per device (the CPU,
//! the board's Super I/O, each DIMM, each GPU, each drive), and under it the readings grouped by
//! kind. Every reading keeps the four columns both Roch tools show: current, minimum, maximum and
//! average since the last reset.

use std::collections::VecDeque;

/// The kinds of device a heading can stand for, in the order the list shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceKind {
    System,
    Cpu,
    Board,
    Memory,
    Dimm,
    Gpu,
    Drive,
    Network,
    Errors,
}

impl DeviceKind {
    /// The tag the list prints ahead of a device's name.
    pub fn tag(self) -> &'static str {
        match self {
            DeviceKind::System => "SYSTEM",
            DeviceKind::Cpu => "CPU",
            DeviceKind::Board => "MOTHERBOARD",
            DeviceKind::Memory => "MEMORY",
            DeviceKind::Dimm => "DIMM",
            DeviceKind::Gpu => "GPU",
            DeviceKind::Drive => "DRIVE",
            DeviceKind::Network => "NETWORK",
            DeviceKind::Errors => "ERRORS",
        }
    }
}

/// The section a reading sits in under its device. Ordered as Roch GPU's telemetry table orders
/// its groups, with the board-level kinds (fan duty, throughput, errors) slotted in where they read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Temperatures,
    Voltages,
    Clocks,
    Load,
    Power,
    Current,
    Fans,
    FanDuty,
    Memory,
    Throughput,
    Data,
    Link,
    Limiters,
    Errors,
    Other,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Temperatures => "TEMPERATURES",
            Group::Voltages => "VOLTAGES",
            Group::Clocks => "CLOCKS",
            Group::Load => "LOAD",
            Group::Power => "POWER",
            Group::Current => "CURRENT",
            Group::Fans => "FANS",
            Group::FanDuty => "FAN DUTY",
            Group::Memory => "MEMORY",
            Group::Throughput => "THROUGHPUT",
            Group::Data => "DATA",
            Group::Link => "LINK",
            Group::Limiters => "LIMITERS",
            Group::Errors => "ERRORS",
            Group::Other => "OTHER",
        }
    }

    /// Where a reading goes when its source names only a unit, which is how Roch GPU files the
    /// supplemental sensors a card reports after the table was built.
    pub fn for_unit(unit: &str) -> Group {
        match unit {
            "°C" => Group::Temperatures,
            "V" | "mV" => Group::Voltages,
            "MHz" | "GHz" => Group::Clocks,
            "%" => Group::Load,
            "W" => Group::Power,
            "A" => Group::Current,
            "RPM" => Group::Fans,
            "MB" | "GB" => Group::Memory,
            "MB/s" | "GB/s" | "KB/s" => Group::Throughput,
            "gen" | "lanes" | "GT/s" => Group::Link,
            "state" => Group::Limiters,
            _ => Group::Other,
        }
    }
}

/// One heading in the list.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    /// Stable identifier, also the prefix of every sensor key under it ("cpu", "gpu0", ...).
    pub id: String,
    pub kind: DeviceKind,
    /// What the heading says: "Intel Core i9-14900KF", "MSI PRO Z790-P WIFI DDR4 (MS-7E06)".
    pub name: String,
    /// Where the readings come from, shown dimmed after the name: "Nuvoton NCT6687D".
    pub detail: String,
    /// Tie-break within a kind (GPU 0 before GPU 1).
    pub order: u32,
}

impl DeviceInfo {
    pub fn new(id: impl Into<String>, kind: DeviceKind, name: impl Into<String>) -> Self {
        Self { id: id.into(), kind, name: name.into(), detail: String::new(), order: 0 }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn order(mut self, order: u32) -> Self {
        self.order = order;
        self
    }
}

/// One reading's definition: where it sits and how it prints.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorInfo {
    /// Stable across runs, so hidden sensors and log columns survive a restart: "cpu/temp/coremax".
    pub key: String,
    pub device: String,
    pub group: Group,
    pub label: String,
    /// "°C", "V", "MHz", "RPM", "%", "W", "MB/s", ... or "state" for a Yes/No reading.
    pub unit: String,
    pub decimals: u8,
    /// The key of the row this one folds under (each logical processor under Core Effective Clock).
    pub parent: Option<String>,
    /// Where the number comes from, for the tooltip: "MCHBAR 0x5978", "NCT6687D 0x0124".
    pub source: String,
}

impl SensorInfo {
    pub fn new(
        device: &str,
        key: impl Into<String>,
        group: Group,
        label: impl Into<String>,
        unit: &str,
        decimals: u8,
    ) -> Self {
        let key = key.into();
        let key = if key.starts_with(&format!("{device}/")) { key } else { format!("{device}/{key}") };
        Self {
            key,
            device: device.to_owned(),
            group,
            label: label.into(),
            unit: unit.to_owned(),
            decimals,
            parent: None,
            source: String::new(),
        }
    }

    pub fn parent(mut self, parent_key: impl Into<String>) -> Self {
        let parent = parent_key.into();
        self.parent = Some(if parent.starts_with(&format!("{}/", self.device)) {
            parent
        } else {
            format!("{}/{parent}", self.device)
        });
        self
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }
}

/// Current, minimum, maximum and running mean for one sensor (Roch GPU's `SensorStat`, which is
/// Roch Viewer's `Statistic`).
///
/// The mean is a running total rather than a window, so it describes the whole session since the
/// last reset. A NaN reading is a sensor that did not answer this time: it clears `current` but is
/// not folded in, because a zero minimum on a temperature that was never read is a lie.
#[derive(Debug, Clone, Copy)]
pub struct SensorStat {
    pub current: f64,
    pub minimum: f64,
    pub maximum: f64,
    total: f64,
    count: u64,
}

impl Default for SensorStat {
    fn default() -> Self {
        Self { current: f64::NAN, minimum: f64::NAN, maximum: f64::NAN, total: 0.0, count: 0 }
    }
}

impl SensorStat {
    pub fn add(&mut self, value: f64) {
        if !value.is_finite() {
            self.current = f64::NAN;
            return;
        }
        self.current = value;
        if self.minimum.is_nan() || value < self.minimum {
            self.minimum = value;
        }
        if self.maximum.is_nan() || value > self.maximum {
            self.maximum = value;
        }
        self.total += value;
        self.count += 1;
    }

    /// Fold a reading into the extremes and the mean without making it the current value: the
    /// between-tick samples Roch Viewer takes of Core Max, which exist to catch peaks a once-a-second
    /// read misses.
    pub fn add_between(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        let current = self.current;
        self.add(value);
        if current.is_finite() {
            self.current = current;
        }
    }

    pub fn average(&self) -> f64 {
        if self.count > 0 { self.total / self.count as f64 } else { f64::NAN }
    }

    pub fn samples(&self) -> u64 {
        self.count
    }

    pub fn has_data(&self) -> bool {
        self.count > 0
    }

    /// Clear the extremes and the mean. The current reading stays, so the row does not blank for a
    /// tick after Reset.
    pub fn reset(&mut self) {
        let current = self.current;
        *self = Self::default();
        self.current = current;
    }
}

/// A fixed-capacity history for the graphs: (seconds since the engine started, value).
#[derive(Debug, Clone)]
pub struct History {
    capacity: usize,
    points: VecDeque<(f32, f32)>,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self { capacity: capacity.max(2), points: VecDeque::new() }
    }

    pub fn push(&mut self, t: f32, value: f64) {
        if self.points.len() == self.capacity {
            self.points.pop_front();
        }
        self.points.push_back((t, if value.is_finite() { value as f32 } else { f32::NAN }));
    }

    pub fn clear(&mut self) {
        self.points.clear();
    }

    pub fn points(&self) -> impl ExactSizeIterator<Item = &(f32, f32)> + DoubleEndedIterator {
        self.points.iter()
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// How a figure prints: Roch GPU's `format_reading`. An em dash for nothing read; a 0/1 "state"
/// as No/Yes with its average as the share of samples it was active; a PCIe generation as "Gen 5"
/// (the GPU backends' unit is "gen"); otherwise fixed decimals and the unit.
pub fn format_value(value: f64, unit: &str, decimals: u8) -> String {
    if value.is_nan() {
        return "—".into();
    }
    if unit == "state" {
        return if value == 0.0 {
            "No".into()
        } else if value == 1.0 {
            "Yes".into()
        } else {
            format!("{:.0} % active", value * 100.0)
        };
    }
    let decimals = decimals as usize;
    if unit == "gen" {
        return format!("Gen {value:.decimals$}");
    }
    if unit.is_empty() { format!("{value:.decimals$}") } else { format!("{value:.decimals$} {unit}") }
}

/// The figure alone, for CSV columns: no unit, a dot for the decimal, nothing for no reading.
pub fn format_plain(value: f64, decimals: u8) -> String {
    if value.is_finite() { format!("{value:.*}", decimals as usize) } else { String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missed_reading_clears_current_and_keeps_history() {
        let mut s = SensorStat::default();
        s.add(40.0);
        s.add(f64::NAN);
        assert!(s.current.is_nan());
        assert_eq!(s.minimum, 40.0);
        assert_eq!(s.maximum, 40.0);
        assert_eq!(s.average(), 40.0);
    }

    #[test]
    fn between_tick_samples_move_the_extremes_but_not_the_current_reading() {
        let mut s = SensorStat::default();
        s.add(40.0);
        s.add_between(61.0);
        s.add_between(38.0);
        assert_eq!(s.current, 40.0);
        assert_eq!(s.maximum, 61.0);
        assert_eq!(s.minimum, 38.0);
        assert_eq!(s.samples(), 3);
    }

    #[test]
    fn reset_keeps_the_current_reading() {
        let mut s = SensorStat::default();
        s.add(10.0);
        s.add(20.0);
        s.reset();
        assert_eq!(s.current, 20.0);
        assert!(s.minimum.is_nan());
        assert!(!s.has_data());
    }

    #[test]
    fn history_drops_the_oldest_point() {
        let mut h = History::new(3);
        for i in 0..5 {
            h.push(i as f32, i as f64);
        }
        let values: Vec<f32> = h.points().map(|p| p.1).collect();
        assert_eq!(values, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn readings_print_like_the_roch_tools() {
        assert_eq!(format_value(f64::NAN, "°C", 1), "—");
        assert_eq!(format_value(43.0, "°C", 1), "43.0 °C");
        assert_eq!(format_value(1.0, "state", 0), "Yes");
        assert_eq!(format_value(0.25, "state", 0), "25 % active");
        assert_eq!(format_value(3.0, "", 0), "3");
        assert_eq!(format_value(5.0, "gen", 0), "Gen 5");
        assert_eq!(format_value(f64::NAN, "gen", 0), "—");
        assert_eq!(format_plain(f64::NAN, 2), "");
        assert_eq!(format_plain(1.23456, 3), "1.235");
    }

    #[test]
    fn keys_are_prefixed_with_their_device_once() {
        let a = SensorInfo::new("cpu", "temp/coremax", Group::Temperatures, "Core Max", "°C", 1);
        assert_eq!(a.key, "cpu/temp/coremax");
        let b = SensorInfo::new("cpu", "cpu/temp/coremax", Group::Temperatures, "Core Max", "°C", 1).parent("clk/eff");
        assert_eq!(b.key, "cpu/temp/coremax");
        assert_eq!(b.parent.as_deref(), Some("cpu/clk/eff"));
    }
}
