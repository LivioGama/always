//! End-to-end tests of the updater's network halves against a local mock of
//! GitHub: the release check (`fetch_latest`) and download + verify
//! (`stage_in`). The signatures are real minisign signatures made with a
//! throwaway key (tests/fixtures/update: `test.pub`, `other.pub` and a fake
//! v9.9.9 release; the secret keys were discarded). Each fixture signature's
//! trusted comment is `timestamp:1<TAB>file:<name><TAB>tag:v9.9.9`, written
//! with `minisign -S -t`. The production key is never involved.

use std::fs;
use std::path::Path;

use always::always::updater::apply::{
    self, Downloader, InstallPlan, Method, Os, Progress, installer_command,
};
use always::always::updater::release::{self, CheckOutcome, Endpoints, Release};
use always::always::updater::verify::Verifier;
use semver::Version;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TAG: &str = "v9.9.9";
const TARGET: &str = "x86_64-unknown-linux-gnu";
const ARCHIVE: &str = "always-v9.9.9-x86_64-unknown-linux-gnu.tar.gz";
const TEST_PUB: &str = include_str!("fixtures/update/test.pub");
const OTHER_PUB: &str = include_str!("fixtures/update/other.pub");

fn fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/update/release")
        .join(name);
    fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// A mock GitHub on its own multi-thread runtime, so the blocking client
/// under test can run on the plain test thread.
struct Github {
    rt: tokio::runtime::Runtime,
    server: MockServer,
}

impl Github {
    fn start() -> Self {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        Self { rt, server }
    }

    fn serve(&self, url_path: &str, status: u16, body: Vec<u8>) {
        self.rt.block_on(
            Mock::given(method("GET"))
                .and(path(url_path.to_string()))
                .respond_with(ResponseTemplate::new(status).set_body_bytes(body))
                .mount(&self.server),
        );
    }

    /// Serve the fake v9.9.9 release exactly as published.
    fn serve_release(&self) {
        for name in [
            ARCHIVE,
            &format!("{ARCHIVE}.minisig"),
            &format!("{ARCHIVE}.sha256"),
            "install.sh",
            "install.sh.minisig",
        ] {
            self.serve(&format!("/{TAG}/{name}"), 200, fixture(name));
        }
    }

    fn endpoints(&self) -> Endpoints {
        Endpoints {
            api_latest: format!("{}/releases/latest", self.server.uri()),
            download_base: self.server.uri(),
            allow_http: true,
        }
    }
}

fn release_listing() -> Release {
    Release {
        tag: TAG.to_string(),
        version: Version::new(9, 9, 9),
        html_url: format!("https://github.com/LivioGama/always/releases/tag/{TAG}"),
        assets: [
            ARCHIVE,
            &format!("{ARCHIVE}.minisig"),
            &format!("{ARCHIVE}.sha256"),
            "install.sh",
            "install.sh.minisig",
        ]
        .map(String::from)
        .to_vec(),
    }
}

fn tar_plan() -> InstallPlan {
    InstallPlan {
        os: Os::Linux,
        target: TARGET,
        method: Method::LinuxTar,
        env: vec![
            ("ALWAYS_METHOD", "tar".to_string()),
            ("ALWAYS_PREFIX", "/tmp/never-used".to_string()),
        ],
        location: "/tmp/never-used/bin/always".into(),
    }
}

fn stage(
    gh: &Github,
    verifier_key: &str,
    root: &Path,
    release: &Release,
) -> (Result<apply::Staged, apply::ApplyError>, Vec<Progress>) {
    let verifier = Verifier::from_key_text(verifier_key).unwrap();
    let dl = Downloader::new(&gh.endpoints()).unwrap();
    let mut seen = Vec::new();
    let res = apply::stage_in(root, release, &tar_plan(), &verifier, &dl, &mut |p| {
        seen.push(p)
    });
    (res, seen)
}

// ── Check ───────────────────────────────────────────────────────────────────

fn latest_body(tag: &str, draft: bool, prerelease: bool) -> Vec<u8> {
    serde_json::json!({
        "tag_name": tag, "draft": draft, "prerelease": prerelease,
        "html_url": format!("https://github.com/LivioGama/always/releases/tag/{tag}"),
        "assets": [],
    })
    .to_string()
    .into_bytes()
}

