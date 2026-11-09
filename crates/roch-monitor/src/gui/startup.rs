//! Start with Windows (Roch GPU's `startup_task.rs`): a Task Scheduler entry, made with
//! schtasks.exe, that runs Roch Monitor at logon with the highest privileges it has, minimised to the
//! tray. A task rather than the Run key because the driver needs administrator rights, and a task
//! can start elevated without a UAC prompt at every logon.
//!
//! Only ever changed by the user's own tick in Settings.

pub const TASK_NAME: &str = "Roch Monitor";

/// The schtasks arguments that create the task for `exe`.
pub fn create_args(exe: &str) -> Vec<String> {
    let run = format!("\"{exe}\" --minimized");
    ["/Create", "/F", "/TN", TASK_NAME, "/SC", "ONLOGON", "/RL", "HIGHEST", "/DELAY", "0000:10", "/TR", &run]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[cfg(windows)]
fn run(args: &[String]) -> (i32, String) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new("schtasks").args(args).creation_flags(CREATE_NO_WINDOW).output() {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
            (out.status.code().unwrap_or(-1), text.trim().to_owned())
        }
        Err(e) => (-1, e.to_string()),
    }
}

#[cfg(not(windows))]
fn run(_args: &[String]) -> (i32, String) {
    (-1, "Start with Windows is a Windows feature.".into())
}

pub fn exists() -> bool {
    run(&["/Query".into(), "/TN".into(), TASK_NAME.into()]).0 == 0
}

/// Create the task for this executable, replacing any earlier one.
pub fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if exists() {
        run(&["/Delete".into(), "/F".into(), "/TN".into(), TASK_NAME.into()]);
    }
    let (code, out) = run(&create_args(&exe.display().to_string()));
    if code == 0 { Ok(()) } else { Err(format!("schtasks could not create the task: {out}")) }
}

/// Remove the task; removing one that was never made is success, and a delete that says it worked
/// while the task survives is not.
pub fn disable() -> Result<(), String> {
    if !exists() {
        return Ok(());
    }
    let (code, out) = run(&["/Delete".into(), "/F".into(), "/TN".into(), TASK_NAME.into()]);
    if code == 0 && !exists() {
        Ok(())
    } else {
        Err(format!("schtasks could not remove the task (exit {code}): {out}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_runs_elevated_at_logon_and_starts_in_the_tray() {
        let args = create_args(r"C:\Tools\Roch Monitor\RochMonitor.exe");
        let joined = args.join(" ");
        assert!(joined.contains("/SC ONLOGON"));
        assert!(joined.contains("/RL HIGHEST"));
        assert_eq!(args.last().unwrap(), r#""C:\Tools\Roch Monitor\RochMonitor.exe" --minimized"#);
        assert_eq!(args[args.iter().position(|a| a == "/TN").unwrap() + 1], TASK_NAME);
    }
}
