//! Core clocks and usage from the Windows performance counters (Roch Viewer's `cpu_clocks`).
//!
//! The P-state ratio HWiNFO shows lives in an MSR inpoutx64 cannot read, and the two other routes
//! Roch Viewer measured were useless (CallNtPowerInformation reports the nominal clock for every
//! core) or slow (WMI, 452 ms a query). These are averages over the interval rather than an
//! instantaneous ratio, which is why they read a little under HWiNFO, and the rows are named for
//! what they are. With PawnIO installed the `msr` source reads the real ratios and APERF, and
//! these clock rows stand aside; the usage rows stay, since only the counters have them.
//!
//!   Core Clock (avg)       nominal × "% Processor Performance": what the busy cores ran at.
//!   Core Effective Clock   nominal × "% Processor Utility": the same scale with idle counted,
//!                          the quantity HWiNFO calls the effective clock.
//!   CPU Usage              "% Processor Time", HWiNFO's Total CPU Usage.
//!
//! Each has one child row per logical processor, folded under it.

use crate::engine::{Batch, Source};
use crate::model::{DeviceInfo, Group, SensorInfo};
use crate::sources::intel_mchbar::cpu_device;
use crate::system::SystemInfo;
use crate::win::pdh::{Query, instance_order};

const PERFORMANCE: usize = 0;
const UTILITY: usize = 1;
const FREQUENCY: usize = 2;
const TIME: usize = 3;

/// A performance ratio past this is a counter that wrapped, not a clock.
const MAX_PERCENT: f64 = 1000.0;
const CPU: &str = "cpu";

pub struct CpuLoad {
    query: Query,
    device: DeviceInfo,
    sys: SystemInfo,
    /// Whether to show the clock rows: not when the per-core MSRs are being read instead.
    clocks: bool,
}

impl CpuLoad {
    pub fn detect(sys: &SystemInfo, out: &mut Batch, clocks: bool) -> Option<CpuLoad> {
        // "Processor Information" rather than "Processor": the older object does not break out
        // processor groups and reports nothing useful past 64 threads.
        let Some(mut query) = Query::open(&[
            r"\Processor Information(*)\% Processor Performance",
            r"\Processor Information(*)\% Processor Utility",
            r"\Processor Information(_Total)\Processor Frequency",
            r"\Processor Information(*)\% Processor Time",
        ]) else {
            out.status("CPU clocks and usage: the Processor Information performance counters did not open.");
            return None;
        };
        // The first collection is only a baseline for the rate counters.
        query.collect();
        Some(CpuLoad { query, device: cpu_device(sys), sys: sys.clone(), clocks })
    }

    fn label(&self, instance: &str, position: usize) -> String {
        let (group, number) = instance_order(instance);
        if group != u32::MAX
            && number != u32::MAX
            && let Some(l) = self.sys.topology.label(group as u16, number as u8)
        {
            return l;
        }
        format!("CPU {position}")
    }
}

/// The per-processor entries of one counter, in processor order, aggregates dropped.
fn per_thread(rows: Vec<(String, f64)>) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = rows.into_iter().filter(|(n, _)| !n.ends_with("_Total")).collect();
    v.sort_by_key(|(n, _)| instance_order(n));
    v
}

fn total(rows: &[(String, f64)]) -> Option<f64> {
    rows.iter().find(|(n, _)| n == "_Total").map(|(_, v)| *v)
}

impl Source for CpuLoad {
    fn name(&self) -> &str {
        "CPU counters"
    }