#[test]
fn check_finds_a_newer_release_and_sends_a_user_agent() {
    let gh = Github::start();
    // The mock only answers requests that carry a User-Agent naming `always`.
    gh.rt.block_on(
        Mock::given(method("GET"))
            .and(path("/releases/latest"))
            .and(wiremock::matchers::header_regex("user-agent", "^always/"))
            .and(wiremock::matchers::header(
                "accept",
                "application/vnd.github+json",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(latest_body("v0.2.0", false, false)),
            )
            .mount(&gh.server),
    );
    let out = release::fetch_latest(&gh.endpoints(), &Version::new(0, 1, 0));
    assert!(
        matches!(out, CheckOutcome::Available(ref r) if r.tag == "v0.2.0"),
        "{out:?}"
    );
    let out = release::fetch_latest(&gh.endpoints(), &Version::new(0, 2, 0));
    assert!(matches!(out, CheckOutcome::UpToDate { .. }), "{out:?}");
}

#[test]
fn check_with_no_published_release_is_not_an_error() {
    let gh = Github::start();
    gh.serve(
        "/releases/latest",
        404,
        br#"{"message":"Not Found"}"#.to_vec(),
    );
    let out = release::fetch_latest(&gh.endpoints(), &Version::new(0, 1, 0));
    assert_eq!(out, CheckOutcome::NoRelease);
    assert_eq!(out.result(), "no_release");
}

#[test]
fn check_reports_rate_limit_prerelease_and_draft_without_error() {
    let gh = Github::start();
    gh.rt.block_on(
        Mock::given(method("GET"))
            .and(path("/releases/latest"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("x-ratelimit-remaining", "0")
                    .set_body_bytes(b"rate limit exceeded".to_vec()),
            )
            .mount(&gh.server),
    );
    let out = release::fetch_latest(&gh.endpoints(), &Version::new(0, 1, 0));
    assert!(matches!(out, CheckOutcome::RateLimited { .. }), "{out:?}");

    for (draft, pre) in [(true, false), (false, true)] {
        let gh = Github::start();
        gh.serve("/releases/latest", 200, latest_body("v9.0.0", draft, pre));
        let out = release::fetch_latest(&gh.endpoints(), &Version::new(0, 1, 0));
        assert!(matches!(out, CheckOutcome::Skipped(_)), "{out:?}");
    }
}

#[test]
fn check_offline_is_reported_as_offline() {
    // A port nobody listens on.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let endpoints = Endpoints {
        api_latest: format!("http://127.0.0.1:{port}/releases/latest"),
        download_base: format!("http://127.0.0.1:{port}"),
        allow_http: true,
    };
    let out = release::fetch_latest(&endpoints, &Version::new(0, 1, 0));
    assert!(matches!(out, CheckOutcome::Offline(_)), "{out:?}");
    assert_eq!(out.result(), "offline");
}

#[test]
fn production_endpoints_refuse_plain_http() {
    let endpoints = Endpoints {
        api_latest: "http://api.github.com/repos/LivioGama/always/releases/latest".to_string(),
        download_base: "http://github.com".to_string(),
        allow_http: false,
    };
    let out = release::fetch_latest(&endpoints, &Version::new(0, 1, 0));
    assert!(matches!(out, CheckOutcome::Failed(_)), "{out:?}");
    let dl = Downloader::new(&endpoints).unwrap();
    let root = tempfile::tempdir().unwrap();
    let verifier = Verifier::from_key_text(TEST_PUB).unwrap();
    let res = apply::stage_in(
        root.path(),
        &release_listing(),
        &tar_plan(),
        &verifier,
        &dl,
        &mut |_| {},
    );
    assert!(res.is_err());
    assert!(
        !root.path().join(TAG).exists(),
        "failed stage must leave nothing behind"
    );
}

// ── Download + verify ───────────────────────────────────────────────────────

#[test]
fn stage_downloads_verifies_and_writes_every_file() {
    let gh = Github::start();
    gh.serve_release();
    let root = tempfile::tempdir().unwrap();
    let (res, progress) = stage(&gh, TEST_PUB, root.path(), &release_listing());
    let staged = res.expect("a correctly signed release stages");
    assert_eq!(fs::read(&staged.archive).unwrap(), fixture(ARCHIVE));
    assert_eq!(fs::read(&staged.installer).unwrap(), fixture("install.sh"));
    assert_eq!(
        fs::read_to_string(staged.dir.join(format!("{ARCHIVE}.sha256"))).unwrap(),
        String::from_utf8(fixture(&format!("{ARCHIVE}.sha256"))).unwrap(),
        "the installer re-checks this sibling file"
    );
    assert!(!staged.dir.join(format!("{ARCHIVE}.part")).exists());
    assert!(matches!(
        progress.first(),
        Some(Progress::Downloading { .. })
    ));
    assert!(progress.contains(&Progress::Verifying));
    staged.discard();
    assert!(!root.path().join(TAG).exists());
}

#[cfg(unix)]
#[test]
fn staged_files_are_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let gh = Github::start();
    gh.serve_release();
    let root = tempfile::tempdir().unwrap();
    let staged = stage(&gh, TEST_PUB, root.path(), &release_listing())
        .0
        .unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&staged.dir), 0o700);
    assert_eq!(mode(&staged.archive), 0o600);
    assert_eq!(mode(&staged.installer), 0o600);
}

