//! CSV logging, the way HWiNFO logs: one row per interval, one column per sensor, the columns fixed
//! when logging starts so a spreadsheet can read the whole file.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use roch_sensors::engine::Shared;
use roch_sensors::model::format_plain;

pub struct CsvLogger {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    rows: Arc<AtomicU64>,
    path: PathBuf,
}

/// Quote a CSV field when it needs it.
fn field(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_owned() }
}

/// Where logs go by default: Documents\Roch Monitor.
pub fn default_directory() -> PathBuf {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Documents").join("Roch Monitor")
}

pub fn timestamped_name() -> String {
    format!("RochMonitor_{}.csv", chrono::Local::now().format("%Y-%m-%d_%H-%M-%S"))
}

impl CsvLogger {
    /// Start writing to `path`. `skip` are sensor keys left out (the ones hidden in the window).
    pub fn start(shared: Arc<Shared>, path: &Path, skip: &[String]) -> std::io::Result<CsvLogger> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = BufWriter::new(File::create(path)?);
        let columns: Vec<(usize, u8)> = {
            let st = shared.lock();
            let mut header = vec!["Date".to_owned(), "Time".to_owned()];
            let mut cols = Vec::new();
            for d in st.ordered_devices() {
                for (i, row) in st.sensors.iter().enumerate() {
                    if row.info.device != d.id || skip.contains(&row.info.key) {
                        continue;
                    }
                    let unit = if row.info.unit.is_empty() || row.info.unit == "state" {
                        String::new()
                    } else {
                        format!(" [{}]", row.info.unit)
                    };
                    header.push(field(&format!("{}: {}{}", d.name, row.info.label, unit)));
                    cols.push((i, row.info.decimals));
                }
            }
            writeln!(out, "{}", header.join(","))?;
            cols
        };
        out.flush()?;
        let stop = Arc::new(AtomicBool::new(false));
        let rows = Arc::new(AtomicU64::new(0));
        let thread = {
            let (stop, rows) = (stop.clone(), rows.clone());
            std::thread::Builder::new().name("Roch Monitor: CSV log".into()).spawn(move || {
                let mut next = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    let now = chrono::Local::now();
                    let line = {
                        let st = shared.lock();
                        let mut cells =
                            vec![now.format("%d.%m.%Y").to_string(), now.format("%H:%M:%S%.3f").to_string()];
                        for &(i, decimals) in &columns {
                            cells.push(
                                st.sensors.get(i).map_or(String::new(), |r| format_plain(r.stat.current, decimals)),
                            );
                        }
                        cells.join(",")
                    };
                    if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                        break;
                    }
                    rows.fetch_add(1, Ordering::Relaxed);
                    next += shared.interval();
                    while !stop.load(Ordering::Relaxed) && Instant::now() < next {
                        std::thread::sleep((next - Instant::now()).min(std::time::Duration::from_millis(50)));
                    }
                }
                let _ = out.flush();
            })?
        };
        Ok(CsvLogger { stop, thread: Some(thread), rows, path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn rows(&self) -> u64 {
        self.rows.load(Ordering::Relaxed)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for CsvLogger {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_quoted_only_when_needed() {
        assert_eq!(field("CPU: Core Max [°C]"), "CPU: Core Max [°C]");
        assert_eq!(field("A, B"), "\"A, B\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