    fn poll(&mut self, out: &mut Batch) {
        out.device(self.device.clone());
        if !self.query.collect() {
            // The baseline, or a tick too soon after the last: no reading rather than a wrong one.
            return;
        }
        let time = self.query.array(TIME);
        let nominal = self.query.instance(FREQUENCY, "_Total").filter(|&f| f > 0.0 && f < 100_000.0);
        out.status(if self.clocks {
            format!(
                "Performance counters: {} logical processors, nominal {}.",
                per_thread(time.clone()).len(),
                nominal.map_or("unknown".into(), |f| format!("{f:.0} MHz"))
            )
        } else {
            format!(
                "Performance counters: usage of {} logical processors; clocks from the MSRs instead.",
                per_thread(time.clone()).len()
            )
        });

        if self.clocks {
            let performance = self.query.array(PERFORMANCE);
            let utility = self.query.array(UTILITY);
            let clock = |pct: f64| -> f64 {
                match nominal {
                    Some(n) if (0.0..=MAX_PERCENT).contains(&pct) => n * pct / 100.0,
                    _ => f64::NAN,
                }
            };

            let avg = SensorInfo::new(CPU, "clk/avg", Group::Clocks, "Core Clock (avg)", "MHz", 0)
                .source("nominal × \\Processor Information(_Total)\\% Processor Performance");
            out.put(&avg, total(&performance).map_or(f64::NAN, clock));
            let threads = per_thread(performance);
            let max = threads.iter().map(|(_, v)| clock(*v)).filter(|v| v.is_finite()).fold(f64::NAN, f64::max);
            for (i, (name, pct)) in threads.iter().enumerate() {
                let info = SensorInfo::new(
                    CPU,
                    format!("clk/thread{i}"),
                    Group::Clocks,
                    format!("{} Clock", self.label(name, i)),
                    "MHz",
                    0,
                )
                .parent(avg.key.clone())
                .source(format!("nominal × \\Processor Information({name})\\% Processor Performance"));
                out.put(&info, clock(*pct));
            }
            out.put(
                &SensorInfo::new(CPU, "clk/max", Group::Clocks, "Core Clock (max)", "MHz", 0)
                    .source("the fastest logical processor over the interval"),
                max,
            );

            let eff = SensorInfo::new(CPU, "clk/effective", Group::Clocks, "Core Effective Clock", "MHz", 0)
                .source("nominal × \\Processor Information(_Total)\\% Processor Utility (idle counted)");
            out.put(&eff, total(&utility).map_or(f64::NAN, clock));
            for (i, (name, pct)) in per_thread(utility).iter().enumerate() {
                let info = SensorInfo::new(
                    CPU,
                    format!("clk/eff{i}"),
                    Group::Clocks,
                    format!("{} Effective Clock", self.label(name, i)),
                    "MHz",
                    0,
                )
                .parent(eff.key.clone())
                .source(format!("nominal × \\Processor Information({name})\\% Processor Utility"));
                out.put(&info, clock(*pct));
            }
        }

        let usage = SensorInfo::new(CPU, "load/total", Group::Load, "Total CPU Usage", "%", 1)
            .source("\\Processor Information(_Total)\\% Processor Time");
        out.put(&usage, total(&time).map_or(f64::NAN, |v| v.clamp(0.0, 100.0)));
        let per = per_thread(time);
        let busiest = per.iter().map(|(_, v)| v.clamp(0.0, 100.0)).fold(f64::NAN, f64::max);
        for (i, (name, pct)) in per.iter().enumerate() {
            let info = SensorInfo::new(
                CPU,
                format!("load/thread{i}"),
                Group::Load,
                format!("{} Usage", self.label(name, i)),
                "%",
                1,
            )
            .parent(usage.key.clone())
            .source(format!("\\Processor Information({name})\\% Processor Time"));
            out.put(&info, pct.clamp(0.0, 100.0));
        }
        out.put(&SensorInfo::new(CPU, "load/max", Group::Load, "Max Thread Usage", "%", 1), busiest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_are_dropped_and_threads_sorted() {
        let rows = vec![
            ("0,10".to_string(), 1.0),
            ("_Total".to_string(), 5.0),
            ("0,2".to_string(), 2.0),
            ("0,_Total".to_string(), 3.0),
        ];
        assert_eq!(total(&rows), Some(5.0));
        let t = per_thread(rows);
        assert_eq!(t.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["0,2", "0,10"]);
    }
}
