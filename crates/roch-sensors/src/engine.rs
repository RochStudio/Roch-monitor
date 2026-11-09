//! The polling engine: one thread per source, one shared table of sensors.
//!
//! Each source runs on its own thread for the reason Roch Viewer moved its reads off the UI thread:
//! the reads block. A DDR5 PMIC needs 12 ms per ADC channel to settle, a GPU driver call can take
//! milliseconds, the event log a few dozen. On one thread the slowest would set everyone's pace;
//! on their own, a source that stalls costs its own rows and nobody else's.
//!
//! A tick that overruns its interval is skipped rather than queued, as both Roch tools do it:
//! falling behind should cost samples, not build a backlog against the same bus mutexes.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::model::{DeviceInfo, History, SensorInfo, SensorStat};

/// What one poll found: devices and sensors to declare, and readings for sensors by key.
#[derive(Default)]
pub struct Batch {
    pub devices: Vec<DeviceInfo>,
    /// Devices this source adds rows to but does not own: declared only if nobody else has.
    pub guest_devices: Vec<DeviceInfo>,
    pub sensors: Vec<SensorInfo>,
    pub values: Vec<(String, f64)>,
    /// A one-line status for this source, replacing its previous one: what it found and where.
    pub status: Option<String>,
}

impl Batch {
    pub fn device(&mut self, device: DeviceInfo) {
        self.devices.push(device);
    }

    /// Add rows under a device another source owns (the ASUS EC's VRM under the board), creating
    /// the heading only when that source has not.
    pub fn guest_device(&mut self, device: DeviceInfo) {
        self.guest_devices.push(device);
    }

    /// Declare a sensor (idempotent; the first declaration wins) and give it a reading.
    pub fn put(&mut self, info: &SensorInfo, value: f64) {
        self.sensors.push(info.clone());
        self.values.push((info.key.clone(), value));
    }

    /// A reading for a sensor that is already declared.
    pub fn value(&mut self, key: impl Into<String>, value: f64) {
        self.values.push((key.into(), value));
    }

    /// Declare without a reading: a row that exists but has not answered yet.
    pub fn declare(&mut self, info: SensorInfo) {
        self.sensors.push(info);
    }

    pub fn status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
    }
}

/// A sensor source: one transport, or one family of readings that share a transport.
pub trait Source: Send {
    fn name(&self) -> &str;

    /// Read everything once. Called every interval on this source's own thread.
    fn poll(&mut self, out: &mut Batch);

    /// Rows worth sampling between ticks, Roch Viewer's FAST_SAMPLE_LABELS: cheap reads whose
    /// peaks a once-a-second read misses (Core Max, one MCHBAR word).
    fn fast_interval(&self) -> Option<Duration> {
        None
    }

    fn fast_poll(&mut self, _out: &mut Batch) {}
}

/// A source is built on its own thread, because finding the hardware is itself a read: a Super I/O
/// probe, a GPU driver initialising. `None` means this machine has nothing for it; the status
/// says why.
pub type Factory = Box<dyn FnOnce(&mut Batch) -> Option<Box<dyn Source>> + Send>;

pub struct SensorRow {
    pub info: SensorInfo,
    pub stat: SensorStat,
    pub history: History,
    /// The engine tick of the last reading, so the UI can dim a row that stopped answering.
    pub last_seen: Option<Instant>,
}

pub struct SourceStatus {
    pub name: String,
    pub status: String,
    pub last_poll: Duration,
    pub polls: u64,
    pub failed: bool,
}

pub struct State {
    pub devices: Vec<DeviceInfo>,
    pub sensors: Vec<SensorRow>,
    index: HashMap<String, usize>,
    pub sources: Vec<SourceStatus>,
    /// Bumped whenever a device or sensor is added, so a view can rebuild its row list only then.
    pub layout_revision: u64,
    /// Bumped on every applied poll.
    pub value_revision: u64,
    pub stats_since: Instant,
    pub history_capacity: usize,
}

impl State {
    fn new(history_capacity: usize) -> Self {
        Self {
            devices: Vec::new(),
            sensors: Vec::new(),
            index: HashMap::new(),
            sources: Vec::new(),
            layout_revision: 0,
            value_revision: 0,
            stats_since: Instant::now(),
            history_capacity,
        }
    }

    pub fn sensor(&self, key: &str) -> Option<&SensorRow> {
        self.index.get(key).map(|&i| &self.sensors[i])
    }

