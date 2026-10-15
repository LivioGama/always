//! The whole apply chain, for real, on macOS: a release signed with the
//! `minisign` CLI (throwaway key, trusted comment written exactly like the
//! release workflow writes it) is served by a mock GitHub; the updater
//! downloads it, verifies every file, and the REAL `scripts/release/install.sh`
//! installs the verified archive into a temp directory. Nothing outside the
//! temp directory is touched (no /Applications, no running app).
//!
//! Skipped (with a note) when the `minisign` CLI is not installed, and off
//! macOS arm64: the Linux installer path restarts the user's systemd service,
//! which a test must never do.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use always::always::updater::apply::{
    self, Context, Downloader, HostProbe, Os, plan_install, run_installer_wait,
};
use always::always::updater::release::{Endpoints, Release};
use always::always::updater::verify::Verifier;
use semver::Version;
use sha2::{Digest, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TAG: &str = "v9.9.9";
const TARGET: &str = "aarch64-apple-darwin";

fn minisign_available() -> bool {
    Command::new("minisign")
        .arg("-v")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `minisign -S` with the release workflow's trusted comment.
fn sign(key: &Path, file: &Path) {
    let name = file.file_name().unwrap().to_string_lossy().to_string();
    let comment = format!("timestamp:1\tfile:{name}\ttag:{TAG}");
    let status = Command::new("minisign")
        .arg("-S")
        .arg("-s")
        .arg(key)
        .arg("-m")
        .arg(file)
        .arg("-x")
        .arg(format!("{}.minisig", file.display()))
        .arg("-t")
        .arg(comment)
        .stdin(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn signed_release_is_downloaded_verified_and_installed_by_the_real_installer() {
    if !minisign_available() {
        eprintln!("skipped: minisign CLI not installed");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let w = work.path();

    // A throwaway unencrypted key, as `minisign -G -W` makes.
    let (key, pubkey) = (w.join("k.key"), w.join("k.pub"));
    let status = Command::new("minisign")
        .args(["-G", "-W", "-f", "-s"])
        .arg(&key)
        .arg("-p")
        .arg(&pubkey)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());

    // The "release": an Always.app archive + the real installer, all signed.
    let release_dir = w.join("release");
    let name = format!("always-{TAG}-{TARGET}");
    let macos = release_dir
        .join("src")
        .join(&name)
        .join("Always.app/Contents/MacOS");
    fs::create_dir_all(&macos).unwrap();
    fs::write(macos.join("always-daemon"), "#!/bin/sh\necho 9.9.9\n").unwrap();
    let archive = release_dir.join(format!("{name}.tar.gz"));
    assert!(
        Command::new("tar")
            .env("COPYFILE_DISABLE", "1")
            .arg("czf")
            .arg(&archive)
            .arg("-C")
            .arg(release_dir.join("src"))
            .arg(&name)
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        format!("{}.sha256", archive.display()),
        format!(
            "{}  {name}.tar.gz\n",
            hex(&Sha256::digest(fs::read(&archive).unwrap()))
        ),
    )
    .unwrap();
    let installer = release_dir.join("install.sh");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/release/install.sh"),
        &installer,
    )
    .unwrap();
    sign(&key, &archive);
    sign(&key, &installer);

    // Mock GitHub serving those files.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(MockServer::start());
    let mut assets = Vec::new();
    for file in [
        format!("{name}.tar.gz"),
        format!("{name}.tar.gz.minisig"),
        format!("{name}.tar.gz.sha256"),
        "install.sh".to_string(),
        "install.sh.minisig".to_string(),
    ] {
        let body = fs::read(release_dir.join(&file)).unwrap();
        rt.block_on(
            Mock::given(method("GET"))
                .and(path(format!("/{TAG}/{file}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
                .mount(&server),
        );
        assets.push(file);
    }
    let release = Release {
        tag: TAG.to_string(),
        version: Version::new(9, 9, 9),
        html_url: String::new(),
        assets,
    };

    // An "installed" Always.app to update: the plan is derived from where the
    // running binary lives, exactly as in production.
    let apps: PathBuf = w.join("Applications");
    let old_bin = apps.join("Always.app/Contents/MacOS/always-daemon");
    fs::create_dir_all(old_bin.parent().unwrap()).unwrap();
    fs::write(&old_bin, "#!/bin/sh\necho 0.1.0\n").unwrap();
    let plan = plan_install(Os::Mac, TARGET, &old_bin, Context::Cli, "prod", &HostProbe)
        .expect("a writable Always.app is updatable");

    let endpoints = Endpoints {
        api_latest: String::new(),
        download_base: server.uri(),
        allow_http: true,
    };
    let verifier = Verifier::from_key_text(&fs::read_to_string(&pubkey).unwrap()).unwrap();
    let downloader = Downloader::new(&endpoints).unwrap();
    let staged = apply::stage_in(
        &w.join("update"),
        &release,
        &plan,
        &verifier,
        &downloader,
        &mut |_| {},
    )
    .expect("the correctly signed release stages");

    let status = run_installer_wait(&staged, &plan, TAG).unwrap();
    assert!(status.success(), "installer failed: {status}");
    assert_eq!(
        fs::read_to_string(&old_bin).unwrap(),
        "#!/bin/sh\necho 9.9.9\n",
        "the app was replaced by the verified archive's contents"
    );
}
