//! Applying an update (SPEC §12a Apply): decide how this binary was
//! installed, download the signed files, verify every one, and hand the
//! verified installer the verified local archive.
//!
//! Nothing here writes outside the updater's own download directory; the
//! installer script (itself signature-verified) does the replacing.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};

use super::release::{Endpoints, Release, user_agent};
use super::verify::{Verifier, VerifyError, finish_stream};

/// Archives are tens of MB; this only bounds a hostile or broken server.
const MAX_ARCHIVE_BYTES: u64 = 1 << 30;
const MAX_INSTALLER_BYTES: u64 = 2 << 20;
const MAX_SIDECAR_BYTES: u64 = 16 << 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-request ceiling for a download (the blocking client has no
/// per-read timeout): 0.1 MB/s still finishes a 180 MB archive.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Downloaded update directories older than this are deleted.
const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

// ── Platform, install kind ──────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Linux,
    Windows,
}

/// The OS and release target triple this binary runs on, `None` when no
/// release asset exists for it (Intel macOS, other architectures).
pub fn host() -> Option<(Os, &'static str)> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some((Os::Mac, "aarch64-apple-darwin"))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((Os::Linux, "x86_64-unknown-linux-gnu"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some((Os::Linux, "aarch64-unknown-linux-gnu"))
    } else if cfg!(all(
        target_os = "windows",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )) {
        // The installer puts the x86_64 build on ARM64 too (emulated).
        Some((Os::Windows, "x86_64-pc-windows-msvc"))
    } else {
        None
    }
}

/// Which release file the installer uses on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    MacApp,
    LinuxTar,
    LinuxDeb,
    LinuxRpm,
    WindowsZip,
}

impl Method {
    fn extension(self) -> &'static str {
        match self {
            Self::MacApp | Self::LinuxTar => "tar.gz",
            Self::LinuxDeb => "deb",
            Self::LinuxRpm => "rpm",
            Self::WindowsZip => "zip",
        }
    }

    /// The package manager owns the installed files.
    pub fn is_package(self) -> bool {
        matches!(self, Self::LinuxDeb | Self::LinuxRpm)
    }
}

/// `always-<tag>-<target>.<ext>` — the stable asset contract shared with
/// `scripts/release/install.sh` / `install.ps1`.
pub fn asset_name(tag: &str, target: &str, method: Method) -> String {
    format!("always-{tag}-{target}.{}", method.extension())
}

pub fn installer_name(os: Os) -> &'static str {
    match os {
        Os::Windows => "install.ps1",
        Os::Mac | Os::Linux => "install.sh",
    }
}

/// Who asked: the terminal user (`always update`) or the GUI over UDS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Cli,
    Gui,
}

/// How to install the update over THIS binary.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub os: Os,
    pub target: &'static str,
    pub method: Method,
    /// Installer environment that points it at the running install.
    pub env: Vec<(&'static str, String)>,
    /// Where the new version will land (for messages).
    pub location: PathBuf,
}

/// Questions about the host, answerable by a fake in tests.
pub trait Probe {
    fn dpkg_owns(&self, path: &Path) -> bool;
    fn rpm_owns(&self, path: &Path) -> bool;
    fn dir_writable(&self, dir: &Path) -> bool;
}

pub struct HostProbe;