    pub fn sensor_index(&self, key: &str) -> Option<usize> {
        self.index.get(key).copied()
    }

    pub fn device(&self, id: &str) -> Option<&DeviceInfo> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// Devices in display order: by kind, then their own order, then name.
    pub fn ordered_devices(&self) -> Vec<&DeviceInfo> {
        let mut d: Vec<&DeviceInfo> = self.devices.iter().collect();
        d.sort_by(|a, b| (a.kind, a.order, &a.name).cmp(&(b.kind, b.order, &b.name)));
        d
    }

    /// Clear every sensor's minimum, maximum and average, and the graphs with them.
    pub fn reset_stats(&mut self) {
        for s in &mut self.sensors {
            s.stat.reset();
            s.history.clear();
        }
        self.stats_since = Instant::now();
        self.value_revision += 1;
    }

    pub fn reset_sensor(&mut self, key: &str) {
        if let Some(&i) = self.index.get(key) {
            self.sensors[i].stat.reset();
            self.value_revision += 1;
        }
    }

    fn apply(&mut self, source: usize, batch: Batch, epoch: Instant, fast: bool) {
        let now = Instant::now();
        for d in batch.devices {
            match self.devices.iter_mut().find(|e| e.id == d.id) {
                Some(existing) => {
                    if *existing != d {
                        *existing = d;
                        self.layout_revision += 1;
                    }
                }
                None => {
                    self.devices.push(d);
                    self.layout_revision += 1;
                }
            }
        }
        for d in batch.guest_devices {
            if !self.devices.iter().any(|e| e.id == d.id) {
                self.devices.push(d);
                self.layout_revision += 1;
            }
        }
        for s in batch.sensors {
            if !self.index.contains_key(&s.key) {
                self.index.insert(s.key.clone(), self.sensors.len());
                self.sensors.push(SensorRow {
                    info: s,
                    stat: SensorStat::default(),
                    history: History::new(self.history_capacity),
                    last_seen: None,
                });
                self.layout_revision += 1;
            }
        }
        let t = now.duration_since(epoch).as_secs_f32();
        for (key, value) in batch.values {
            let Some(&i) = self.index.get(&key) else { continue };
            let row = &mut self.sensors[i];
            if fast {
                row.stat.add_between(value);
            } else {
                row.stat.add(value);
                row.history.push(t, value);
                if value.is_finite() {
                    row.last_seen = Some(now);
                }
            }
        }
        if let (Some(status), Some(src)) = (batch.status, self.sources.get_mut(source)) {
            src.status = status;
        }
        if !fast {
            self.value_revision += 1;
        }
    }
}

type Repaint = Box<dyn Fn() + Send + Sync>;

pub struct Shared {
    state: Mutex<State>,
    interval_ms: AtomicU64,
    stop: AtomicBool,
    repaint: Mutex<Option<Repaint>>,
    epoch: Instant,
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms.load(Ordering::Relaxed).max(100))
    }

    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    fn notify(&self) {
        if let Some(f) = self.repaint.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            f();
        }
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

/// The shortest interval the window offers; below this the bus mutexes are held more than they are
/// free, which is unfair to every other tool on the machine.
pub const MIN_INTERVAL_MS: u64 = 250;

impl Engine {
    /// Start one thread per factory. `history_capacity` is points kept per sensor for the graphs.
    pub fn start(factories: Vec<(String, Factory)>, interval_ms: u64, history_capacity: usize) -> Engine {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new(history_capacity)),
            interval_ms: AtomicU64::new(interval_ms.max(MIN_INTERVAL_MS)),
            stop: AtomicBool::new(false),
            repaint: Mutex::new(None),
            epoch: Instant::now(),
        });
        {
            let mut st = shared.lock();
            for (name, _) in &factories {
                st.sources.push(SourceStatus {
                    name: name.clone(),
                    status: "Starting…".into(),
                    last_poll: Duration::ZERO,
                    polls: 0,
                    failed: false,
                });
            }
        }
        let mut threads = Vec::new();
        for (index, (name, factory)) in factories.into_iter().enumerate() {
            let shared = shared.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("Roch Monitor: {name}"))
                .spawn(move || run_source(index, factory, &shared));
            if let Ok(handle) = spawned {
                threads.push(handle);
            }
        }
        Engine { shared, threads }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.lock()
    }

    pub fn set_interval(&self, ms: u64) {
        self.shared.interval_ms.store(ms.max(MIN_INTERVAL_MS), Ordering::Relaxed);
    }

    pub fn interval(&self) -> Duration {
        self.shared.interval()
    }

    /// Called after each applied poll: the window asks itself to repaint.
    pub fn on_update(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.shared.repaint.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
    }

    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Sleep until `until`, in short steps so a stop request or an interval change is noticed.
