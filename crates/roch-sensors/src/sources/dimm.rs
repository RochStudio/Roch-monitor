//! The modules' own sensors, one device per populated slot (Roch Viewer's `ddr4_tsod`,
//! `ddr5_pmic` and `ddr5_telemetry`, and its per-DIMM telemetry panels).
//!
//! DDR4 may carry a JEDEC JC-42.4 thermal sensor at 0x18 + slot. It is optional, and a kit built
//! without one does not answer, so a DIMM either reports what its sensor said or nothing.
//!
//! DDR5 moved the thermal sensor inside the SPD5 hub at 0x50 + slot, and gave each module a PMIC at
//! 0x48 + slot that owns VDD, VDDQ and VPP. The VID registers say what a rail is set to; the ADC
//! says what it measures, which differs between two modules on one board setting. Reading the ADC
//! costs one write per channel to its selector (R30h), which is restored afterwards.

use std::time::Duration;

use crate::engine::{Batch, Source};
use crate::hw::cpuid::Vendor;
use crate::hw::inpout::driver;
use crate::model::{DeviceInfo, DeviceKind, Group, SensorInfo};
use crate::sources::smbus::{PMICS, Platform, SPD_HUBS, Session, Smbus, THERMAL_SENSORS};
use crate::system::{SystemInfo, slot_name};

// --- DDR4, JC-42.4 / TSE2004av.
const TSOD_AMBIENT: u8 = 0x05;

/// The ambient register: 13 bits of two's complement in 1/16 °C, the top three bits alarm flags.
/// None outside what the part can report, which is a failed transfer, not a cold DIMM.
pub fn decode_tsod(high: u8, low: u8) -> Option<f64> {
    let raw = ((high as u16) << 8) | low as u16;
    let mut value = (raw & 0x1FFF) as i32;
    if value & 0x1000 != 0 {
        value -= 0x2000;
    }
    let celsius = value as f64 * 0.0625;
    (-40.0..=125.0).contains(&celsius).then_some(celsius)
}

// --- DDR5 SPD hub (JESD300-5).
const HUB_TYPE: [u8; 2] = [0x51, 0x18];
const HUB_TEMPERATURE: u8 = 0x31;

/// MR49/MR50: an 11-bit two's-complement value in quarter degrees, from bit 2.
pub fn decode_hub_temperature(low: u8, high: u8) -> f64 {
    let raw = ((high as u16) << 8) | low as u16;
    let mut value = ((raw >> 2) & 0x7FF) as i32;
    if value & 0x400 != 0 {
        value -= 0x800;
    }
    value as f64 * 0.25
}

// --- DDR5 PMIC (JESD301).
const ADC_VALUE: u8 = 0x31;
/// JESD301-2 asks 9 ms after selecting a channel. At 9 ms with two DIMMs read in turn, R31h still
/// held the previous channel's sample often enough to rotate the readings, so: longer, and the
/// first read after settling is thrown away.
const ADC_SETTLE: Duration = Duration::from_millis(12);

/// (key, label, ADC channel, mV per count, max mV)
const ADC_CHANNELS: [(&str, &str, u8, f64, f64); 6] = [
    ("vdd", "VDD (SWA)", 0x0, 15.0, 3000.0),
    ("vddq", "VDDQ (SWB)", 0x2, 15.0, 3000.0),
    ("vpp", "VPP (SWC)", 0x3, 15.0, 3000.0),
    ("vin", "VIN Bulk", 0x5, 70.0, 15000.0),
    ("vout1v8", "VOUT 1.8V", 0x8, 15.0, 3000.0),
    ("vout1v0", "VOUT 1.0V", 0x9, 15.0, 3000.0),
];

pub fn adc_select_byte(channel: u8) -> u8 {
    0x80 | ((channel & 0x0F) << 3)
}

pub fn decode_adc_mv(raw: u8, step_mv: f64, max_mv: f64) -> Option<f64> {
    let mv = raw as f64 * step_mv;
    (raw != 0 && mv <= max_mv).then_some(mv)
}

