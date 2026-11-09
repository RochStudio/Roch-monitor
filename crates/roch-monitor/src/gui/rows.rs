//! The list as drawn: device headings, group headings and sensor rows, flattened, with folding,
//! search and hidden sensors applied. Toolkit-free, like Roch GPU's `telemetry_table`, so the rules
//! can be tested without a window.
//!
//! Every row is the same height, so the scroll area only lays out what is on screen.

use roch_sensors::engine::State;
use roch_sensors::model::DeviceKind;

use crate::report::device_rows;

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Device { id: String, kind: DeviceKind, name: String, detail: String, collapsed: bool, sensors: usize },
    Group { title: &'static str },
    Sensor { index: usize, depth: u8, children: usize, expanded: bool, hidden: bool },
}

pub struct View<'a> {
    pub search: &'a str,
    pub hidden: &'a [String],
    pub show_hidden: bool,
    pub collapsed: &'a [String],
    pub expanded: &'a [String],
    pub group_headings: bool,
}

fn matches(needle: &str, hay: &str) -> bool {
    hay.to_lowercase().contains(needle)
}

pub fn build(state: &State, v: &View) -> Vec<Row> {
    let needle = v.search.trim().to_lowercase();
    let searching = !needle.is_empty();
    let mut rows = Vec::new();
    for d in state.ordered_devices() {
        let device_hit =
            searching && (matches(&needle, &d.name) || matches(&needle, d.kind.tag()) || matches(&needle, &d.detail));
        let groups = device_rows(state, &d.id);
        let mut body = Vec::new();
        let mut count = 0usize;
        for (group, list) in groups {
            let group_hit = searching && matches(&needle, group.title());
            let mut entries = Vec::new();
            for &i in &list {
                let r = &state.sensors[i];
                let hidden = v.hidden.contains(&r.info.key);
                if hidden && !v.show_hidden {
                    continue;
                }
                let children: Vec<usize> = list
                    .iter()
                    .copied()
                    .filter(|&c| state.sensors[c].info.parent.as_deref() == Some(r.info.key.as_str()))
                    .collect();
                let hit = !searching
                    || device_hit
                    || group_hit
                    || matches(&needle, &r.info.label)
                    || matches(&needle, &r.info.key)
                    || children.iter().any(|&c| matches(&needle, &state.sensors[c].info.label));
                match r.info.parent.as_deref() {
                    Some(parent) => {
                        let parent_open = v.expanded.iter().any(|e| e == parent);
                        let parent_present = list.iter().any(|&p| state.sensors[p].info.key == parent);
                        // A child shows when its parent is open, or when a search names it.
                        let shown = if searching {
                            matches(&needle, &r.info.label) || device_hit || group_hit
                        } else {
                            parent_open || !parent_present
                        };
                        if shown {
                            entries.push(Row::Sensor { index: i, depth: 1, children: 0, expanded: false, hidden });
                        }
                        count += 1;
                    }
                    None => {
                        if hit {
                            entries.push(Row::Sensor {
                                index: i,
                                depth: 0,
                                children: children.len(),
                                expanded: v.expanded.contains(&r.info.key),
                                hidden,
                            });
                        }
                        count += 1;
                    }
                }
            }
            if entries.is_empty() {
                continue;
            }
            if v.group_headings {
                body.push(Row::Group { title: group.title() });
            }
            body.extend(entries);
        }
        if body.is_empty() {
            continue;
        }
        let collapsed = !searching && v.collapsed.contains(&d.id);
        rows.push(Row::Device {
            id: d.id.clone(),
            kind: d.kind,
            name: d.name.clone(),
            detail: d.detail.clone(),
            collapsed,
            sensors: count,
        });
        if !collapsed {
            rows.extend(body);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use roch_sensors::engine::{Batch, Engine, Source};
    use roch_sensors::model::{DeviceInfo, Group, SensorInfo};
    use std::time::{Duration, Instant};

    struct Fixture;

    impl Source for Fixture {
        fn name(&self) -> &str {
            "fixture"
        }
        fn poll(&mut self, out: &mut Batch) {
            out.device(DeviceInfo::new("cpu", DeviceKind::Cpu, "Test CPU"));
            out.device(DeviceInfo::new("board", DeviceKind::Board, "Test Board"));
            let eff = SensorInfo::new("cpu", "clk/eff", Group::Clocks, "Core Effective Clock", "MHz", 0);
            out.put(&eff, 100.0);
            out.put(
                &SensorInfo::new("cpu", "clk/eff0", Group::Clocks, "Core 0 Effective Clock", "MHz", 0)
                    .parent("clk/eff"),
                50.0,
            );
            out.put(&SensorInfo::new("cpu", "temp/max", Group::Temperatures, "Core Max", "°C", 1), 40.0);
            out.put(&SensorInfo::new("board", "fan/cpu", Group::Fans, "CPU Fan", "RPM", 0), 900.0);
        }
    }

    fn state_with_fixture() -> Engine {
        let engine = Engine::start(
            vec![("f".into(), Box::new(|_: &mut Batch| Some(Box::new(Fixture) as Box<dyn Source>)))],
            250,
            8,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.lock().sensors.len() < 4 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        engine
    }

    fn view<'a>(search: &'a str, collapsed: &'a [String], expanded: &'a [String], hidden: &'a [String]) -> View<'a> {
        View { search, hidden, show_hidden: false, collapsed, expanded, group_headings: true }
    }

    fn sensor_labels(state: &State, rows: &[Row]) -> Vec<String> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Sensor { index, .. } => Some(state.sensors[*index].info.label.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn devices_group_and_fold() {
        let engine = state_with_fixture();
        let st = engine.lock();
        let rows = build(&st, &view("", &[], &[], &[]));
        assert!(matches!(&rows[0], Row::Device { id, .. } if id == "cpu"));
        assert!(matches!(rows[1], Row::Group { title: "TEMPERATURES" }));
        // The per-thread child is folded under its parent until it is opened.
        assert_eq!(sensor_labels(&st, &rows), vec!["Core Max", "Core Effective Clock", "CPU Fan"]);
        let open = vec!["cpu/clk/eff".to_string()];
        let rows = build(&st, &view("", &[], &open, &[]));
        assert_eq!(
            sensor_labels(&st, &rows),
            vec!["Core Max", "Core Effective Clock", "Core 0 Effective Clock", "CPU Fan"]
        );
        let collapsed = vec!["cpu".to_string()];
        let rows = build(&st, &view("", &collapsed, &[], &[]));
        assert_eq!(sensor_labels(&st, &rows), vec!["CPU Fan"]);
    }

    #[test]
    fn search_finds_folded_children_and_hidden_rows_stay_hidden() {
        let engine = state_with_fixture();
        let st = engine.lock();
        // The child a search names shows even folded, with its parent above it for context.
        let rows = build(&st, &view("core 0", &[], &[], &[]));
        assert_eq!(sensor_labels(&st, &rows), vec!["Core Effective Clock", "Core 0 Effective Clock"]);
        let hidden = vec!["board/fan/cpu".to_string()];
        let rows = build(&st, &view("", &[], &[], &hidden));
        assert!(!sensor_labels(&st, &rows).contains(&"CPU Fan".to_string()));
        assert!(
            !rows.iter().any(|r| matches!(r, Row::Device { id, .. } if id == "board")),
            "an empty device is not shown"
        );
    }
}
