//! Stamp the daemon binary with the current git short SHA so
//! `always --version` and runtime tracing logs both report the exact
//! revision a user is running. The Mac app reads `--version` at startup
//! and compares against its bundled `Info.plist` to detect drift.
//!
//! NOTE: Swift compilation for Apple Intelligence / Apple STT bridges has
//! been removed. The Swift GUI (Always/) is deprecated — see DEPRECATED.md.
//! Audio capture now uses the cross-platform cpal backend on all platforms.

#[allow(unused_imports)]
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ALWAYS_BUILD_SHA={sha}");

    // Re-run when HEAD moves.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");

    // Embed an Info.plist in the daemon binary so TCC can find the
    // NSSpeechRecognitionUsageDescription / NSMicrophoneUsageDescription
    // keys when the binary runs outside the app bundle. Without this,
    // TCC crashes the process on Speech framework access.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let info_plist = manifest_dir.join("Always/Info.plist");
        if info_plist.exists() {
            println!(
                "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
                info_plist.display()
            );
        }
    }
}