/// The VID registers: (register, base mV, 8-bit-mode bit in 0x2B).
const VID_RAILS: [(&str, u8, f64, Option<u8>); 3] =
    [("vdd", 0x21, 800.0, Some(1 << 4)), ("vddq", 0x25, 800.0, Some(1 << 5)), ("vpp", 0x27, 1500.0, None)];

pub fn decode_vid(raw: u8, base_mv: f64, eight_bit: bool) -> f64 {
    let steps = if eight_bit { raw as f64 } else { ((raw >> 1) & 0x7F) as f64 };
    (base_mv + steps * 5.0) / 1000.0
}

/// Total module power from the PMIC's telemetry registers. Two modes, told apart by R1Bh bit 6:
/// set, the registers are watts in 1/8 W; clear, they are current as a fraction of each rail's
/// configured limit, which the rail voltage turns into watts.
pub fn decode_power(regs: &[(u8, u8)], volts: &[f64; 3]) -> Option<f64> {
    let get = |r: u8| regs.iter().find(|(k, _)| *k == r).map(|(_, v)| *v);
    let swa = get(0x0C)? as f64;
    let swb = (get(0x0E)? & 0x3F) as f64;
    let swc = (get(0x0F)? & 0x3F) as f64;
    let mode = get(0x1B)?;
    if mode & (1 << 6) != 0 {
        let (a, b, c) = (swa * 0.125, swb * 0.125, swc * 0.125);
        let combined = get(0x1A).unwrap_or(0) & (1 << 1) != 0;
        return Some(if combined && a >= b + c { a } else { a + b + c });
    }
    let limits = get(0x20)?;
    const AB: [f64; 4] = [3.0, 4.0, 5.0, 6.0];
    const C: [f64; 4] = [0.5, 0.75, 1.0, 1.25];
    let phases = if get(0x29)? & (1 << 3) != 0 { 2.0 } else { 1.0 };
    let a = swa * AB[((limits >> 6) & 3) as usize] / 256.0 * phases;
    let b = swb * AB[((limits >> 2) & 3) as usize] / 64.0;
    let c = swc * C[(limits & 3) as usize] / 64.0;
    Some(a * volts[0] + b * volts[1] + c * volts[2])
}

pub fn pmic_vendor(bank_byte: u8, code: u8) -> String {
    let bank = (bank_byte & 0x7F) + 1;
    match (bank, code) {
        (11, 0x8C) => "Richtek Power".into(),
        _ => format!("0x{bank_byte:02X}{code:02X}"),
    }
}

pub fn pmic_revision(raw: u8) -> String {
    format!("{}.{}", ((raw >> 4) & 3) + 1, (raw >> 1) & 7)
}

#[derive(Debug, Clone)]
enum Kind {
    Ddr4 { tsod: u8 },
    Ddr5 { hub: u8, pmic: Option<u8> },
}

#[derive(Debug, Clone)]
struct Module {
    controller: u16,
    kind: Kind,
    device: DeviceInfo,
}

pub struct Dimms {
    bus: Smbus,
    modules: Vec<Module>,
    measure_adc: bool,
    status: String,
}

fn module_device(sys: &SystemInfo, slot: usize, detail: String) -> DeviceInfo {
    let name = slot_name(slot);
    let (title, size) = match sys.module_for_slot(slot) {
        Some(m) => {
            let maker = m.manufacturer.trim();
            let part = m.part_number.trim();
            let who = if maker.is_empty() || crate::hw::smbios::is_placeholder(maker) {
                part.to_owned()
            } else {
                format!("{maker} {part}")
            };
            (format!("DIMM {name}: {}", who.trim()), format!("{} GB", m.size_mb / 1024))
        }
        None => (format!("DIMM {name}"), String::new()),
    };
    let detail = if size.is_empty() { detail } else { format!("{size} · {detail}") };
    DeviceInfo::new(format!("dimm-{}", name.to_lowercase()), DeviceKind::Dimm, title).detail(detail).order(slot as u32)
}

