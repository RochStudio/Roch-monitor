//! Graphics cards, through Roch GPU's own vendor backends: NVAPI and NVML for NVIDIA, ADL/ADLX for
//! AMD, IGCL for Intel Arc. The rows are Roch GPU's telemetry table (`telemetry_table.rs`): the same
//! sensors, the same names and units, and the same rule that a sensor the card never reports gets
//! no row rather than a column of dashes.
//!
//! Read-only. Only the backend's initialise, capability and telemetry reads are called; none of its
//! setters are reachable from here.

use std::panic::{AssertUnwindSafe, catch_unwind};

use roch_core::{GpuBackend, GpuCapabilities, GpuTelemetry, VoltageControlStyle};

use crate::engine::{Batch, Source};
use crate::model::{DeviceInfo, DeviceKind, Group, SensorInfo};

type Maker = fn() -> Box<dyn GpuBackend>;

#[cfg(windows)]
const VENDORS: [(&str, Maker); 3] = [
    ("NVIDIA", || Box::new(roch_nvidia::NvidiaBackend::new())),
    ("AMD", || Box::new(roch_amd::AmdBackend::new())),
    ("Intel", || Box::new(roch_intel::IntelBackend::new())),
];

#[cfg(not(windows))]
const VENDORS: [(&str, Maker); 0] = [];

struct Card {
    backend: usize,
    index: usize,
    device: DeviceInfo,
    caps: GpuCapabilities,
    nvidia: bool,
    /// The numbered clock domains seen running (at least [`DOMAIN_RUNNING_MHZ`]) at least once. One
    /// that never has gets no row: the walk on a 5070 Ti carries type 31, which reads a flat 5 kHz.
    ticking: std::collections::BTreeSet<i32>,
}

pub struct Gpus {
    backends: Vec<Box<dyn GpuBackend>>,
    cards: Vec<Card>,
    status: String,
}

impl Gpus {
    pub fn detect(out: &mut Batch) -> Option<Gpus> {
        let mut backends: Vec<Box<dyn GpuBackend>> = Vec::new();
        let mut cards = Vec::new();
        let mut tried = Vec::new();
        for (vendor, make) in VENDORS {
            // A vendor library that panics must not stop the next one being tried.
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                let mut b = make();
                let r = b.initialize();
                (b, r)
            }));
            match outcome {
                Ok((mut backend, Ok(()))) if !backend.devices().is_empty() => {
                    let slot = backends.len();
                    let devices = backend.devices().to_vec();
                    for d in devices {
                        let caps = backend.capabilities(d.index).unwrap_or_default();
                        let number = cards.len();
                        let mut detail = Vec::new();
                        if !d.driver_version.is_empty() && d.driver_version != "unknown" {
                            detail.push(format!("driver {}", d.driver_version));
                        }
                        if d.vram_megabytes > 0 {
                            detail.push(format!("{} GB", (d.vram_megabytes as f64 / 1024.0).round()));
                        }
                        if !d.bus_id.is_empty() {
                            detail.push(d.bus_id.clone());
                        }
                        cards.push(Card {
                            backend: slot,
                            index: d.index,
                            device: DeviceInfo::new(format!("gpu{number}"), DeviceKind::Gpu, d.name.clone())
                                .detail(format!("{} · {}", backend.backend_name(), detail.join(" · ")))
                                .order(number as u32),
                            caps,
                            nvidia: vendor == "NVIDIA",
                            ticking: Default::default(),
                        });
                    }
                    backends.push(backend);
                }
                Ok((backend, Ok(()))) => tried.push(format!("{}: no GPUs", backend.backend_name())),
                Ok((backend, Err(e))) => tried.push(format!("{}: {}", backend.backend_name(), e.message)),
                Err(_) => tried.push(format!("{vendor}: the backend panicked")),
            }
        }
        if cards.is_empty() {
            out.status(format!("Graphics: no supported GPU. Tried {}.", tried.join("; ")));
            return None;
        }
        let names: Vec<String> = cards.iter().map(|c| c.device.name.clone()).collect();
        let status = format!("Graphics: {} via Roch GPU's backends.", names.join(", "));
        Some(Gpus { backends, cards, status })
    }
}

/// Below this a numbered clock domain is not running: every real one on a 5070 Ti is 100 MHz or more,
/// and type 31 reads 0.005.
const DOMAIN_RUNNING_MHZ: f64 = 1.0;

fn mhz(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 { v } else { f64::NAN }
}

fn positive(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 { v } else { f64::NAN }
}

/// Decimals Roch GPU gives a supplemental sensor by its unit.
fn decimals_for(unit: &str) -> u8 {
    match unit {
        "°C" | "W" | "%" | "GT/s" => 1,
        "V" => 3,
        "A" => 2,
        _ => 0,
    }
}