fn sleep_until(shared: &Shared, until: Instant) {
    while !shared.stopping() {
        let now = Instant::now();
        if now >= until {
            return;
        }
        std::thread::sleep((until - now).min(Duration::from_millis(50)));
    }
}

fn run_source(index: usize, factory: Factory, shared: &Shared) {
    let mut first = Batch::default();
    let built = catch_unwind(AssertUnwindSafe(|| factory(&mut first)));
    let mut source = match built {
        Ok(Some(source)) => source,
        Ok(None) => {
            let status = first.status.take().unwrap_or_else(|| "Not present on this machine.".into());
            let mut st = shared.lock();
            st.apply(index, first, shared.epoch, false);
            if let Some(s) = st.sources.get_mut(index) {
                s.status = status;
                s.failed = true;
            }
            drop(st);
            shared.notify();
            return;
        }
        Err(_) => {
            let mut st = shared.lock();
            if let Some(s) = st.sources.get_mut(index) {
                s.status = "Stopped: detection panicked.".into();
                s.failed = true;
            }
            return;
        }
    };
    shared.lock().apply(index, first, shared.epoch, false);

    let mut next = Instant::now();
    while !shared.stopping() {
        let started = Instant::now();
        let mut batch = Batch::default();
        let polled = catch_unwind(AssertUnwindSafe(|| source.poll(&mut batch)));
        let took = started.elapsed();
        {
            let mut st = shared.lock();
            match polled {
                Ok(()) => st.apply(index, batch, shared.epoch, false),
                Err(_) => {
                    if let Some(s) = st.sources.get_mut(index) {
                        s.status = format!("Stopped: {} panicked while reading.", source.name());
                        s.failed = true;
                    }
                    drop(st);
                    shared.notify();
                    return;
                }
            }
            if let Some(s) = st.sources.get_mut(index) {
                s.last_poll = took;
                s.polls += 1;
            }
        }
        shared.notify();

        let interval = shared.interval();
        next += interval;
        let now = Instant::now();
        if next < now {
            // Overran: skip the ticks that were missed instead of firing them back to back.
            next = now + interval;
        }
        if let Some(fast) = source.fast_interval() {
            let mut at = Instant::now() + fast;
            while at + fast / 2 < next && !shared.stopping() {
                sleep_until(shared, at);
                let mut b = Batch::default();
                if catch_unwind(AssertUnwindSafe(|| source.fast_poll(&mut b))).is_ok() {
                    shared.lock().apply(index, b, shared.epoch, true);
                }
                at += fast;
            }
        }
        sleep_until(shared, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Group;

    struct Counter {
        n: f64,
    }

    impl Source for Counter {
        fn name(&self) -> &str {
            "counter"
        }
        fn poll(&mut self, out: &mut Batch) {
            self.n += 1.0;
            out.device(DeviceInfo::new("t", crate::model::DeviceKind::System, "Test"));
            out.put(&SensorInfo::new("t", "n", Group::Other, "N", "", 0), self.n);
            out.status("counting");
        }
    }

    #[test]
    fn a_source_is_polled_and_its_rows_appear() {
        let mut engine = Engine::start(
            vec![("counter".into(), Box::new(|_: &mut Batch| Some(Box::new(Counter { n: 0.0 }) as Box<dyn Source>)))],
            MIN_INTERVAL_MS,
            16,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let st = engine.lock();
                if let Some(row) = st.sensor("t/n")
                    && row.stat.samples() >= 2
                {
                    assert_eq!(row.stat.minimum, 1.0);
                    assert_eq!(st.sources[0].status, "counting");
                    break;
                }
            }
            assert!(Instant::now() < deadline, "no samples");
            std::thread::sleep(Duration::from_millis(20));
        }
        engine.stop();
    }

    #[test]
    fn a_missing_source_says_why() {
        let mut engine = Engine::start(
            vec![(
                "absent".into(),
                Box::new(|b: &mut Batch| {
                    b.status("no such chip");
                    None
                }),
            )],
            MIN_INTERVAL_MS,
            16,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.lock().sources[0].status != "no such chip" {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(engine.lock().sources[0].failed);
        engine.stop();
    }
}
