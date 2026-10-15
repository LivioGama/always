//! `scripts/release/install.sh` in self-update mode (`ALWAYS_ARCHIVE`): the
//! updater hands it an archive it has already signature-verified, and the
//! script must install that file WITHOUT touching the network, while still
//! checking the sibling `.sha256`. Runs the real script on the host
//! (macOS arm64 or Linux), installing into a temp dir; skipped elsewhere.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

const TAG: &str = "v9.9.9";

struct Host {
    prefix_name: String,
    mac: bool,
}

fn host() -> Option<Host> {
    let (target, mac) = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", true)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        ("x86_64-unknown-linux-gnu", false)
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-gnu", false)
    } else {
        return None;
    };
    Some(Host {
        prefix_name: format!("always-{TAG}-{target}"),
        mac,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/release/install.sh")
}

/// A fake release archive laid out like the real one, plus its `.sha256`.
fn make_archive(dir: &Path, host: &Host) -> PathBuf {
    let stage = dir.join("stage").join(&host.prefix_name);
    if host.mac {
        let macos = stage.join("Always.app/Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(macos.join("always-daemon"), "#!/bin/sh\necho updated\n").unwrap();
    } else {
        fs::create_dir_all(stage.join("bin")).unwrap();
        let bin = stage.join("bin/always");
        fs::write(&bin, "#!/bin/sh\necho always 9.9.9\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let archive = dir.join(format!("{}.tar.gz", host.prefix_name));
    let status = Command::new("tar")
        .arg("czf")
        .arg(&archive)
        .arg("-C")
        .arg(dir.join("stage"))
        .arg(&host.prefix_name)
        .env("COPYFILE_DISABLE", "1")
        .status()
        .unwrap();
    assert!(status.success());
    write_sha(&archive, &hex(&Sha256::digest(fs::read(&archive).unwrap())));
    archive
}

fn write_sha(archive: &Path, digest: &str) {
    let name = archive.file_name().unwrap().to_string_lossy();
    fs::write(
        format!("{}.sha256", archive.display()),
        format!("{digest}  {name}\n"),
    )
    .unwrap();
}

/// Run the installer in self-update mode. `curl` is replaced by a shim that
/// records any call and fails, to prove nothing is fetched.
fn run(dir: &Path, archive: Option<&Path>, version: Option<&str>) -> (Output, PathBuf, PathBuf) {
    let shim_dir = dir.join("shim");
    fs::create_dir_all(&shim_dir).unwrap();
    let curl_marker = dir.join("curl-was-called");
    let curl = shim_dir.join("curl");
    fs::write(
        &curl,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nexit 22\n",
            curl_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();

    let apps = dir.join("apps");
    let prefix = dir.join("prefix");
    let path = format!("{}:{}", shim_dir.display(), std::env::var("PATH").unwrap());
    let mut cmd = Command::new("bash");
    cmd.arg(script())
        .env("PATH", path)
        .env("HOME", dir.join("home"))
        .env("ALWAYS_APP_DIR", &apps)
        .env("ALWAYS_PREFIX", &prefix)
        .env("ALWAYS_METHOD", "tar")
        .env("ALWAYS_SYSTEMD", "0")
        .env("ALWAYS_NO_LAUNCH", "1")
        .env_remove("ALWAYS_BASE_URL")
        .env_remove("ALWAYS_UPDATER_PID");
    match archive {
        Some(a) => cmd.env("ALWAYS_ARCHIVE", a),
        None => cmd.env_remove("ALWAYS_ARCHIVE"),
    };
    match version {
        Some(v) => cmd.env("ALWAYS_VERSION", v),
        None => cmd.env_remove("ALWAYS_VERSION"),
    };
    let out = cmd.output().unwrap();
    (out, curl_marker, prefix)
}

fn installed(host: &Host, dir: &Path, prefix: &Path) -> PathBuf {
    if host.mac {
        dir.join("apps/Always.app/Contents/MacOS/always-daemon")
    } else {
        prefix.join("bin/always")
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn installs_the_given_archive_without_touching_the_network() {
    let Some(host) = host() else {
        eprintln!("skipped: no release target for this host");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path(), &host);
    let (out, curl_marker, prefix) = run(dir.path(), Some(&archive), Some(TAG));
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        installed(&host, dir.path(), &prefix).is_file(),
        "{}",
        text(&out)
    );
    assert!(
        !curl_marker.exists(),
        "installer touched the network: {}",
        text(&out)
    );
    assert!(text(&out).contains("already-verified"), "{}", text(&out));
}

#[test]
fn still_checks_the_sha256_sibling() {
    let Some(host) = host() else { return };
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path(), &host);
    write_sha(&archive, &"0".repeat(64));
    let (out, curl_marker, prefix) = run(dir.path(), Some(&archive), Some(TAG));
    assert!(!out.status.success());
    assert!(text(&out).contains("Checksum mismatch"), "{}", text(&out));
    assert!(!installed(&host, dir.path(), &prefix).exists());
    assert!(!curl_marker.exists());
}

#[test]
fn refuses_a_missing_checksum_file() {
    let Some(host) = host() else { return };
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path(), &host);
    fs::remove_file(format!("{}.sha256", archive.display())).unwrap();
    let (out, _, prefix) = run(dir.path(), Some(&archive), Some(TAG));
    assert!(!out.status.success());
    assert!(
        text(&out).contains("without its checksum"),
        "{}",
        text(&out)
    );
    assert!(!installed(&host, dir.path(), &prefix).exists());
}

#[test]
fn refuses_an_archive_named_for_another_platform() {
    let Some(host) = host() else { return };
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path(), &host);
    let wrong = dir
        .path()
        .join("always-v9.9.9-riscv64-unknown-linux-gnu.tar.gz");
    fs::copy(&archive, &wrong).unwrap();
    fs::copy(
        format!("{}.sha256", archive.display()),
        format!("{}.sha256", wrong.display()),
    )
    .unwrap();
    let (out, _, prefix) = run(dir.path(), Some(&wrong), Some(TAG));
    assert!(!out.status.success());
    assert!(text(&out).contains("this install needs"), "{}", text(&out));
    assert!(!installed(&host, dir.path(), &prefix).exists());
}

#[test]
fn archive_override_needs_a_version_and_an_existing_file() {
    let Some(host) = host() else { return };
    let dir = tempfile::tempdir().unwrap();
    let archive = make_archive(dir.path(), &host);
    let (out, _, _) = run(dir.path(), Some(&archive), None);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("needs ALWAYS_VERSION"),
        "{}",
        text(&out)
    );
    let (out, _, _) = run(dir.path(), Some(&dir.path().join("nope.tar.gz")), Some(TAG));
    assert!(!out.status.success());
    assert!(text(&out).contains("does not exist"), "{}", text(&out));
}
