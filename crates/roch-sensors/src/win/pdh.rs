//! Performance counters through PDH, read natively (Roch Viewer's `cpu_clocks._Pdh`).
//!
//! Roch Viewer measured the routes to these numbers: WMI's perf classes take 452 ms a query, PDH
//! 0.12 ms for the same counters. Counters are added by their English path, so the paths do not
//! depend on the display language. Rate counters need two collections before they mean anything,
//! so a query is opened once and kept.

#[cfg(windows)]
use windows_sys::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};

/// PDH_CSTATUS_VALID_DATA and PDH_CSTATUS_NEW_DATA: the two statuses an item can be used with.
const VALID: u32 = 0;
const NEW_DATA: u32 = 1;

pub struct Query {
    handle: isize,
    counters: Vec<isize>,
    primed: bool,
    last: Option<std::time::Instant>,
}

/// Two collections closer than this divide a rate by almost nothing: Roch Viewer saw 7410 MHz from
/// a real 5675 MHz that way, and this monitor saw 9147. A collection inside it is refused.
pub const MIN_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

// A PDH query handle may be used from any one thread at a time; each source owns its own.
unsafe impl Send for Query {}

impl Query {
    /// Open a query with these counter paths. A path the machine does not have is skipped and its
    /// slot reads empty, so callers index counters by the order they asked for them.
    #[cfg(windows)]
    pub fn open(paths: &[&str]) -> Option<Query> {
        let mut handle = 0isize;
        // SAFETY: plain PDH calls with owned buffers.
        unsafe {
            if PdhOpenQueryW(std::ptr::null(), 0, &mut handle) != 0 {
                return None;
            }
            let mut counters = Vec::with_capacity(paths.len());
            let mut any = false;
            for path in paths {
                let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
                let mut counter = 0isize;
                if PdhAddEnglishCounterW(handle, wide.as_ptr(), 0, &mut counter) == 0 {
                    any = true;
                    counters.push(counter);
                } else {
                    counters.push(0);
                }
            }
            if !any {
                PdhCloseQuery(handle);
                return None;
            }
            Some(Query { handle, counters, primed: false, last: None })
        }
    }

    #[cfg(not(windows))]
    pub fn open(_paths: &[&str]) -> Option<Query> {
        None
    }

    /// Take a sample. Returns false for the very first one, which only sets the baseline a rate
    /// counter is measured against, and for one asked for within [`MIN_SAMPLE_INTERVAL`] of the
    /// last, which is refused rather than taken.
    #[cfg(windows)]
    pub fn collect(&mut self) -> bool {
        let now = std::time::Instant::now();
        if self.last.is_some_and(|t| now.duration_since(t) < MIN_SAMPLE_INTERVAL) {
            return false;
        }
        // SAFETY: the handle is ours and open.
        if unsafe { PdhCollectQueryData(self.handle) } != 0 {
            return false;
        }
        self.last = Some(now);
        let was = self.primed;
        self.primed = true;
        was
    }

    #[cfg(not(windows))]
    pub fn collect(&mut self) -> bool {
        false
    }

    /// Every instance of counter `index` as (instance name, value).
    #[cfg(windows)]
    pub fn array(&self, index: usize) -> Vec<(String, f64)> {
        let Some(&counter) = self.counters.get(index) else { return Vec::new() };
        if counter == 0 {
            return Vec::new();
        }
        let mut size = 0u32;
        let mut count = 0u32;
        // SAFETY: the size query, then a read into a buffer of that many bytes; the items and the
        // instance-name strings they point at both live inside that buffer.
        unsafe {
            let status =
                PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, std::ptr::null_mut());
            if status != PDH_MORE_DATA || count == 0 {
                return Vec::new();
            }
            let mut buffer = vec![0u64; (size as usize).div_ceil(8) + 1];
            let items = buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, items) != 0 {
                return Vec::new();
            }
            let mut out = Vec::with_capacity(count as usize);
            for i in 0..count as usize {
                let item = &*items.add(i);
                if item.FmtValue.CStatus != VALID && item.FmtValue.CStatus != NEW_DATA {
                    continue;
                }
                out.push((wide_to_string(item.szName), item.FmtValue.Anonymous.doubleValue));
            }
            out
        }
    }

    #[cfg(not(windows))]
    pub fn array(&self, _index: usize) -> Vec<(String, f64)> {
        Vec::new()
    }

    /// One instance of counter `index` by exact name ("_Total").
    pub fn instance(&self, index: usize, name: &str) -> Option<f64> {
        self.array(index).into_iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }
}

#[cfg(windows)]
impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: closing our own query.
        unsafe {
            PdhCloseQuery(self.handle);
        }
    }
}

#[cfg(windows)]
unsafe fn wide_to_string(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: PDH hands back NUL-terminated strings inside the item buffer.
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

/// Sort key for "group,processor" instance names: "0,10" sorts after "0,2", not between "0,1"
/// and "0,2" as text would put it.
pub fn instance_order(name: &str) -> (u32, u32) {
    let mut parts = name.split(',').map(|p| p.trim().parse::<u32>().unwrap_or(u32::MAX));
    (parts.next().unwrap_or(u32::MAX), parts.next().unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processor_instances_sort_numerically() {
        let mut names = vec!["0,10", "0,2", "0,1", "1,0", "0,_Total"];
        names.sort_by_key(|n| instance_order(n));
        assert_eq!(names, vec!["0,1", "0,2", "0,10", "0,_Total", "1,0"]);
    }

    #[cfg(windows)]
    #[test]
    fn the_processor_counters_answer_after_a_baseline() {
        let mut q = Query::open(&[r"\Processor Information(_Total)\% Processor Time"]).expect("PDH");
        assert!(!q.collect());
        assert!(!q.collect(), "a second collection straight after the first is refused");
        std::thread::sleep(MIN_SAMPLE_INTERVAL + std::time::Duration::from_millis(50));
        assert!(q.collect());
        let total = q.instance(0, "_Total").expect("a _Total instance");
        assert!((0.0..=100.5).contains(&total));
    }
}