fn assert_refused_and_clean(
    res: Result<apply::Staged, apply::ApplyError>,
    root: &Path,
    stage_name: &str,
) -> String {
    let err = res.expect_err("must not stage");
    assert_eq!(err.stage(), stage_name, "{err}");
    assert!(
        !root.join(TAG).exists(),
        "a refused download must be deleted: {err}"
    );
    err.to_string()
}

#[test]
fn tampered_archive_is_rejected_and_deleted() {
    let mut bad = fixture(ARCHIVE);
    let last = bad.len() - 1;
    bad[last] ^= 0xff;
    let gh = Github::start();
    for name in [
        &format!("{ARCHIVE}.minisig"),
        &format!("{ARCHIVE}.sha256"),
        "install.sh",
        "install.sh.minisig",
    ] {
        gh.serve(&format!("/{TAG}/{name}"), 200, fixture(name));
    }
    gh.serve(&format!("/{TAG}/{ARCHIVE}"), 200, bad);
    let root = tempfile::tempdir().unwrap();
    let (res, _) = stage(&gh, TEST_PUB, root.path(), &release_listing());
    // The published sha256 no longer matches either, but the signature check
    // fires first.
    let msg = assert_refused_and_clean(res, root.path(), "verify");
    assert!(msg.contains("signature"), "{msg}");
}

#[test]
fn tampered_installer_is_rejected_and_deleted() {
    let gh = Github::start();
    for name in [
        ARCHIVE,
        &format!("{ARCHIVE}.minisig"),
        &format!("{ARCHIVE}.sha256"),
        "install.sh.minisig",
    ] {
        gh.serve(&format!("/{TAG}/{name}"), 200, fixture(name));
    }
    gh.serve(
        &format!("/{TAG}/install.sh"),
        200,
        b"#!/bin/sh\nrm -rf \"$HOME\"\n".to_vec(),
    );
    let root = tempfile::tempdir().unwrap();
    let (res, _) = stage(&gh, TEST_PUB, root.path(), &release_listing());
    assert_refused_and_clean(res, root.path(), "verify");
}

#[test]
fn missing_signature_is_refused_never_skipped() {
    for missing in [
        format!("{ARCHIVE}.minisig"),
        "install.sh.minisig".to_string(),
    ] {
        let gh = Github::start();
        for name in [
            ARCHIVE,
            &format!("{ARCHIVE}.minisig"),
            &format!("{ARCHIVE}.sha256"),
            "install.sh",
            "install.sh.minisig",
        ] {
            if *name != missing {
                gh.serve(&format!("/{TAG}/{name}"), 200, fixture(name));
            }
        }
        let root = tempfile::tempdir().unwrap();
        let (res, _) = stage(&gh, TEST_PUB, root.path(), &release_listing());
        let msg = assert_refused_and_clean(res, root.path(), "verify");
        assert!(msg.contains("no signature"), "{msg}");
    }
}