impl Dimms {
    pub fn detect(sys: &SystemInfo, out: &mut Batch, measure_adc: bool) -> Option<Dimms> {
        let io = match driver() {
            Ok(io) => io,
            Err(e) => {
                out.status(format!("DIMM sensors unavailable: {e}"));
                return None;
            }
        };
        let bus = match sys.vendor() {
            Vendor::Intel => match Smbus::intel(io) {
                Ok(bus) => bus,
                Err(e) => {
                    out.status(format!("DIMM sensors: {e}."));
                    return None;
                }
            },
            // AM5 only: Roch Viewer's FCH path was written and validated there.
            Vendor::Amd if sys.cpu.family >= 0x19 && sys.memory_generation() == Some("DDR5") => Smbus::amd(io),
            _ => {
                out.status("DIMM sensors: no SMBus transport for this platform.");
                return None;
            }
        };
        let generation = sys.memory_generation();
        let mut modules = Vec::new();
        {
            let Some(s) = bus.session() else {
                out.status("DIMM sensors: the SMBus was held by another tool during detection.");
                return None;
            };
            for &controller in bus.controllers() {
                if generation != Some("DDR4") {
                    for hub in SPD_HUBS {
                        let ty = [s.read_ddr5_byte(controller, hub, 0x00), s.read_ddr5_byte(controller, hub, 0x01)];
                        // Believed only once the device identifies as an SPD5 hub.
                        if ty != [Ok(HUB_TYPE[0]), Ok(HUB_TYPE[1])] {
                            continue;
                        }
                        let slot = (hub - SPD_HUBS.start) as usize;
                        let pmic = PMICS.start + slot as u8;
                        let pmic = s.read_ddr5_byte(controller, pmic, 0x00).is_ok().then_some(pmic);
                        let mut detail = format!("SPD hub 0x{hub:02X}");
                        if let Some(p) = pmic {
                            let ident = (
                                s.read_ddr5_byte(controller, p, 0x3C),
                                s.read_ddr5_byte(controller, p, 0x3D),
                                s.read_ddr5_byte(controller, p, 0x3B),
                            );
                            detail.push_str(&format!(" · PMIC 0x{p:02X}"));
                            if let (Ok(bank), Ok(code), Ok(rev)) = ident {
                                detail.push_str(&format!(" {} rev {}", pmic_vendor(bank, code), pmic_revision(rev)));
                            }
                        }
                        modules.push(Module {
                            controller,
                            kind: Kind::Ddr5 { hub, pmic },
                            device: module_device(sys, slot, detail),
                        });
                    }
                }
                if modules.is_empty() && generation != Some("DDR5") && bus.platform == Platform::IntelPch {
                    for tsod in THERMAL_SENSORS {
                        if s.read_thermal_word(controller, tsod, TSOD_AMBIENT).is_ok() {
                            let slot = (tsod - THERMAL_SENSORS.start) as usize;
                            modules.push(Module {
                                controller,
                                kind: Kind::Ddr4 { tsod },
                                device: module_device(sys, slot, format!("thermal sensor 0x{tsod:02X}")),
                            });
                        }
                    }
                }
                if !modules.is_empty() {
                    // DIMMs live on one controller; probing the other blocks for nothing.
                    break;
                }
            }
        }
        let where_ = match bus.platform {
            Platform::IntelPch => format!("Intel PCH SMBus at 0x{:04X}", bus.base),
            Platform::AmdFch => format!("AMD FCH SMBus at 0x{:04X}", bus.base),
        };
        if modules.is_empty() {
            out.status(format!(
                "DIMM sensors: {where_}; no module answered (a DDR4 kit's thermal sensor is optional, and many are built without one)."
            ));
            return None;
        }
        let found: Vec<String> = modules
            .iter()
            .map(|m| match m.kind {
                Kind::Ddr4 { tsod } => format!("0x{tsod:02X}"),
                Kind::Ddr5 { hub, pmic } => match pmic {
                    Some(p) => format!("hub 0x{hub:02X}/PMIC 0x{p:02X}"),
                    None => format!("hub 0x{hub:02X}"),
                },
            })
            .collect();
        let status = format!(
            "{where_}: {} ({}).",
            if matches!(modules[0].kind, Kind::Ddr4 { .. }) {
                "DDR4 thermal sensors"
            } else {
                "DDR5 SPD hubs and PMICs"
            },
            found.join(", ")
        );
        Some(Dimms { bus, modules, measure_adc, status })
    }

