//! Embeds the icon, version information and application manifest into RochMonitor.exe, as Roch GPU
//! does: Explorer shows the logo, and the process gets per-monitor DPI awareness.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/RochMonitor.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Load opengl32.dll on first use: only the window needs it, and merely having it mapped costs
    // a CLI run ~55 ms on exit (Roch GPU measured this).
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=RochMonitor=/DELAYLOAD:opengl32.dll");
        println!("cargo:rustc-link-arg-bin=RochMonitor=delayimp.lib");
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/RochMonitor.ico")
        .set_manifest_file("assets/app.manifest")
        .set("ProductName", "Roch Monitor")
        .set("FileDescription", "Roch Monitor")
        .set("LegalCopyright", "Copyright (c) 2026 Roch Studio. GPL-3.0-or-later.")
        .set("OriginalFilename", "RochMonitor.exe");
    if let Err(e) = res.compile() {
        // A missing resource compiler should not block a build; the exe just has no icon.
        println!("cargo:warning=could not embed Windows resources: {e}");
    }
}