#[test]
fn signature_by_a_key_the_app_does_not_trust_is_refused() {
    let gh = Github::start();
    gh.serve_release();
    let root = tempfile::tempdir().unwrap();
    // The files are signed by the test key; the app "trusts" another one.
    let (res, _) = stage(&gh, OTHER_PUB, root.path(), &release_listing());
    let msg = assert_refused_and_clean(res, root.path(), "verify");
    assert!(msg.contains("different key"), "{msg}");
}

#[test]
fn a_release_that_lacks_a_required_asset_is_refused_before_any_download() {
    let gh = Github::start(); // serves nothing: any request would 404
    let mut listing = release_listing();
    listing.assets.retain(|a| a != "install.sh.minisig");
    let root = tempfile::tempdir().unwrap();
    let (res, progress) = stage(&gh, TEST_PUB, root.path(), &listing);
    let msg = assert_refused_and_clean(res, root.path(), "check");
    assert!(msg.contains("install.sh.minisig"), "{msg}");
    assert!(progress.is_empty());
}

#[test]
fn published_checksum_mismatch_is_refused() {
    let gh = Github::start();
    for name in [
        ARCHIVE,
        &format!("{ARCHIVE}.minisig"),
        "install.sh",
        "install.sh.minisig",
    ] {
        gh.serve(&format!("/{TAG}/{name}"), 200, fixture(name));
    }
    // A valid-looking digest of other bytes.
    gh.serve(
        &format!("/{TAG}/{ARCHIVE}.sha256"),
        200,
        format!("{}  {ARCHIVE}\n", "0".repeat(64)).into_bytes(),
    );
    let root = tempfile::tempdir().unwrap();
    let (res, _) = stage(&gh, TEST_PUB, root.path(), &release_listing());
    let msg = assert_refused_and_clean(res, root.path(), "verify");
    assert!(msg.contains("SHA-256"), "{msg}");
}

#[test]
fn offline_during_download_leaves_nothing_behind() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let endpoints = Endpoints {
        api_latest: String::new(),
        download_base: format!("http://127.0.0.1:{port}"),
        allow_http: true,
    };
    let dl = Downloader::new(&endpoints).unwrap();
    let verifier = Verifier::from_key_text(TEST_PUB).unwrap();
    let root = tempfile::tempdir().unwrap();
    let res = apply::stage_in(
        root.path(),
        &release_listing(),
        &tar_plan(),
        &verifier,
        &dl,
        &mut |_| {},
    );
    // Fetching the signature is the first thing needed; without a network
    // that is reported as the missing signature it is.
    let err = res.expect_err("offline");
    assert!(matches!(err.stage(), "verify" | "download"), "{err}");
    assert!(!root.path().join(TAG).exists());
}

// ── Running the installer ───────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn installer_is_started_with_the_verified_archive_and_this_install() {
    use std::os::unix::fs::PermissionsExt as _;
    let work = tempfile::tempdir().unwrap();
    let dump = work.path().join("env.txt");
    let script = work.path().join("install.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/bash\nenv | grep '^ALWAYS_' | sort > '{}'\npwd >> '{}'\n",
            dump.display(),
            dump.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let archive = work.path().join(ARCHIVE);
    fs::write(&archive, b"x").unwrap();
    let staged = apply::Staged {
        dir: work.path().to_path_buf(),
        archive: archive.clone(),
        installer: script,
    };
    let status = installer_command(&staged, &tar_plan(), TAG)
        .status()
        .unwrap();
    assert!(status.success());
    let seen = fs::read_to_string(&dump).unwrap();
    assert!(
        seen.contains(&format!("ALWAYS_ARCHIVE={}", archive.display())),
        "{seen}"
    );
    assert!(seen.contains("ALWAYS_VERSION=v9.9.9"), "{seen}");
    assert!(seen.contains("ALWAYS_METHOD=tar"), "{seen}");
    assert!(seen.contains("ALWAYS_PREFIX=/tmp/never-used"), "{seen}");
    // The detached (GUI) path sets no updater pid: the daemon that started
    // the installer is meant to be quit and replaced with the bundle.
    assert!(!seen.contains("ALWAYS_UPDATER_PID"), "{seen}");
    assert!(!seen.contains("ALWAYS_BASE_URL"), "{seen}");
}
