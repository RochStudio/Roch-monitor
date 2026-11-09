//! Everything the monitor is showing, as text or JSON: the command line's `--dump`, and the
//! window's Save report (HWMonitor's "Save monitoring data").

use std::fmt::Write as _;

use roch_sensors::engine::State;
use roch_sensors::model::{Group, format_value};
use roch_sensors::system::SystemInfo;

pub fn hms(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

/// Sensors of one device, grouped and in declaration order, parents before their children.
pub fn device_rows(state: &State, device: &str) -> Vec<(Group, Vec<usize>)> {
    let mut groups: Vec<(Group, Vec<usize>)> = Vec::new();
    for (i, row) in state.sensors.iter().enumerate() {
        if row.info.device != device {
            continue;
        }
        match groups.iter_mut().find(|(g, _)| *g == row.info.group) {
            Some((_, v)) => v.push(i),
            None => groups.push((row.info.group, vec![i])),
        }
    }
    groups.sort_by_key(|(g, _)| *g);
    // Children directly after their parent.
    for (_, v) in &mut groups {
        let mut ordered = Vec::with_capacity(v.len());
        for &i in v.iter().filter(|&&i| state.sensors[i].info.parent.is_none()) {
            ordered.push(i);
            let key = &state.sensors[i].info.key;
            ordered.extend(v.iter().copied().filter(|&c| state.sensors[c].info.parent.as_deref() == Some(key)));
        }
        // Orphans (a child whose parent is in another group) go last rather than vanish.
        for &i in v.iter() {
            if !ordered.contains(&i) {
                ordered.push(i);
            }
        }
        *v = ordered;
    }
    groups
}

pub fn text(state: &State, sys: &SystemInfo) -> String {
    let mut s = String::new();
    let now = chrono::Local::now();
    let _ = writeln!(s, "Roch Monitor {} — {}", env!("CARGO_PKG_VERSION"), now.format("%Y-%m-%d %H:%M:%S"));
    let _ = writeln!(s, "{}", "=".repeat(100));
    let _ = writeln!(s, "Computer   {}", sys.computer);
    let _ = writeln!(s, "OS         {}", sys.os);
    let _ = writeln!(s, "CPU        {}", sys.cpu_name());
    let _ = writeln!(s, "Board      {}", sys.board_name());
    if !sys.smbios.bios_version.is_empty() {
        let _ =
            writeln!(s, "BIOS       {} {} ({})", sys.smbios.bios_vendor, sys.smbios.bios_version, sys.smbios.bios_date);
    }
    let _ = writeln!(s, "Memory     {}", sys.memory_summary());
    let _ =
        writeln!(s, "Running    {} since the statistics were last reset", hms(state.stats_since.elapsed().as_secs()));
    let _ = writeln!(s);
    let _ = writeln!(s, "{:<44}{:>14}{:>14}{:>14}{:>14}", "Sensor", "Current", "Minimum", "Maximum", "Average");
    for d in state.ordered_devices() {
        let rows = device_rows(state, &d.id);
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(s);
        let detail = if d.detail.is_empty() { String::new() } else { format!(" — {}", d.detail) };
        let _ = writeln!(s, "[{}] {}{}", d.kind.tag(), d.name, detail);
        for (group, list) in rows {
            let _ = writeln!(s, "  {}", group.title());
            for i in list {
                let r = &state.sensors[i];
                let (u, dp) = (r.info.unit.as_str(), r.info.decimals);
                let indent = if r.info.parent.is_some() { "      " } else { "    " };
                let mut label = format!("{indent}{}", r.info.label);
                if label.chars().count() > 43 {
                    label = label.chars().take(42).collect::<String>() + "…";
                }
                let _ = writeln!(
                    s,
                    "{:<44}{:>14}{:>14}{:>14}{:>14}",
                    label,
                    format_value(r.stat.current, u, dp),
                    format_value(r.stat.minimum, u, dp),
                    format_value(r.stat.maximum, u, dp),
                    format_value(r.stat.average(), u, dp)
                );
            }
        }
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "Sources");
    for src in &state.sources {
        let took =
            if src.polls > 0 { format!(" [{:.1} ms]", src.last_poll.as_secs_f64() * 1000.0) } else { String::new() };
        let _ = writeln!(s, "  {:<14}{}{}", src.name, src.status, took);
    }
    s
}

pub fn json(state: &State, sys: &SystemInfo) -> serde_json::Value {
    let num = |v: f64| if v.is_finite() { serde_json::json!(v) } else { serde_json::Value::Null };
    let devices: Vec<serde_json::Value> = state
        .ordered_devices()
        .into_iter()
        .map(|d| {
            let sensors: Vec<serde_json::Value> = device_rows(state, &d.id)
                .into_iter()
                .flat_map(|(_, list)| list)
                .map(|i| {
                    let r = &state.sensors[i];
                    serde_json::json!({
                        "key": r.info.key,
                        "label": r.info.label,
                        "group": r.info.group.title(),
                        "unit": r.info.unit,
                        "current": num(r.stat.current),
                        "minimum": num(r.stat.minimum),
                        "maximum": num(r.stat.maximum),
                        "average": num(r.stat.average()),
                        "parent": r.info.parent,
                        "source": r.info.source,
                    })
                })
                .collect();
            serde_json::json!({"id": d.id, "kind": d.kind.tag(), "name": d.name, "detail": d.detail, "sensors": sensors})
        })
        .collect();
    let sources: Vec<serde_json::Value> = state
        .sources
        .iter()
        .map(|s| serde_json::json!({"name": s.name, "status": s.status, "polls": s.polls, "last_poll_ms": s.last_poll.as_secs_f64() * 1000.0}))
        .collect();
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "computer": sys.computer,
        "os": sys.os,
        "cpu": sys.cpu_name(),
        "board": sys.board_name(),
        "memory": sys.memory_summary(),
        "running_seconds": state.stats_since.elapsed().as_secs(),
        "devices": devices,
        "sources": sources,
    })
}