fn put_card(out: &mut Batch, card: &mut Card, t: &GpuTelemetry, extra: &std::collections::BTreeMap<String, f64>) {
    let dev = card.device.id.as_str();
    let s = |key: &str, group: Group, label: &str, unit: &str, decimals: u8| {
        SensorInfo::new(dev, key, group, label, unit, decimals)
    };

    out.put(
        &s("temp", Group::Temperatures, "GPU Temperature", "°C", 1),
        if t.temperature_c > 0.0 { t.temperature_c } else { f64::NAN },
    );
    // NaN is "not exposed"; the model's default of 0.0 is "never set". Neither is a reading.
    if t.hot_spot_c.is_finite() && t.hot_spot_c > 0.0 {
        out.put(&s("hotspot", Group::Temperatures, "GPU Hot Spot", "°C", 1), t.hot_spot_c);
    }
    if t.memory_temperature_c.is_finite() && t.memory_temperature_c > 0.0 {
        out.put(&s("memtemp", Group::Temperatures, "GPU Memory Junction", "°C", 1), t.memory_temperature_c);
    }

    if t.voltage_mv.is_finite() && t.voltage_mv > 0.0 {
        out.put(&s("volt", Group::Voltages, "GPU Core (VID)", "mV", 0), t.voltage_mv);
    }
    if t.nvvdd_mv.is_finite() {
        out.put(&s("nvvdd", Group::Voltages, "NVVDD (measured)", "mV", 1), t.nvvdd_mv);
    }
    if t.msvdd_mv.is_finite() {
        out.put(&s("msvdd", Group::Voltages, "MSVDD (measured)", "mV", 1), t.msvdd_mv);
    }

    out.put(&s("clk/core", Group::Clocks, "GPU Core", "MHz", 0), mhz(t.core_clock_mhz));
    if let Some(v) = extra.get("coremeasured")
        && !card.caps.power_limit_is_offset
        && card.caps.voltage_style != VoltageControlStyle::Percent
    {
        out.put(&s("clk/coremeasured", Group::Clocks, "GPU Core (measured)", "MHz", 0), *v);
    }
    if t.memory_speed_mbps.is_finite() {
        out.put(&s("clk/memspeed", Group::Clocks, "GPU Memory", "Mbps", 0), t.memory_speed_mbps);
    } else {
        out.put(&s("clk/mem", Group::Clocks, "GPU Memory", "MHz", 0), mhz(t.memory_clock_mhz));
    }
    if t.fabric_clock_mhz.is_finite() {
        out.put(&s("clk/fclk", Group::Clocks, "Fabric (FCLK)", "MHz", 0), t.fabric_clock_mhz);
    }
    if t.soc_clock_mhz.is_finite() {
        out.put(&s("clk/soc", Group::Clocks, "SoC", "MHz", 0), t.soc_clock_mhz);
    }
    for (key, label) in [("xbar", "Crossbar"), ("sys", "SYS"), ("video", "Video")] {
        if let Some(v) = extra.get(key) {
            out.put(&s(&format!("clk/{key}"), Group::Clocks, label, "MHz", 0), *v);
        }
    }
    // The domains NVIDIA only numbers, in ascending type, named where the name has been checked.
    let mut domains: Vec<(i32, f64)> = extra
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("domain").and_then(|n| n.parse().ok()).map(|n| (n, *v)))
        .collect();
    domains.sort_by_key(|d| d.0);
    for (n, v) in domains {
        if v.is_finite() && v >= DOMAIN_RUNNING_MHZ {
            card.ticking.insert(n);
        }
        if !card.ticking.contains(&n) {
            continue;
        }
        #[cfg(windows)]
        let label = roch_nvidia::domain_name(n);
        #[cfg(not(windows))]
        let label = format!("Domain type {n}");
        out.put(&s(&format!("clk/domain{n}"), Group::Clocks, &label, "MHz", 0), v);
    }

    out.put(&s("load/core", Group::Load, "GPU Core Load", "%", 0), t.gpu_load_percent);
    out.put(&s("load/mem", Group::Load, "Memory Controller Load", "%", 0), t.memory_load_percent);

    out.put(&s("power/board", Group::Power, "GPU Board Power", "W", 1), positive(t.power_watts));
    if !card.caps.power_limit_is_offset || t.power_percent.is_finite() {
        out.put(&s("power/tdp", Group::Power, "Total, % of TDP", "%", 1), t.power_percent);
    }

    let n = t.rail_amps.len();
    for (i, a) in t.rail_amps.iter().enumerate() {
        #[cfg(windows)]
        let label = roch_nvidia::power_rail_name(i as i32, n as i32);
        #[cfg(not(windows))]
        let label = format!("Rail {i}");
        out.put(&s(&format!("rail{i}"), Group::Current, &label, "A", 2), *a);
    }

    let fans = t.fan_rpms.len().max(t.fan_percents.len());
    if fans == 0 {
        out.put(&s("fan0", Group::Fans, "GPU Fan", "RPM", 0), t.fan_rpm);
        out.put(&s("duty0", Group::FanDuty, "GPU Fan", "%", 0), t.fan_percent);
    } else {
        for i in 0..fans {
            let label = if fans == 1 { "GPU Fan".to_owned() } else { format!("GPU Fan {}", i + 1) };
            if let Some(rpm) = t.fan_rpms.get(i) {
                out.put(&s(&format!("fan{i}"), Group::Fans, &label, "RPM", 0), *rpm);
            }
            if let Some(pct) = t.fan_percents.get(i) {
                out.put(&s(&format!("duty{i}"), Group::FanDuty, &label, "%", 0), *pct);
            }
        }
    }

    let used_label = if card.caps.power_limit_is_offset { "Dedicated VRAM Used" } else { "VRAM Allocated" };
    out.put(&s("vram/used", Group::Memory, used_label, "MB", 0), t.memory_used_mb);

    for x in &t.supplemental_sensors {
        let key = format!("x/{}", x.key.replace([':', ' '], "-"));
        out.put(&s(&key, Group::for_unit(&x.unit), &x.name, &x.unit, decimals_for(&x.unit)), x.value);
    }
    let _ = card.nvidia;
}

