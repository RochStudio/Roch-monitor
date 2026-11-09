//! Network adapters: current download and upload rate, and what has moved since the monitor
//! started, from the interface counters (GetIfTable2). Physical adapters that are up, only:
//! the loopback, the virtual switches and the filter drivers stacked on a real adapter are not
//! separate networks.

use std::collections::HashMap;
use std::time::Instant;

use crate::engine::{Batch, Source};
use crate::model::{DeviceInfo, DeviceKind, Group, SensorInfo};

#[derive(Debug, Clone, PartialEq)]
pub struct Interface {
    pub luid: u64,
    pub alias: String,
    pub description: String,
    pub in_octets: u64,
    pub out_octets: u64,
    pub link_mbps: f64,
}

#[cfg(windows)]
pub fn interfaces() -> Vec<Interface> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    // SAFETY: GetIfTable2 allocates the table; it is read within NumEntries and freed.
    unsafe {
        if GetIfTable2(&mut table) != 0 || table.is_null() {
            return Vec::new();
        }
        let n = (*table).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), n);
        let wide = |w: &[u16]| {
            let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
            String::from_utf16_lossy(&w[..end])
        };
        let mut out = Vec::new();
        for r in rows {
            let flags = r.InterfaceAndOperStatusFlags._bitfield;
            let hardware = flags & 0x01 != 0;
            let filter = flags & 0x02 != 0;
            // IF_TYPE_SOFTWARE_LOOPBACK = 24; IfOperStatusUp = 1.
            if !hardware || filter || r.Type == 24 || r.OperStatus != 1 {
                continue;
            }
            out.push(Interface {
                luid: r.InterfaceLuid.Value,
                alias: wide(&r.Alias),
                description: wide(&r.Description),
                in_octets: r.InOctets,
                out_octets: r.OutOctets,
                link_mbps: r.ReceiveLinkSpeed as f64 / 1_000_000.0,
            });
        }
        FreeMibTable(table as *const _);
        out
    }
}

#[cfg(not(windows))]
pub fn interfaces() -> Vec<Interface> {
    Vec::new()
}

struct Seen {
    first: (u64, u64),
    last: (u64, u64, Instant),
}

pub struct Network {
    seen: HashMap<u64, Seen>,
}

impl Network {
    pub fn new() -> Network {
        let now = Instant::now();
        let seen = interfaces()
            .into_iter()
            .map(|i| (i.luid, Seen { first: (i.in_octets, i.out_octets), last: (i.in_octets, i.out_octets, now) }))
            .collect();
        Network { seen }
    }
}

impl Default for Network {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for Network {
    fn name(&self) -> &str {
        "Network"
    }

    fn poll(&mut self, out: &mut Batch) {
        let now = Instant::now();
        let list = interfaces();
        out.status(format!("Network: {} adapter(s) up, from the interface counters.", list.len()));
        for (n, i) in list.iter().enumerate() {
            let dev = format!("net-{:x}", i.luid);
            let detail = if i.link_mbps >= 1000.0 {
                format!("{} · {:.1} Gbps", i.description, i.link_mbps / 1000.0)
            } else {
                format!("{} · {:.0} Mbps", i.description, i.link_mbps)
            };
            out.device(DeviceInfo::new(&dev, DeviceKind::Network, i.alias.clone()).detail(detail).order(n as u32));
            let seen = self
                .seen
                .entry(i.luid)
                .or_insert(Seen { first: (i.in_octets, i.out_octets), last: (i.in_octets, i.out_octets, now) });
            let secs = now.duration_since(seen.last.2).as_secs_f64();
            let (down, up) = if secs > 0.05 {
                (
                    i.in_octets.saturating_sub(seen.last.0) as f64 / 1024.0 / secs,
                    i.out_octets.saturating_sub(seen.last.1) as f64 / 1024.0 / secs,
                )
            } else {
                (f64::NAN, f64::NAN)
            };
            seen.last = (i.in_octets, i.out_octets, now);
            const MB: f64 = 1024.0 * 1024.0;
            let total_down = i.in_octets.saturating_sub(seen.first.0) as f64 / MB;
            let total_up = i.out_octets.saturating_sub(seen.first.1) as f64 / MB;
            out.put(&SensorInfo::new(&dev, "rate/down", Group::Throughput, "Download Rate", "KB/s", 1), down);
            out.put(&SensorInfo::new(&dev, "rate/up", Group::Throughput, "Upload Rate", "KB/s", 1), up);
            out.put(&SensorInfo::new(&dev, "total/down", Group::Data, "Downloaded (session)", "MB", 1), total_down);
            out.put(&SensorInfo::new(&dev, "total/up", Group::Data, "Uploaded (session)", "MB", 1), total_up);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn interfaces_are_listed_without_loopback() {
        for i in super::interfaces() {
            assert!(!i.alias.to_lowercase().contains("loopback"));
        }
    }
}