    fn read_ddr5(&self, s: &Session, m: &Module, hub: u8, pmic: Option<u8>, out: &mut Batch) {
        let dev = &m.device.id;
        let c = m.controller;
        let temp = match (s.read_ddr5_byte(c, hub, HUB_TEMPERATURE), s.read_ddr5_byte(c, hub, HUB_TEMPERATURE + 1)) {
            (Ok(lo), Ok(hi)) => Some(decode_hub_temperature(lo, hi)).filter(|t| (0.0..=125.0).contains(t)),
            _ => None,
        };
        out.put(
            &SensorInfo::new(dev, "temp", Group::Temperatures, "SPD Hub Temperature", "°C", 2)
                .source(format!("SPD hub 0x{hub:02X} MR49/MR50")),
            temp.unwrap_or(f64::NAN),
        );
        let Some(p) = pmic else { return };
        let mut volts = [f64::NAN; 3];
        if self.measure_adc {
            let original = s.read_ddr5_byte(c, p, crate::sources::smbus::PMIC_ADC_SELECT).ok();
            for (i, (key, label, channel, step, max)) in ADC_CHANNELS.iter().enumerate() {
                let mv = (|| {
                    s.write_pmic_adc_select(c, p, adc_select_byte(*channel)).ok()?;
                    std::thread::sleep(ADC_SETTLE);
                    let _ = s.read_ddr5_byte(c, p, ADC_VALUE);
                    let raw = s.read_ddr5_byte(c, p, ADC_VALUE).ok()?;
                    decode_adc_mv(raw, *step, *max)
                })();
                let v = mv.map_or(f64::NAN, |mv| mv / 1000.0);
                if i < 3 {
                    volts[i] = v;
                }
                out.put(
                    &SensorInfo::new(dev, format!("volt/{key}"), Group::Voltages, *label, "V", 3)
                        .source(format!("PMIC 0x{p:02X} ADC channel {channel}, {step} mV per count")),
                    v,
                );
            }
            // Leave the multiplexer where it was found.
            if let Some(o) = original {
                let _ = s.write_pmic_adc_select(c, p, o);
            }
        }
        // Power needs each rail's voltage: the ADC's measurement first, the VID setting for any
        // rail it did not give.
        if let Ok(mode) = s.read_ddr5_byte(c, p, 0x2B) {
            for (i, (_, reg, base, bit)) in VID_RAILS.iter().enumerate() {
                if volts[i].is_nan()
                    && let Ok(raw) = s.read_ddr5_byte(c, p, *reg)
                {
                    volts[i] = decode_vid(raw, *base, bit.is_some_and(|b| mode & b != 0));
                }
            }
        }
        let regs: Vec<(u8, u8)> = [0x0C, 0x0E, 0x0F, 0x1B, 0x1A, 0x20, 0x29]
            .into_iter()
            .filter_map(|r| s.read_ddr5_byte(c, p, r).ok().map(|v| (r, v)))
            .collect();
        // In current mode a rail with no voltage makes the total NaN, which shows as no reading.
        let power = decode_power(&regs, &volts).filter(|w| w.is_finite() && (0.0..=50.0).contains(w));
        out.put(
            &SensorInfo::new(dev, "power/total", Group::Power, "Total Power", "W", 3)
                .source(format!("PMIC 0x{p:02X} telemetry registers 0x0C-0x0F")),
            power.unwrap_or(f64::NAN),
        );
    }
}

