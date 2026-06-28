//! Linker setup for esp-hal firmware.
//!
//! esp-hal does NOT inject its linker script automatically — each firmware crate
//! must add `-Tlinkall.x` itself (it provides the ESP32-S3 memory layout). Without
//! this, the binary links at a default/garbage layout and the chip aborts at boot.
fn main() {
    // Give a friendlier message for common link errors (see `linker_be_nice`).
    linker_be_nice();

    // Fail early with actionable advice if the Xtensa linker isn't on PATH.
    check_xtensa_linker_available();

    // linkall.x must be the LAST linker script.
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

#[cfg(unix)]
fn check_xtensa_linker_available() {
    println!("cargo:rerun-if-env-changed=PATH");

    let target = std::env::var("TARGET").unwrap_or_default();
    let linker = target
        .strip_prefix("xtensa-")
        .and_then(|t| t.strip_suffix("-none-elf"))
        .map(|chip| format!("xtensa-{chip}-elf-gcc"))
        .unwrap_or_else(|| "xtensa-esp32-elf-gcc".to_string());

    if std::process::Command::new(&linker)
        .arg("--version")
        .output()
        .is_ok()
    {
        return;
    }

    let export_file = std::env::var("HOME")
        .map(|home| format!("{home}/export-esp.sh"))
        .unwrap_or_else(|_| "$HOME/export-esp.sh".to_string());

    panic!(
        "Xtensa linker `{linker}` was not found in PATH.\n\n\
         Source espup's environment first:  source {export_file}\n\
         (the project Makefile does this for you)."
    );
}

#[cfg(not(unix))]
fn check_xtensa_linker_available() {}

/// Cargo calls this build script as the linker "error handling script" with
/// extra args when a link fails; turn cryptic errors into hints.
fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];
        if kind == "undefined-symbol" && what == "_stack_start" {
            eprintln!("\n💡 Is the linker script `linkall.x` missing?\n");
        }
        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
