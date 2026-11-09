//! Roch Monitor: one executable, two front ends. A flag such as `--dump` runs the command line on
//! the caller's console; no arguments opens the window.

// A windows-subsystem exe, like Roch GPU; the CLI borrows the caller's console instead.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod cli;
mod console;
mod elevation;
mod gui;
mod logger;
mod report;

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let code = entry(&args);
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code);
}

fn entry(args: &[String]) -> i32 {
    if cli::wants_command_line(args) {
        let _console = console::attach();
        return cli::run(args);
    }
    let elevated = roch_sensors::hw::is_elevated();
    let declined = args.iter().any(|a| a == "--no-elevate" || a == "--elevated");
    if !elevated && !declined && elevation::relaunch_elevated(args) {
        return 0;
    }
    gui::run(args)
}
