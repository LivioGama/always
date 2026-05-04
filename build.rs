//! Stamp the daemon binary with the current git short SHA so
//! `always --version` and runtime tracing logs both report the exact
//! revision a user is running. The Mac app reads `--version` at startup
//! and compares against its bundled `Info.plist` to detect drift.

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

    // Re-run when HEAD moves. `.git/HEAD` is the cheapest signal we can
    // expose. Avoid `cargo:rerun-if-changed=.` — that would invalidate
    // the build on every source edit.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
}
