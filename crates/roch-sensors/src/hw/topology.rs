//! Which logical processors share a core, and which cores are performance or efficiency cores.
//!
//! Roch Viewer names its per-processor rows "CPU 0".."CPU n" because the topology was not
//! something its transports could read. Windows already knows it: GetLogicalProcessorInformationEx
//! reports every core with its efficiency class and the logical processors on it, so the rows can
//! say "P-core 3 T1" the way HWiNFO does, without a driver.

/// One logical processor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logical {
    pub group: u16,
    pub number: u8,
    pub core: usize,
    pub thread: usize,
    pub performance: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Topology {
    pub logical: Vec<Logical>,
    pub cores: usize,
    pub hybrid: bool,
}

impl Topology {
    /// The row label for one logical processor, by its PDH instance ("0,5").
    pub fn label(&self, group: u16, number: u8) -> Option<String> {
        let l = self.logical.iter().find(|l| l.group == group && l.number == number)?;
        let threads_on_core = self.logical.iter().filter(|o| o.core == l.core).count();
        let kind = if !self.hybrid {
            "Core"
        } else if l.performance {
            "P-core"
        } else {
            "E-core"
        };
        Some(if threads_on_core > 1 {
            format!("{kind} {} T{}", l.core, l.thread)
        } else {
            format!("{kind} {}", l.core)
        })
    }

    /// Build from (efficiency class, [(group, mask)]) per core, in the order Windows lists them.
    pub fn from_cores(cores: &[(u8, Vec<(u16, u64)>)]) -> Topology {
        let max_class = cores.iter().map(|c| c.0).max().unwrap_or(0);
        let min_class = cores.iter().map(|c| c.0).min().unwrap_or(0);
        let hybrid = max_class != min_class;
        let mut logical = Vec::new();
        // Performance cores are numbered first, then efficiency cores, as HWiNFO numbers them.
        let mut order: Vec<usize> = (0..cores.len()).collect();
        order.sort_by_key(|&i| (std::cmp::Reverse(cores[i].0), i));
        for (core_index, &i) in order.iter().enumerate() {
            let mut thread = 0;
            for &(group, mask) in &cores[i].1 {
                for bit in 0..64u8 {
                    if mask & (1u64 << bit) != 0 {
                        logical.push(Logical {
                            group,
                            number: bit,
                            core: core_index,
                            thread,
                            performance: cores[i].0 == max_class,
                        });
                        thread += 1;
                    }
                }
            }
        }
        Topology { logical, cores: cores.len(), hybrid }
    }

    #[cfg(windows)]
    pub fn read() -> Topology {
        use windows_sys::Win32::System::SystemInformation::{
            GetLogicalProcessorInformationEx, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
        };
        let mut len = 0u32;
        // SAFETY: a size query, then a read into a buffer of that size; each record is walked by
        // its own Size field and only read within the buffer.
        unsafe {
            GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut len);
            if len == 0 {
                return Topology::default();
            }
            let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
            let base = buffer.as_mut_ptr() as *mut u8;
            if GetLogicalProcessorInformationEx(RelationProcessorCore, base as *mut _, &mut len) == 0 {
                return Topology::default();
            }
            let mut cores = Vec::new();
            let mut at = 0usize;
            while at + std::mem::size_of::<u32>() * 2 <= len as usize {
                let record = base.add(at) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;
                let size = (*record).Size as usize;
                if size == 0 {
                    break;
                }
                if (*record).Relationship == RelationProcessorCore {
                    let p = &(*record).Anonymous.Processor;
                    let masks_ptr = p.GroupMask.as_ptr();
                    let masks = (0..p.GroupCount as usize)
                        .map(|g| {
                            let m = *masks_ptr.add(g);
                            (m.Group, m.Mask as u64)
                        })
                        .collect();
                    cores.push((p.EfficiencyClass, masks));
                }
                at += size;
            }
            Topology::from_cores(&cores)
        }
    }

    #[cfg(not(windows))]
    pub fn read() -> Topology {
        Topology::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hybrid_part_names_p_and_e_cores() {
        // Two P-cores with HT (class 1), then two E-cores (class 0).
        let t = Topology::from_cores(&[
            (1, vec![(0, 0b11)]),
            (1, vec![(0, 0b1100)]),
            (0, vec![(0, 0b10000)]),
            (0, vec![(0, 0b100000)]),
        ]);
        assert!(t.hybrid);
        assert_eq!(t.label(0, 0).unwrap(), "P-core 0 T0");
        assert_eq!(t.label(0, 3).unwrap(), "P-core 1 T1");
        assert_eq!(t.label(0, 4).unwrap(), "E-core 2");
        assert_eq!(t.label(0, 5).unwrap(), "E-core 3");
    }

    #[test]
    fn a_uniform_part_names_plain_cores() {
        let t = Topology::from_cores(&[(0, vec![(0, 0b11)]), (0, vec![(0, 0b1100)])]);
        assert!(!t.hybrid);
        assert_eq!(t.label(0, 2).unwrap(), "Core 1 T0");
    }

    #[cfg(windows)]
    #[test]
    fn this_machine_has_cores() {
        let t = Topology::read();
        assert!(t.cores > 0);
        assert_eq!(t.logical.len(), std::thread::available_parallelism().map(|n| n.get()).unwrap_or(t.logical.len()));
    }
}