impl Probe for HostProbe {
    fn dpkg_owns(&self, path: &Path) -> bool {
        Command::new("dpkg")
            .arg("-S")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn rpm_owns(&self, path: &Path) -> bool {
        Command::new("rpm")
            .arg("-qf")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn dir_writable(&self, dir: &Path) -> bool {
        let probe = dir.join(format!(".always-write-test-{}", std::process::id()));
        match fs::File::create(&probe) {
            Ok(_) => {
                let _ = fs::remove_file(&probe);
                true
            }
            Err(_) => false,
        }
    }
}

/// The `<dir>` of `<dir>/Always.app/Contents/MacOS/<binary>`.
fn mac_app_parent(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.file_name()? == "Always.app")
        .then(|| app.parent().map(Path::to_path_buf))
        .flatten()
}

/// Work out how to update the install `exe` belongs to, or why it cannot be.
/// The error is a ready-to-show sentence.
pub fn plan_install(
    os: Os,
    target: &'static str,
    exe: &Path,
    ctx: Context,
    instance: &str,
    probe: &dyn Probe,
) -> Result<InstallPlan, String> {
    if instance != "prod" {
        return Err("This is a development instance; it is never updated in place.".to_string());
    }
    match os {
        Os::Mac => {
            let Some(app_dir) = mac_app_parent(exe) else {
                return Err(
                    "This copy of Always is not an installed Always.app (a development build?), so it cannot update itself."
                        .to_string(),
                );
            };
            if !probe.dir_writable(&app_dir) {
                return Err(format!(
                    "{} is not writable by you, so Always.app cannot be replaced there. Reinstall from the release page.",
                    app_dir.display()
                ));
            }
            Ok(InstallPlan {
                os,
                target,
                method: Method::MacApp,
                env: vec![("ALWAYS_APP_DIR", app_dir.display().to_string())],
                location: app_dir.join("Always.app"),
            })
        }
        Os::Windows => {
            let is_always_exe = exe
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("always.exe"));
            let Some(dir) = exe.parent().filter(|_| is_always_exe) else {
                return Err(
                    "This is not an installed always.exe, so it cannot update itself.".to_string(),
                );
            };
            let in_cargo_target = dir
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "target");
            if in_cargo_target {
                return Err("This is a development build (cargo target directory); it is not updated in place.".to_string());
            }
            if !probe.dir_writable(dir) {
                return Err(format!("{} is not writable by you.", dir.display()));
            }
            Ok(InstallPlan {
                os,
                target,
                method: Method::WindowsZip,
                env: vec![
                    ("ALWAYS_INSTALL_DIR", dir.display().to_string()),
                    ("ALWAYS_NO_PATH", "1".to_string()),
                ],
                location: dir.to_path_buf(),
            })
        }
        Os::Linux => {
            let method = if probe.dpkg_owns(exe) {
                Method::LinuxDeb
            } else if probe.rpm_owns(exe) {
                Method::LinuxRpm
            } else {
                Method::LinuxTar
            };
            if method.is_package() {
                if ctx == Context::Gui {
                    return Err(
                        "Always was installed with your package manager, which owns its files. Run `always update` in a terminal; it asks for sudo."
                            .to_string(),
                    );
                }
                let name = if method == Method::LinuxDeb {
                    "deb"
                } else {
                    "rpm"
                };
                return Ok(InstallPlan {
                    os,
                    target,
                    method,
                    env: vec![("ALWAYS_METHOD", name.to_string())],
                    location: exe.to_path_buf(),
                });
            }
            let bin_dir = exe.parent();
            let prefix = bin_dir
                .filter(|b| b.file_name().is_some_and(|n| n == "bin"))
                .and_then(Path::parent);
            let (Some(bin_dir), Some(prefix)) = (bin_dir, prefix) else {
                return Err(format!(
                    "{} is not <prefix>/bin/always, the layout the installer maintains, so it cannot update itself.",
                    exe.display()
                ));
            };
            if !probe.dir_writable(bin_dir) {
                return Err(format!(
                    "{} is not writable by you. Run `sudo always update`, or reinstall.",
                    bin_dir.display()
                ));
            }
            Ok(InstallPlan {
                os,
                target,
                method: Method::LinuxTar,
                env: vec![
                    ("ALWAYS_METHOD", "tar".to_string()),
                    ("ALWAYS_PREFIX", prefix.display().to_string()),
                ],
                location: exe.to_path_buf(),
            })
        }
    }
}

/// [`plan_install`] for the running process.
pub fn plan_for_host(ctx: Context) -> Result<InstallPlan, String> {
    let Some((os, target)) = host() else {
        return Err("No Always release is built for this OS/architecture.".to_string());
    };
    let exe = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|e| format!("Cannot locate the running binary: {e}"))?;
    plan_install(os, target, &exe, ctx, crate::config::instance(), &HostProbe)
}