impl Source for Gpus {
    fn name(&self) -> &str {
        "Graphics"
    }

    fn poll(&mut self, out: &mut Batch) {
        out.status(self.status.clone());
        for card in &mut self.cards {
            out.device(card.device.clone());
            let backend = &mut self.backends[card.backend];
            match backend.read_telemetry(card.index) {
                Ok(t) => {
                    let extra = if card.nvidia { backend.measure_extra_clocks(card.index) } else { Default::default() };
                    put_card(out, card, &t, &extra);
                }
                Err(e) => out.status(format!("{} ({}: {})", self.status, card.device.name, e.message)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_reports_only_what_it_has() {
        let mut card = Card {
            backend: 0,
            index: 0,
            device: DeviceInfo::new("gpu0", DeviceKind::Gpu, "Test"),
            caps: GpuCapabilities::default(),
            nvidia: false,
            ticking: Default::default(),
        };
        let t = GpuTelemetry {
            temperature_c: 40.0,
            core_clock_mhz: 1500.0,
            memory_clock_mhz: 7000.0,
            power_watts: 0.0,
            fan_rpms: vec![1000.0, 1100.0],
            fan_percents: vec![30.0, 31.0],
            ..GpuTelemetry::default()
        };
        let mut b = Batch::default();
        put_card(&mut b, &mut card, &t, &Default::default());
        let keys: Vec<&str> = b.sensors.iter().map(|s| s.key.as_str()).collect();
        assert!(keys.contains(&"gpu0/temp"));
        assert!(!keys.contains(&"gpu0/hotspot"), "a hot spot never set is not a reading: no row");
        assert!(!keys.contains(&"gpu0/volt"));
        assert!(keys.contains(&"gpu0/fan1"));
        let board = b.values.iter().find(|(k, _)| k == "gpu0/power/board").unwrap().1;
        assert!(board.is_nan(), "0 W is the backend's 'unavailable'");
    }

    #[test]
    fn a_clock_domain_that_has_only_read_zero_gets_no_row() {
        let mut card = Card {
            backend: 0,
            index: 0,
            device: DeviceInfo::new("gpu0", DeviceKind::Gpu, "Test"),
            caps: GpuCapabilities::default(),
            nvidia: true,
            ticking: Default::default(),
        };
        let t = GpuTelemetry::default();
        let poll = |card: &mut Card, d3: f64, d31: f64| {
            let extra = [("domain3".to_owned(), d3), ("domain31".to_owned(), d31)].into_iter().collect();
            let mut b = Batch::default();
            put_card(&mut b, card, &t, &extra);
            b.values
        };

        for d31 in [0.0, 0.005] {
            let first = poll(&mut card, 540.0, d31);
            assert!(first.iter().any(|(k, v)| k == "gpu0/clk/domain3" && *v == 540.0));
            assert!(!first.iter().any(|(k, _)| k == "gpu0/clk/domain31"), "{d31} MHz is not a running clock");
        }

        // Once it has run, it keeps its row, and a later zero is a reading.
        poll(&mut card, 540.0, 120.0);
        let later = poll(&mut card, 540.0, 0.0);
        assert!(later.iter().any(|(k, v)| k == "gpu0/clk/domain31" && *v == 0.0));
    }
}
