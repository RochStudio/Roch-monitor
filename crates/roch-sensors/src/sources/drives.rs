//! Drives: temperature from the storage stack, activity and throughput from the performance
//! counters. HWiNFO and HWMonitor both show these; neither needs the driver here.
//!
//! The temperature is what the device reports through IOCTL_STORAGE_QUERY_PROPERTY
//! (StorageDeviceTemperatureProperty), which the in-box NVMe and ATA miniports answer from the
//! drive's own health data. A drive that does not answer gets no temperature row.

use std::collections::HashMap;

use crate::engine::{Batch, Source};
use crate::model::{DeviceInfo, DeviceKind, Group, SensorInfo};
use crate::win::pdh::Query;

const IDLE: usize = 0;
const READ: usize = 1;
const WRITE: usize = 2;

#[derive(Debug, Clone)]
struct Drive {
    number: u32,
    device: DeviceInfo,
    sensors: usize,
}

pub struct Drives {
    drives: Vec<Drive>,
    query: Option<Query>,
}

/// What the storage stack says a drive is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Identity {
    pub product: String,
    pub serial: String,
    pub bus: &'static str,
    pub removable: bool,
}

pub fn bus_name(bus: i32) -> &'static str {
    match bus {
        1 => "SCSI",
        3 => "ATA",
        7 => "USB",
        8 => "RAID",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        14 => "Virtual",
        15 => "Storage Spaces",
        17 => "NVMe",
        _ => "",
    }
}

#[cfg(windows)]
mod imp {
    use super::Identity;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
        STORAGE_TEMPERATURE_DATA_DESCRIPTOR, STORAGE_TEMPERATURE_INFO, StorageDeviceProperty,
        StorageDeviceTemperatureProperty,
    };

    pub struct Handle(pub HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: our own handle.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Open a drive for property queries only: no read or write access is asked for.
    pub fn open(number: u32) -> Option<Handle> {
        let path: Vec<u16> = format!("\\\\.\\PhysicalDrive{number}").encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: a plain CreateFileW with zero access rights.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        (h != INVALID_HANDLE_VALUE && !h.is_null()).then_some(Handle(h))
    }

    fn query(h: &Handle, property: i32, out: &mut [u8]) -> Option<usize> {
        let q = STORAGE_PROPERTY_QUERY {
            PropertyId: property,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut got = 0u32;
        // SAFETY: input and output buffers are ours and sized as passed.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                IOCTL_STORAGE_QUERY_PROPERTY,
                (&q as *const STORAGE_PROPERTY_QUERY).cast(),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                out.as_mut_ptr().cast(),
                out.len() as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(got as usize)
    }

    pub fn identity(h: &Handle) -> Option<Identity> {
        let mut buf = vec![0u8; 1024];
        let got = query(h, StorageDeviceProperty, &mut buf)?;
        if got < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
            return None;
        }
        // SAFETY: the buffer holds at least one descriptor; it is read unaligned.
        let d: STORAGE_DEVICE_DESCRIPTOR = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
        let text = |offset: u32| -> String {
            let o = offset as usize;
            if o == 0 || o >= got {
                return String::new();
            }
            let end = buf[o..got].iter().position(|&b| b == 0).map_or(got, |p| o + p);
            String::from_utf8_lossy(&buf[o..end]).trim().to_owned()
        };
        let vendor = text(d.VendorIdOffset);
        let product = text(d.ProductIdOffset);
        let product =
            if vendor.is_empty() || product.starts_with(&vendor) { product } else { format!("{vendor} {product}") };
        Some(Identity {
            product,
            serial: text(d.SerialNumberOffset),
            bus: super::bus_name(d.BusType),
            removable: d.RemovableMedia != 0,
        })
    }

    /// Every temperature the drive reports, composite first. Empty when it reports none.
    pub fn temperatures(h: &Handle) -> Vec<f64> {
        let mut buf = vec![0u64; 64];
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
        let Some(got) = query(h, StorageDeviceTemperatureProperty, bytes) else { return Vec::new() };
        let head = std::mem::size_of::<STORAGE_TEMPERATURE_DATA_DESCRIPTOR>()
            - std::mem::size_of::<STORAGE_TEMPERATURE_INFO>();
        if got < head {
            return Vec::new();
        }
        // SAFETY: the buffer is 8-byte aligned and at least one descriptor long.
        let d = unsafe { &*(buf.as_ptr() as *const STORAGE_TEMPERATURE_DATA_DESCRIPTOR) };
        let first = d.TemperatureInfo.as_ptr();
        let count = (d.InfoCount as usize).min((got - head) / std::mem::size_of::<STORAGE_TEMPERATURE_INFO>());
        (0..count)
            // SAFETY: bounded by the byte count the driver returned.
            .map(|i| unsafe { (*first.add(i)).Temperature } as f64)
            .collect()
    }
}

