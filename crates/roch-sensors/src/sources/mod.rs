//! Every sensor source, and the list the engine starts from.

pub mod amd_smu;
pub mod asus_ec;
pub mod cpu_load;
pub mod dimm;
pub mod drives;
pub mod gpu;
pub mod intel_mchbar;
pub mod msr;
pub mod network;
pub mod smbus;
pub mod superio;
pub mod system_usage;
pub mod whea;

use std::sync::Arc;

use crate::engine::{Batch, Factory, Source};
use crate::system::SystemInfo;

/// Which optional sources to start.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The driver-based transports: the per-core MSRs, the board, DIMM, MCHBAR and EC sensors,
    /// and the AMD SMU's PM table.
    pub hardware: bool,
    /// Which kernel driver those go through.
    pub driver: crate::hw::Driver,
    pub gpu: bool,
    pub drives: bool,
    pub network: bool,
    /// Read the DDR5 PMIC's ADC (one selector write per channel, restored afterwards). Off reads
    /// only what needs no write: the hub temperature and the VID settings.
    pub pmic_adc: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { hardware: true, driver: crate::hw::Driver::Auto, gpu: true, drives: true, network: true, pmic_adc: true }
    }
}

fn boxed<S: Source + 'static>(s: Option<S>) -> Option<Box<dyn Source>> {
    s.map(|s| Box::new(s) as Box<dyn Source>)
}

/// The sources for this machine, each as a factory the engine runs on the source's own thread.
pub fn factories(sys: &SystemInfo, options: Options) -> Vec<(String, Factory)> {
    let sys = Arc::new(sys.clone());
    let mut list: Vec<(String, Factory)> = Vec::new();
    let mut add = |name: &str, f: Factory| list.push((name.to_owned(), f));

    {
        let s = sys.clone();
        add("System", Box::new(move |_: &mut Batch| boxed(Some(system_usage::SystemUsage::new(&s)))));
    }
    // Decided before anything starts, so the counter clocks know whether to stand aside.
    let driver = options.driver;
    let msr = options.hardware && driver.pawnio() && msr::available(&sys);
    {
        let s = sys.clone();
        add("CPU counters", Box::new(move |b: &mut Batch| boxed(cpu_load::CpuLoad::detect(&s, b, !msr))));
    }
    if options.hardware {
        let s = sys.clone();
        if driver.pawnio() {
            add("Per-core MSRs", Box::new(move |b: &mut Batch| boxed(msr::CpuMsr::detect(&s, b))));
        } else {
            add(
                "Per-core MSRs",
                Box::new(|b: &mut Batch| {
                    b.status("Per-core MSRs: read through PawnIO only, which --driver inpoutx64 rules out.");
                    None
                }),
            );
        }
        let s = sys.clone();
        add("MCHBAR", Box::new(move |b: &mut Batch| boxed(intel_mchbar::IntelMchbar::detect(&s, b, driver))));
        let s = sys.clone();
        add("SMU PM table", Box::new(move |b: &mut Batch| boxed(amd_smu::AmdSmu::detect(&s, b, driver))));
        let s = sys.clone();
        add("Super I/O", Box::new(move |b: &mut Batch| boxed(superio::SuperIo::detect(&s, b, driver))));
        let s = sys.clone();
        add("ASUS EC", Box::new(move |b: &mut Batch| boxed(asus_ec::AsusEc::detect(&s, b, driver))));
        let s = sys.clone();
        let adc = options.pmic_adc;
        add("DIMM sensors", Box::new(move |b: &mut Batch| boxed(dimm::Dimms::detect(&s, b, adc, driver))));
    }
    if options.gpu {
        add("Graphics", Box::new(|b: &mut Batch| boxed(gpu::Gpus::detect(b))));
    }
    if options.drives {
        add("Drives", Box::new(|b: &mut Batch| boxed(drives::Drives::detect(b))));
    }
    if options.network {
        add("Network", Box::new(|_: &mut Batch| boxed(Some(network::Network::new()))));
    }
    add("WHEA", Box::new(|_: &mut Batch| boxed(Some(whea::Whea::new()))));
    list
}
