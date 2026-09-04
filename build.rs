//! Stamp the daemon binary with the current git short SHA so
//! `always --version` and runtime tracing logs both report the exact
//! revision a user is running. The Mac app reads `--version` at startup
//! and compares against its bundled `Info.plist` to detect drift.
//!
//! On macOS ARM64 we also compile the Apple Intelligence Swift bridge
//! (FoundationModels framework) into a static library and link it into
//! the daemon binary, following the same pattern as Handy.

use std::path::{Path, PathBuf};
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

    // Compile Apple Intelligence Swift bridge on macOS ARM64.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    build_apple_intelligence_bridge();
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn build_apple_intelligence_bridge() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let swift_dir = manifest_dir.join("swift");
    let real_swift_path = swift_dir.join("apple_intelligence.swift");
    let stub_swift_path = swift_dir.join("apple_intelligence_stub.swift");
    let bridge_header_path = swift_dir.join("apple_intelligence_bridge.h");
    let real_swift_file = real_swift_path.to_str().unwrap();
    let stub_swift_file = stub_swift_path.to_str().unwrap();
    let bridge_header = bridge_header_path.to_str().unwrap();

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    let object_path = out_dir.join("apple_intelligence.o");
    let static_lib_path = out_dir.join("libapple_intelligence.a");

    // SDKROOT override for non-Xcode toolchains.
    let sdk_path = std::env::var("SDKROOT").unwrap_or_else(|_| {
        String::from_utf8(
            Command::new("xcrun")
                .args(["--sdk", "macosx", "--show-sdk-path"])
                .output()
                .expect("Failed to locate macOS SDK")
                .stdout,
        )
        .expect("SDK path is not valid UTF-8")
        .trim()
        .to_string()
    });

    // Check if the SDK supports FoundationModels.
    let framework_path =
        Path::new(&sdk_path).join("System/Library/Frameworks/FoundationModels.framework");
    let force_stub = std::env::var("ALWAYS_FORCE_AI_STUB").as_deref() == Ok("1");

    // Auto-detect Command-Line-Tools-only toolchain — CLT has the framework
    // but no FoundationModelsMacros plugin (full Xcode only).
    let command_line_tools_only = std::env::var("SWIFTC").is_err() && is_command_line_tools_only();
    if command_line_tools_only && !force_stub {
        println!(
            "cargo:warning=Command Line Tools-only toolchain detected; Apple Intelligence \
             (FoundationModels) needs full Xcode. Falling back to stubs."
        );
    }

    let has_foundation_models = framework_path.exists() && !force_stub && !command_line_tools_only;

    let source_file = if has_foundation_models {
        println!("cargo:warning=Building with Apple Intelligence support.");
        real_swift_file
    } else {
        if framework_path.exists() {
            println!("cargo:warning=Building Apple Intelligence with stubs.");
        } else {
            println!("cargo:warning=Apple Intelligence SDK not found. Building with stubs.");
        }
        stub_swift_file
    };

    println!("cargo:rerun-if-changed={source_file}");
    println!("cargo:rerun-if-changed={bridge_header}");

    let swiftc_path = std::env::var("SWIFTC").unwrap_or_else(|_| {
        String::from_utf8(
            Command::new("xcrun")
                .args(["--find", "swiftc"])
                .output()
                .expect("Failed to locate swiftc")
                .stdout,
        )
        .expect("swiftc path is not valid UTF-8")
        .trim()
        .to_string()
    });

    let toolchain_swift_lib = Path::new(&swiftc_path)
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("lib/swift/macosx"))
        .expect("Unable to determine Swift toolchain lib directory");
    let sdk_swift_lib = Path::new(&sdk_path).join("usr/lib/swift");

    let status = Command::new(&swiftc_path)
        .args([
            "-parse-as-library",
            "-target",
            "arm64-apple-macosx11.0",
            "-sdk",
            &sdk_path,
            "-O",
            "-import-objc-header",
            bridge_header,
            "-c",
            source_file,
            "-o",
            object_path.to_str().expect("Failed to convert object path"),
        ])
        .status()
        .expect("Failed to invoke swiftc for Apple Intelligence bridge");

    if !status.success() {
        panic!("swiftc failed to compile {source_file}");
    }

    let status = Command::new("libtool")
        .args([
            "-static",
            "-o",
            static_lib_path.to_str().expect("Failed to convert static lib path"),
            object_path.to_str().expect("Failed to convert object path"),
        ])
        .status()
        .expect("Failed to create static library for Apple Intelligence bridge");

    if !status.success() {
        panic!("libtool failed for Apple Intelligence bridge");
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=apple_intelligence");
    println!("cargo:rustc-link-search=native={}", toolchain_swift_lib.display());
    println!("cargo:rustc-link-search=native={}", sdk_swift_lib.display());
    println!("cargo:rustc-link-lib=framework=Foundation");

    if has_foundation_models {
        // Weak link so the app launches on systems without FoundationModels.
        println!("cargo:rustc-link-arg=-weak_framework");
        println!("cargo:rustc-link-arg=FoundationModels");
    }

    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn is_command_line_tools_only() -> bool {
    Command::new("xcode-select")
        .arg("-p")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|path| path.trim().ends_with("CommandLineTools"))
        .unwrap_or(false)
}