/// "0 C:" -> (0, "C:"); "1 D: E:" -> (1, "D: E:").
pub fn parse_disk_instance(name: &str) -> Option<(u32, String)> {
    let mut parts = name.splitn(2, ' ');
    let number = parts.next()?.parse().ok()?;
    Some((number, parts.next().unwrap_or("").trim().to_owned()))
}

impl Drives {
    pub fn detect(out: &mut Batch) -> Option<Drives> {
        let mut drives = Vec::new();
        #[cfg(windows)]
        for number in 0..32u32 {
            let Some(h) = imp::open(number) else { continue };
            let Some(id) = imp::identity(&h) else { continue };
            if id.bus == "Virtual" || id.bus == "Storage Spaces" {
                continue;
            }
            let name = if id.product.is_empty() { format!("Drive {number}") } else { id.product.clone() };
            let detail = [id.bus.to_owned(), format!("disk {number}")]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            drives.push(Drive {
                number,
                device: DeviceInfo::new(format!("drive{number}"), DeviceKind::Drive, name).detail(detail).order(number),
                sensors: imp::temperatures(&h).len(),
            });
        }
        if drives.is_empty() {
            out.status("Drives: none could be opened for a property query.");
            return None;
        }
        let mut query = Query::open(&[
            r"\PhysicalDisk(*)\% Idle Time",
            r"\PhysicalDisk(*)\Disk Read Bytes/sec",
            r"\PhysicalDisk(*)\Disk Write Bytes/sec",
        ]);
        if let Some(q) = query.as_mut() {
            q.collect();
        }
        Some(Drives { drives, query })
    }
}

impl Source for Drives {
    fn name(&self) -> &str {
        "Drives"
    }

    fn poll(&mut self, out: &mut Batch) {
        let mut letters: HashMap<u32, String> = HashMap::new();
        let mut counters: HashMap<u32, [f64; 3]> = HashMap::new();
        if let Some(q) = self.query.as_mut()
            && q.collect()
        {
            for (index, slot) in [(IDLE, 0usize), (READ, 1), (WRITE, 2)] {
                for (name, value) in q.array(index) {
                    if let Some((n, l)) = parse_disk_instance(&name) {
                        letters.insert(n, l);
                        counters.entry(n).or_insert([f64::NAN; 3])[slot] = value;
                    }
                }
            }
        }
        out.status(format!(
            "Drives: {} found; temperature from the storage stack, activity from the PhysicalDisk counters.",
            self.drives.len()
        ));
        for d in &mut self.drives {
            let mut device = d.device.clone();
            if let Some(l) = letters.get(&d.number).filter(|l| !l.is_empty()) {
                device.detail = format!("{} · {l}", device.detail);
            }
            let dev = device.id.clone();
            out.device(device);
            #[cfg(windows)]
            if d.sensors > 0
                && let Some(h) = imp::open(d.number)
            {
                for (i, t) in imp::temperatures(&h).into_iter().enumerate() {
                    let label =
                        if i == 0 { "Drive Temperature".to_owned() } else { format!("Drive Temperature {}", i + 1) };
                    let t = if (-40.0..=150.0).contains(&t) && t != 0.0 { t } else { f64::NAN };
                    out.put(
                        &SensorInfo::new(&dev, format!("temp{i}"), Group::Temperatures, label, "°C", 0)
                            .source("IOCTL_STORAGE_QUERY_PROPERTY, StorageDeviceTemperatureProperty"),
                        t,
                    );
                }
            }
            if let Some(c) = counters.get(&d.number) {
                out.put(
                    &SensorInfo::new(&dev, "load/activity", Group::Load, "Total Activity", "%", 1)
                        .source("100 − \\PhysicalDisk\\% Idle Time"),
                    (100.0 - c[0]).clamp(0.0, 100.0),
                );
                const MB: f64 = 1024.0 * 1024.0;
                out.put(
                    &SensorInfo::new(&dev, "rate/read", Group::Throughput, "Read Rate", "MB/s", 2)
                        .source("\\PhysicalDisk\\Disk Read Bytes/sec"),
                    c[1] / MB,
                );
                out.put(
                    &SensorInfo::new(&dev, "rate/write", Group::Throughput, "Write Rate", "MB/s", 2)
                        .source("\\PhysicalDisk\\Disk Write Bytes/sec"),
                    c[2] / MB,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_instances_name_their_number_and_letters() {
        assert_eq!(parse_disk_instance("0 C:"), Some((0, "C:".into())));
        assert_eq!(parse_disk_instance("1 D: E:"), Some((1, "D: E:".into())));
        assert_eq!(parse_disk_instance("_Total"), None);
        assert_eq!(bus_name(17), "NVMe");
    }
}
