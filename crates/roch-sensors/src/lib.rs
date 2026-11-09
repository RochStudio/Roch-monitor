//! Roch Monitor's sensor engine.
//!
//! Roch Viewer's hardware transports (the Super I/O, the PCH and FCH SMBus, the memory
//! controller's MCHBAR, the ACPI EC, the performance counters, the WHEA log), ported from Python,
//! and Roch GPU's vendor backends, behind one polling engine that keeps every reading's current,
//! minimum, maximum and average the way both tools' telemetry windows do.
//!
//! Read-only throughout. The only writes anywhere are selectors that choose what the next read
//! returns, each documented where it is made.

pub mod engine;
pub mod hw;
pub mod model;
pub mod sources;
pub mod system;
pub mod win;

pub use engine::{Batch, Engine, Source, State};
pub use model::{DeviceInfo, DeviceKind, Group, SensorInfo, SensorStat, format_plain, format_value};
pub use system::SystemInfo;