// ── Errors, progress ────────────────────────────────────────────────────────

/// Why an update stopped. `stage()` is the `UpdateFailed.stage` wire value.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Check(String),
    #[error("{0}")]
    Download(String),
    #[error("{0}")]
    Verify(String),
    #[error("{0}")]
    Install(String),
}

impl ApplyError {
    pub fn stage(&self) -> &'static str {
        match self {
            Self::Refused(_) => "refused",
            Self::Check(_) => "check",
            Self::Download(_) => "download",
            Self::Verify(_) => "verify",
            Self::Install(_) => "install",
        }
    }
}

impl From<VerifyError> for ApplyError {
    fn from(e: VerifyError) -> Self {
        Self::Verify(e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Downloading { asset: String, percent: Option<u8> },
    Verifying,
}

// ── Download + verify ───────────────────────────────────────────────────────

pub struct Downloader {
    client: reqwest::blocking::Client,
    base: String,
}

impl Downloader {
    pub fn new(endpoints: &Endpoints) -> Result<Self, ApplyError> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(user_agent())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(DOWNLOAD_TIMEOUT)
            .https_only(!endpoints.allow_http)
            .build()
            .map_err(|e| ApplyError::Download(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            client,
            base: endpoints.download_base.trim_end_matches('/').to_string(),
        })
    }

    fn url(&self, tag: &str, name: &str) -> String {
        format!("{}/{tag}/{name}", self.base)
    }

    fn open(
        &self,
        tag: &str,
        name: &str,
        cap: u64,
    ) -> Result<reqwest::blocking::Response, ApplyError> {
        let resp = self
            .client
            .get(self.url(tag, name))
            .send()
            .map_err(|e| ApplyError::Download(format!("{name}: {}", e.without_url())))?;
        if !resp.status().is_success() {
            return Err(ApplyError::Download(format!(
                "{name}: GitHub answered HTTP {}",
                resp.status().as_u16()
            )));
        }
        if resp.content_length().is_some_and(|n| n > cap) {
            return Err(ApplyError::Download(format!(
                "{name} is larger than allowed"
            )));
        }
        Ok(resp)
    }

    fn bytes(&self, tag: &str, name: &str, cap: u64) -> Result<Vec<u8>, ApplyError> {
        let resp = self.open(tag, name, cap)?;
        let mut body = Vec::new();
        resp.take(cap + 1)
            .read_to_end(&mut body)
            .map_err(|e| ApplyError::Download(format!("{name}: {e}")))?;
        if body.len() as u64 > cap {
            return Err(ApplyError::Download(format!(
                "{name} is larger than allowed"
            )));
        }
        Ok(body)
    }

    fn text(&self, tag: &str, name: &str, cap: u64) -> Result<String, ApplyError> {
        String::from_utf8(self.bytes(tag, name, cap)?)
            .map_err(|_| ApplyError::Download(format!("{name} is not text")))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The files of one verified download, in the update directory.
#[derive(Debug, Clone)]
pub struct Staged {
    pub dir: PathBuf,
    pub archive: PathBuf,
    pub installer: PathBuf,
}

impl Staged {
    /// Delete the downloaded files (best effort).
    pub fn discard(&self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// `<cache dir>/always/update` — where downloads live.
pub fn update_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("always")
        .join("update")
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Download everything the update needs into a fresh private directory and
/// verify it (SPEC §12a U1). On any failure the directory is deleted and
/// nothing is left behind.
pub fn stage(
    release: &Release,
    plan: &InstallPlan,
    verifier: &Verifier,
    downloader: &Downloader,
    progress: &mut dyn FnMut(Progress),
) -> Result<Staged, ApplyError> {
    stage_in(
        &update_root(),
        release,
        plan,
        verifier,
        downloader,
        progress,
    )
}

pub fn stage_in(
    root: &Path,
    release: &Release,
    plan: &InstallPlan,
    verifier: &Verifier,
    downloader: &Downloader,
    progress: &mut dyn FnMut(Progress),
) -> Result<Staged, ApplyError> {
    let archive = asset_name(&release.tag, plan.target, plan.method);
    let installer = installer_name(plan.os);
    let needed = [
        archive.clone(),
        format!("{archive}.minisig"),
        format!("{archive}.sha256"),
        installer.to_string(),
        format!("{installer}.minisig"),
    ];
    for name in &needed {
        if !release.assets.iter().any(|a| a == name) {
            return Err(ApplyError::Check(format!(
                "Release {} has no {name}; refusing to update from it.",
                release.tag
            )));
        }
    }

    let dir = root.join(&release.tag);
    if dir.exists() {
        let _ = fs::remove_dir_all(&dir);
    }
    create_private_dir(&dir)
        .map_err(|e| ApplyError::Download(format!("cannot create {}: {e}", dir.display())))?;

    match stage_files(
        &dir, release, &archive, installer, verifier, downloader, progress,
    ) {
        Ok(staged) => Ok(staged),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

fn stage_files(
    dir: &Path,
    release: &Release,
    archive: &str,
    installer: &str,
    verifier: &Verifier,
    dl: &Downloader,
    progress: &mut dyn FnMut(Progress),
) -> Result<Staged, ApplyError> {
    let tag = release.tag.as_str();

    // Small things first: both signatures, the checksum, the installer.
    // A signature that cannot be fetched is a verification failure, not a
    // network hiccup: there is no way to install without one.
    let signature = |name: String| -> Result<minisign_verify::Signature, ApplyError> {
        let text = dl
            .text(tag, &name, MAX_SIDECAR_BYTES)
            .map_err(|e| ApplyError::Verify(format!("no signature to check {name}: {e}")))?;
        Ok(Verifier::parse_signature(&text)?)
    };
    let archive_sig = signature(format!("{archive}.minisig"))?;
    let installer_sig = signature(format!("{installer}.minisig"))?;
    let sha_text = dl.text(tag, &format!("{archive}.sha256"), MAX_SIDECAR_BYTES)?;
    let want_sha = sha_text
        .split_whitespace()
        .next()
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| ApplyError::Verify(format!("{archive}.sha256 holds no SHA-256 digest")))?;
    let installer_bytes = dl.bytes(tag, installer, MAX_INSTALLER_BYTES)?;
    verifier.verify(&installer_bytes, &installer_sig, installer, tag)?;

    // The archive streams to disk while it is hashed and signature-checked,
    // so the bytes verified are the bytes written. It only gets its real name
    // once it has verified.
    let part = dir.join(format!("{archive}.part"));
    let got_sha = {
        let mut resp = dl.open(tag, archive, MAX_ARCHIVE_BYTES)?;
        let total = resp.content_length();
        let mut out = create_private_file(&part)
            .map_err(|e| ApplyError::Download(format!("cannot write {}: {e}", part.display())))?;
        let mut stream = verifier.stream(&archive_sig)?;
        let mut sha = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut done: u64 = 0;
        let mut last_percent: Option<u8> = None;
        let mut last_report: Option<Instant> = None;
        progress(Progress::Downloading {
            asset: archive.to_string(),
            percent: total.map(|_| 0),
        });
        loop {
            let n = resp
                .read(&mut buf)
                .map_err(|e| ApplyError::Download(format!("{archive}: {e}")))?;
            if n == 0 {
                break;
            }
            done += n as u64;
            if done > MAX_ARCHIVE_BYTES {
                return Err(ApplyError::Download(format!(
                    "{archive} is larger than allowed"
                )));
            }
            out.write_all(&buf[..n]).map_err(|e| {
                ApplyError::Download(format!("cannot write {}: {e}", part.display()))
            })?;
            stream.update(&buf[..n]);
            sha.update(&buf[..n]);
            if let Some(total) = total.filter(|t| *t > 0) {
                let percent = (done.saturating_mul(100) / total).min(100) as u8;
                if last_percent != Some(percent)
                    && last_report.is_none_or(|t| t.elapsed() >= Duration::from_millis(200))
                {
                    last_percent = Some(percent);
                    last_report = Some(Instant::now());
                    progress(Progress::Downloading {
                        asset: archive.to_string(),
                        percent: Some(percent),
                    });
                }
            }
        }
        out.flush()
            .map_err(|e| ApplyError::Download(format!("cannot write {}: {e}", part.display())))?;
        drop(out);
        progress(Progress::Verifying);
        finish_stream(stream, &archive_sig, archive, tag)?;
        hex(&sha.finalize())
    };
    if got_sha != want_sha {
        return Err(ApplyError::Verify(format!(
            "{archive} does not match its published SHA-256 (expected {want_sha}, got {got_sha})"
        )));
    }

    let archive_path = dir.join(archive);
    fs::rename(&part, &archive_path).map_err(|e| {
        ApplyError::Download(format!("cannot finalize {}: {e}", archive_path.display()))
    })?;
    fs::write(dir.join(format!("{archive}.sha256")), &sha_text)
        .map_err(|e| ApplyError::Download(format!("cannot write the checksum file: {e}")))?;
    let installer_path = dir.join(installer);
    let mut f = create_private_file(&installer_path).map_err(|e| {
        ApplyError::Download(format!("cannot write {}: {e}", installer_path.display()))
    })?;
    f.write_all(&installer_bytes).map_err(|e| {
        ApplyError::Download(format!("cannot write {}: {e}", installer_path.display()))
    })?;
    drop(f);

    Ok(Staged {
        dir: dir.to_path_buf(),
        archive: archive_path,
        installer: installer_path,
    })
}

// ── Running the verified installer ──────────────────────────────────────────

/// The installer, pointed at the verified local archive and at THIS install.
/// `ALWAYS_BASE_URL` is cleared so nothing can redirect it to the network.
pub fn installer_command(staged: &Staged, plan: &InstallPlan, tag: &str) -> Command {
    let mut cmd = match plan.os {
        Os::Windows => {
            let mut c = Command::new("powershell.exe");
            c.args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ]);
            c.arg(&staged.installer);
            c
        }
        Os::Mac | Os::Linux => {
            let bash = if Path::new("/bin/bash").exists() {
                "/bin/bash"
            } else {
                "bash"
            };
            let mut c = Command::new(bash);
            c.arg(&staged.installer);
            c
        }
    };
    cmd.current_dir(&staged.dir)
        .env_remove("ALWAYS_BASE_URL")
        .env("ALWAYS_VERSION", tag)
        .env("ALWAYS_ARCHIVE", &staged.archive);
    for (key, value) in &plan.env {
        cmd.env(key, value);
    }
    cmd
}

/// Run the installer to completion with the terminal attached (`always
/// update`): its output, and a `sudo` prompt, reach the user. This process
/// survives the installer quitting the running app (`ALWAYS_UPDATER_PID`),
/// so it can report the result. The detached GUI path sets no such pid: the
/// daemon that started it is part of the bundle being replaced.
pub fn run_installer_wait(
    staged: &Staged,
    plan: &InstallPlan,
    tag: &str,
) -> Result<ExitStatus, ApplyError> {
    installer_command(staged, plan, tag)
        .env("ALWAYS_UPDATER_PID", std::process::id().to_string())
        .status()
        .map_err(|e| ApplyError::Install(format!("cannot start the installer: {e}")))
}

/// Start the installer detached and return at once (GUI path). On macOS it
/// quits and replaces the app that contains this process, so the caller
/// must not wait. `on_exit` runs when the installer ends — which, when it
/// replaced this process, is never.
pub fn spawn_installer_detached<F>(
    staged: &Staged,
    plan: &InstallPlan,
    tag: &str,
    log: &Path,
    on_exit: F,
) -> Result<(), ApplyError>
where
    F: FnOnce(std::io::Result<ExitStatus>) + Send + 'static,
{
    if let Some(parent) = log.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| ApplyError::Install(format!("cannot open {}: {e}", log.display())))?;
    let log_err = log_file
        .try_clone()
        .map_err(|e| ApplyError::Install(format!("cannot open {}: {e}", log.display())))?;
    let mut cmd = installer_command(staged, plan, tag);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Own process group: it must outlive this daemon.
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| ApplyError::Install(format!("cannot start the installer: {e}")))?;
    std::thread::Builder::new()
        .name("update-installer-wait".into())
        .spawn(move || on_exit(child.wait()))
        .map_err(|e| ApplyError::Install(format!("cannot watch the installer: {e}")))?;
    Ok(())
}

// ── Housekeeping ────────────────────────────────────────────────────────────

/// Delete update directories older than an hour. Never call while an
/// update is staging or installing (the caller checks).
pub fn sweep_stale_downloads() {
    sweep_older_than(&update_root(), STALE_AFTER);
}

pub fn sweep_older_than(root: &Path, age: Duration) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|d| d >= age);
        if old {
            let path = entry.path();
            let _ = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

/// Windows renames the running `always.exe` to `always.exe.old` to replace
/// it (a running exe cannot be overwritten, only renamed). Delete the
/// leftover once the old process is gone. Best effort; no-op elsewhere.
pub fn remove_leftover_exe() {
    if !cfg!(windows) {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut old = exe.into_os_string();
        old.push(".old");
        let _ = fs::remove_file(PathBuf::from(old));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProbe {
        dpkg: bool,
        rpm: bool,
        writable: bool,
    }
    impl Probe for FakeProbe {
        fn dpkg_owns(&self, _: &Path) -> bool {
            self.dpkg
        }
        fn rpm_owns(&self, _: &Path) -> bool {
            self.rpm
        }
        fn dir_writable(&self, _: &Path) -> bool {
            self.writable
        }
    }
    const FREE: FakeProbe = FakeProbe {
        dpkg: false,
        rpm: false,
        writable: true,
    };

    fn plan(os: Os, exe: &str, ctx: Context, probe: &FakeProbe) -> Result<InstallPlan, String> {
        plan_install(os, "t", Path::new(exe), ctx, "prod", probe)
    }

    #[test]
    fn asset_names_match_the_installers() {
        let t = "v0.2.0";
        assert_eq!(
            asset_name(t, "aarch64-apple-darwin", Method::MacApp),
            "always-v0.2.0-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_name(t, "x86_64-unknown-linux-gnu", Method::LinuxTar),
            "always-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            asset_name(t, "x86_64-unknown-linux-gnu", Method::LinuxDeb),
            "always-v0.2.0-x86_64-unknown-linux-gnu.deb"
        );
        assert_eq!(
            asset_name(t, "aarch64-unknown-linux-gnu", Method::LinuxRpm),
            "always-v0.2.0-aarch64-unknown-linux-gnu.rpm"
        );
        assert_eq!(
            asset_name(t, "x86_64-pc-windows-msvc", Method::WindowsZip),
            "always-v0.2.0-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(installer_name(Os::Mac), "install.sh");
        assert_eq!(installer_name(Os::Linux), "install.sh");
        assert_eq!(installer_name(Os::Windows), "install.ps1");
    }

    #[test]
    fn dev_instance_is_never_updated() {
        let exe = "/Applications/Always Dev.app/Contents/MacOS/always-daemon";
        let r = plan_install(Os::Mac, "t", Path::new(exe), Context::Cli, "dev", &FREE);
        assert!(r.unwrap_err().contains("development instance"));
    }

    #[test]
    fn macos_installed_bundle_updates_in_place() {
        let p = plan(
            Os::Mac,
            "/Applications/Always.app/Contents/MacOS/always-daemon",
            Context::Gui,
            &FREE,
        )
        .unwrap();
        assert_eq!(p.method, Method::MacApp);
        assert_eq!(p.env, [("ALWAYS_APP_DIR", "/Applications".to_string())]);
        let p = plan(
            Os::Mac,
            "/Users/me/Applications/Always.app/Contents/MacOS/always-daemon",
            Context::Cli,
            &FREE,
        )
        .unwrap();
        assert_eq!(p.env[0].1, "/Users/me/Applications");
    }

    #[test]
    fn macos_non_bundle_or_unwritable_is_refused() {
        for exe in [
            "/Users/me/always/target/debug/always",
            "/Applications/Always Dev.app/Contents/MacOS/always-daemon",
            "/Applications/Renamed.app/Contents/MacOS/always-daemon",
        ] {
            assert!(plan(Os::Mac, exe, Context::Cli, &FREE).is_err(), "{exe}");
        }
        let ro = FakeProbe {
            writable: false,
            ..FREE
        };
        let err = plan(
            Os::Mac,
            "/Applications/Always.app/Contents/MacOS/always-daemon",
            Context::Cli,
            &ro,
        )
        .unwrap_err();
        assert!(err.contains("not writable"));
    }

    #[test]
    fn linux_tarball_is_replaced_in_place_with_its_prefix() {
        let p = plan(Os::Linux, "/home/me/.local/bin/always", Context::Gui, &FREE).unwrap();
        assert_eq!(p.method, Method::LinuxTar);
        assert!(p.env.contains(&("ALWAYS_METHOD", "tar".to_string())));
        assert!(
            p.env
                .contains(&("ALWAYS_PREFIX", "/home/me/.local".to_string()))
        );
    }

    #[test]
    fn linux_odd_layout_or_unwritable_tarball_is_refused() {
        assert!(plan(Os::Linux, "/opt/always/always", Context::Cli, &FREE).is_err());
        let ro = FakeProbe {
            writable: false,
            ..FREE
        };
        assert!(plan(Os::Linux, "/usr/local/bin/always", Context::Cli, &ro).is_err());
    }

    #[test]
    fn linux_packages_update_only_from_the_cli() {
        for (probe, method) in [
            (FakeProbe { dpkg: true, ..FREE }, Method::LinuxDeb),
            (FakeProbe { rpm: true, ..FREE }, Method::LinuxRpm),
        ] {
            let cli = plan(Os::Linux, "/usr/bin/always", Context::Cli, &probe).unwrap();
            assert_eq!(cli.method, method);
            assert!(cli.method.is_package());
            let gui = plan(Os::Linux, "/usr/bin/always", Context::Gui, &probe).unwrap_err();
            assert!(gui.contains("package manager"), "{gui}");
        }
    }

    #[test]
    fn windows_updates_the_directory_it_runs_from() {
        let p = plan(
            Os::Windows,
            "C:/Users/me/AppData/Local/Programs/Always/always.exe",
            Context::Cli,
            &FREE,
        )
        .unwrap();
        assert_eq!(p.method, Method::WindowsZip);
        assert!(p.env.contains(&("ALWAYS_NO_PATH", "1".to_string())));
        assert!(
            p.env
                .iter()
                .any(|(k, v)| *k == "ALWAYS_INSTALL_DIR" && v.ends_with("Programs/Always"))
        );
        assert!(
            plan(
                Os::Windows,
                "C:/src/always/target/release/always.exe",
                Context::Cli,
                &FREE
            )
            .is_err()
        );
        assert!(plan(Os::Windows, "C:/x/other.exe", Context::Cli, &FREE).is_err());
    }

    #[test]
    fn stale_download_directories_are_swept() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("v1.0.0")).unwrap();
        fs::write(root.path().join("v1.0.0").join("f"), b"x").unwrap();
        // A zero max-age makes everything stale; a long one keeps it.
        sweep_older_than(root.path(), Duration::from_secs(3600));
        assert!(root.path().join("v1.0.0").exists());
        sweep_older_than(root.path(), Duration::ZERO);
        assert!(!root.path().join("v1.0.0").exists());
    }
}
