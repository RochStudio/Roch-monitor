//! The command line: one sample of everything, as a table or JSON, or a CSV log for a set time.

use std::io::Write;
use std::time::{Duration, Instant};

use roch_sensors::engine::Engine;
use roch_sensors::sources::{Options, factories};
use roch_sensors::system::SystemInfo;

use crate::{logger, report};

pub const HELP: &str = "\
Roch Monitor — live hardware sensors with current, minimum, maximum and average.

Usage:
  RochMonitor.exe                      open the window (asks for administrator rights)
  RochMonitor.exe --dump [options]     sample for a few seconds and print every sensor
  RochMonitor.exe --log FILE [options] write a CSV log for --seconds, then exit
  RochMonitor.exe --help | --version

Options for --dump and --log:
  --seconds N        how long to sample (default 3 for --dump, 60 for --log)
  --interval MS      polling interval in milliseconds (default 1000, minimum 250)
  --json             --dump prints JSON instead of a table
  --no-hardware      skip the driver-based sources (board, DIMMs, MCHBAR, EC)
  --no-gpu           skip the graphics backends
  --no-pmic-adc      read DDR5 PMIC settings but not the ADC (no selector writes)

Window options:
  --no-elevate       run the window without asking for administrator rights
  --settings [PAGE]  open with the Settings window showing, optionally at a page:
                     appearance, colours, text, layout, window or sensors

Board, DIMM and memory-controller sensors need the inpoutx64 driver (inpoutx64.dll and
inpoutx64.sys beside RochMonitor.exe) and administrator rights; everything else works without.
";

pub fn wants_command_line(args: &[String]) -> bool {
    args.iter().any(|a| matches!(a.as_str(), "--dump" | "--log" | "--help" | "-h" | "--version" | "/?"))
}

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

pub fn options(args: &[String]) -> Options {
    Options {
        hardware: !args.iter().any(|a| a == "--no-hardware"),
        gpu: !args.iter().any(|a| a == "--no-gpu"),
        pmic_adc: !args.iter().any(|a| a == "--no-pmic-adc"),
        ..Options::default()
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut out = std::io::stdout();
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h" | "/?")) {
        let _ = write!(out, "{HELP}");
        return 0;
    }
    if args.iter().any(|a| a == "--version") {
        let _ = writeln!(out, "Roch Monitor {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let logging = value(args, "--log");
    let seconds: u64 =
        value(args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(if logging.is_some() { 60 } else { 3 });
    let interval: u64 = value(args, "--interval").and_then(|s| s.parse().ok()).unwrap_or(1000);

    let sys = SystemInfo::detect();
    if !sys.elevated && options(args).hardware {
        let _ = writeln!(
            std::io::stderr(),
            "Note: not running as administrator, so the inpoutx64 driver will not open; board, DIMM and MCHBAR sensors will be missing."
        );
    }
    let mut engine = Engine::start(factories(&sys, options(args)), interval, 600);

    if let Some(path) = logging {
        // Let every source declare its rows before the columns are fixed.
        std::thread::sleep(Duration::from_millis(interval.max(1000) + 500));
        let mut log = match logger::CsvLogger::start(engine.shared().clone(), std::path::Path::new(path), &[]) {
            Ok(l) => l,
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "Could not open {path}: {e}");
                return 1;
            }
        };
        let _ = writeln!(out, "Logging to {path} for {seconds} s…");
        let end = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(200));
        }
        log.stop();
        let _ = writeln!(out, "Wrote {} rows.", log.rows());
        engine.stop();
        return 0;
    }

    std::thread::sleep(Duration::from_secs(seconds.max(1)));
    let text = {
        let state = engine.lock();
        if args.iter().any(|a| a == "--json") {
            serde_json::to_string_pretty(&report::json(&state, &sys)).unwrap_or_default()
        } else {
            report::text(&state, &sys)
        }
    };
    engine.stop();
    let _ = writeln!(out, "{text}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn flags_pick_the_front_end() {
        assert!(wants_command_line(&v(&["--dump"])));
        assert!(wants_command_line(&v(&["--log", "a.csv"])));
        assert!(!wants_command_line(&v(&[])));
        assert!(!wants_command_line(&v(&["--no-elevate"])));
    }

    #[test]
    fn options_follow_the_flags() {
        let o = options(&v(&["--dump", "--no-gpu", "--no-pmic-adc"]));
        assert!(o.hardware && !o.gpu && !o.pmic_adc);
    }
}
