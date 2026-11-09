//! Every sensor source, and the list the engine starts from.

pub mod amd_smu;
pub mod asus_ec;
pub mod cpu_load;
pub mod dimm;
pub mod drives;
pub mod gpu;
pub mod intel_mchbar;
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
    /// The board, DIMM, MCHBAR, SMU PM-table and EC transports, which need the inpoutx64 driver.
    pub hardware: bool,
    pub gpu: bool,
    pub drives: bool,
    pub network: bool,
    /// Read the DDR5 PMIC's ADC (one selector write per channel, restored afterwards). Off reads
    /// only what needs no write: the hub temperature and the VID settings.
    pub pmic_adc: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { hardware: true, gpu: true, drives: true, network: true, pmic_adc: true }
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
    {
        let s = sys.clone();
        add("CPU counters", Box::new(move |b: &mut Batch| boxed(cpu_load::CpuLoad::detect(&s, b))));
    }
    if options.hardware {
        let s = sys.clone();
        add("MCHBAR", Box::new(move |b: &mut Batch| boxed(intel_mchbar::IntelMchbar::detect(&s, b))));
        let s = sys.clone();
        add("SMU PM table", Box::new(move |b: &mut Batch| boxed(amd_smu::AmdSmu::detect(&s, b))));
        let s = sys.clone();
        add("Super I/O", Box::new(move |b: &mut Batch| boxed(superio::SuperIo::detect(&s, b))));
        let s = sys.clone();
        add("ASUS EC", Box::new(move |b: &mut Batch| boxed(asus_ec::AsusEc::detect(&s, b))));
        let s = sys.clone();
        let adc = options.pmic_adc;
        add("DIMM sensors", Box::new(move |b: &mut Batch| boxed(dimm::Dimms::detect(&s, b, adc))));
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