impl Source for Dimms {
    fn name(&self) -> &str {
        "DIMM sensors"
    }

    fn poll(&mut self, out: &mut Batch) {
        out.status(self.status.clone());
        for m in &self.modules {
            out.device(m.device.clone());
            // One acquisition per module: its reads depend on each other (an ADC select and the
            // sample it produces), so another tool must not land in between.
            let Some(s) = self.bus.session() else { continue };
            match m.kind {
                Kind::Ddr4 { tsod } => {
                    let t = s
                        .read_thermal_word(m.controller, tsod, TSOD_AMBIENT)
                        .ok()
                        .and_then(|(hi, lo)| decode_tsod(hi, lo));
                    out.put(
                        &SensorInfo::new(&m.device.id, "temp", Group::Temperatures, "DIMM Temperature", "°C", 1)
                            .source(format!("JC-42.4 sensor 0x{tsod:02X}, ambient register 0x05")),
                        t.unwrap_or(f64::NAN),
                    );
                }
                Kind::Ddr5 { hub, pmic } => self.read_ddr5(&s, m, hub, pmic, out),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_kits_thermal_sensors_decode() {
        // Read off this machine's two G.Skill modules at 0x19 and 0x1B: 0xC25C and 0xC268, alarm
        // flags set in the top bits.
        assert_eq!(decode_tsod(0xC2, 0x5C), Some(37.75));
        assert_eq!(decode_tsod(0xC2, 0x68), Some(38.5));
        assert_eq!(decode_tsod(0x1F, 0xF0), Some(-1.0));
    }

    #[test]
    fn hub_temperatures_decode_in_quarter_degrees() {
        // 35.25 °C = 141 quarters, from bit 2.
        let raw = 141u16 << 2;
        assert_eq!(decode_hub_temperature(raw as u8, (raw >> 8) as u8), 35.25);
    }

    #[test]
    fn adc_and_vid_follow_jesd301() {
        assert_eq!(adc_select_byte(0x5), 0x80 | (5 << 3));
        assert_eq!(decode_adc_mv(74, 15.0, 3000.0), Some(1110.0));
        assert_eq!(decode_adc_mv(0, 15.0, 3000.0), None);
        assert_eq!(decode_adc_mv(255, 15.0, 3000.0), None);
        assert!((decode_vid(0x46 << 1, 800.0, false) - 1.15).abs() < 1e-9);
        assert!((decode_vid(70, 800.0, true) - 1.15).abs() < 1e-9);
    }

    #[test]
    fn power_in_current_mode_matches_zentimings() {
        // Limits 6000/6000/1250 mA, raws 3/1/0 -> 0.2405 W against ZenTimings' 0.242-0.276 W.
        let regs = vec![(0x0C, 3), (0x0E, 1), (0x0F, 0), (0x1B, 0), (0x1A, 0), (0x20, 0b1100_1111), (0x29, 0)];
        let w = decode_power(&regs, &[1.46, 1.46, 1.8]).unwrap();
        assert!((w - (3.0 * 6.0 / 256.0 * 1.46 + 1.0 * 6.0 / 64.0 * 1.46)).abs() < 1e-9);
        assert!((w - 0.2405).abs() < 0.005);
        // Power mode: eighths of a watt, straight.
        let regs = vec![(0x0C, 16), (0x0E, 2), (0x0F, 1), (0x1B, 1 << 6), (0x1A, 0)];
        assert_eq!(decode_power(&regs, &[f64::NAN; 3]), Some(2.375));
    }

    #[test]
    fn pmic_identity_decodes() {
        assert_eq!(pmic_vendor(0x8A, 0x8C), "Richtek Power");
        assert_eq!(pmic_revision(0x12), "2.1");
    }
}
