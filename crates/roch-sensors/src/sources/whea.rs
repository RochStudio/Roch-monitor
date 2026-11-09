//! How many hardware errors Windows has logged since this boot (Roch Viewer's `whea_errors`).
//!
//! WHEA is how firmware and the CPU tell Windows something went wrong in hardware: a corrected
//! memory error, a PCIe error, a machine check. A kit that boots and benchmarks can still be quietly
//! correcting errors, and this count is where that shows. Counted since boot, because the question
//! is about the configuration in front of you, not three BIOS versions ago.
//!
//! The query costs tens of milliseconds and errors are rare, so it is re-run every few seconds
//! rather than every tick. A log that cannot be read reports nothing rather than zero: zero is a
//! claim the machine is clean.

use std::time::{Duration, Instant};

use crate::engine::{Batch, Source};
use crate::model::{DeviceInfo, DeviceKind, Group, SensorInfo};

const DEV: &str = "whea";
const CACHE: Duration = Duration::from_secs(5);

pub struct Whea {
    last: Option<(Instant, Option<u64>)>,
}

impl Whea {
    pub fn new() -> Whea {
        Whea { last: None }
    }
}

impl Default for Whea {
    fn default() -> Self {
        Self::new()
    }
}

/// Milliseconds since boot, for the query's TimeCreated window.
#[cfg(windows)]
fn uptime_ms() -> u64 {
    // SAFETY: no arguments.
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
}

pub fn query_since_boot(uptime_ms: u64) -> String {
    format!(
        "*[System[Provider[@Name='Microsoft-Windows-WHEA-Logger'] and TimeCreated[timediff(@SystemTime) <= {uptime_ms}]]]"
    )
}

#[cfg(windows)]
pub fn count_since_boot() -> Option<u64> {
    use windows_sys::Win32::System::EventLog::{
        EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection,
    };
    let channel: Vec<u16> = "System\0".encode_utf16().collect();
    let query: Vec<u16> = query_since_boot(uptime_ms()).encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wevtapi calls with owned buffers; every handle returned is closed.
    unsafe {
        let results = EvtQuery(0, channel.as_ptr(), query.as_ptr(), EvtQueryChannelPath | EvtQueryReverseDirection);
        if results == 0 {
            return None;
        }
        let mut total = 0u64;
        let mut events = [0isize; 64];
        loop {
            let mut returned = 0u32;
            if EvtNext(results, events.len() as u32, events.as_mut_ptr(), 1000, 0, &mut returned) == 0 {
                // ERROR_NO_MORE_ITEMS ends the walk; any other failure leaves a partial count,
                // which would understate the errors, so it reports nothing.
                let err = windows_sys::Win32::Foundation::GetLastError();
                EvtClose(results);
                return (err == windows_sys::Win32::Foundation::ERROR_NO_MORE_ITEMS).then_some(total);
            }
            for &e in &events[..returned as usize] {
                EvtClose(e);
            }
            total += returned as u64;
        }
    }
}

#[cfg(not(windows))]
pub fn count_since_boot() -> Option<u64> {
    None
}

impl Source for Whea {
    fn name(&self) -> &str {
        "WHEA"
    }

    fn poll(&mut self, out: &mut Batch) {
        out.device(
            DeviceInfo::new(DEV, DeviceKind::Errors, "Windows Hardware Errors (WHEA)")
                .detail("System event log, since boot"),
        );
        let fresh = self.last.is_none_or(|(at, _)| at.elapsed() >= CACHE);
        if fresh {
            self.last = Some((Instant::now(), count_since_boot()));
        }
        let count = self.last.and_then(|(_, c)| c);
        out.status(match count {
            Some(n) => format!("WHEA: {n} hardware error(s) logged since boot."),
            None => "WHEA: the System event log could not be read.".into(),
        });
        out.put(
            &SensorInfo::new(DEV, "total", Group::Errors, "Total Errors", "", 0)
                .source("Microsoft-Windows-WHEA-Logger events in the System log since boot"),
            count.map_or(f64::NAN, |n| n as f64),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_selects_whea_since_boot() {
        let q = query_since_boot(1234);
        assert!(q.contains("Microsoft-Windows-WHEA-Logger"));
        assert!(q.contains("<= 1234"));
    }

    #[cfg(windows)]
    #[test]
    fn the_log_answers() {
        assert!(count_since_boot().is_some());
    }
}
