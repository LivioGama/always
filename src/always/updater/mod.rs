//! Cross-platform self-update (SPEC §12a). Replaces Sparkle.
//!
//! * [`release`] — ask GitHub whether a newer release exists.
//! * [`verify`]  — minisign check against the key compiled into the binary.
//! * [`apply`]   — download, verify, and hand the verified installer the
//!   verified archive.
//! * this module — the daemon side: a background check (notify only), the
//!   remembered result, and the UDS command handlers.
//!
//! The daemon never installs anything unprompted. All blocking network work
//! runs on plain threads (reqwest's blocking client must not run inside the
//! tokio runtime).

pub mod apply;
pub mod release;
pub mod verify;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::always::event::{DaemonEvent, global_broadcaster};
use apply::{Context, Downloader, Progress};
use release::{CheckOutcome, Endpoints, Release, current_version};
use verify::Verifier;

/// First background check this long after daemon start, so start-up (audio,
/// UDS, model load) never competes with it.
const START_DELAY: Duration = Duration::from_secs(15);
/// Wake-up cadence of the background thread.
const POLL: Duration = Duration::from_secs(60 * 60);
/// A definitive answer is not re-asked for this long.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// At daemon start, skip the check when the previous definitive one is
/// younger than this (a flapping daemon must not burn the 60 req/h/IP limit).
const START_MIN_GAP: Duration = Duration::from_secs(60 * 60);

// ── Preference / environment switch ─────────────────────────────────────────

/// `ALWAYS_UPDATE_CHECK=0|false|no|off` or pref `update_check=false` turn the
/// BACKGROUND check off; explicit requests ignore both. Pure for testing.
pub fn background_enabled_from(env: Option<&str>, pref: Option<bool>) -> bool {
    if let Some(v) = env
        && matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    {
        return false;
    }
    pref.unwrap_or(true)
}

fn background_enabled() -> bool {
    let pref = crate::db::open()
        .and_then(|conn| crate::db::get_preferences(&conn))
        .ok()
        .and_then(|p| p.update_check);
    background_enabled_from(std::env::var("ALWAYS_UPDATE_CHECK").ok().as_deref(), pref)
}

// ── Remembered result ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct Stored {
    /// Unix seconds of the last definitive check.
    checked_at: u64,
    /// Newest published version at that time, if any.
    latest: Option<String>,
    url: Option<String>,
}

fn state_path() -> std::path::PathBuf {
    crate::config::config_dir().join("update-state.json")
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_stored() -> Option<Stored> {
    serde_json::from_slice(&std::fs::read(state_path()).ok()?).ok()
}

fn save_stored(stored: &Stored) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec(stored) {
        let _ = std::fs::write(path, json);
    }
}

/// True when a check is due. `first` = the check at daemon start.
fn is_due(now: u64, last_definitive: Option<u64>, first: bool) -> bool {
    let gap = if first { START_MIN_GAP } else { CHECK_EVERY };
    match last_definitive {
        Some(last) => now.saturating_sub(last) >= gap.as_secs(),
        None => true,
    }
}

/// The line `always status` prints when a stored check found an update.
pub fn status_line() -> Option<String> {
    let stored = load_stored()?;
    let latest = Version::parse(stored.latest.as_deref()?).ok()?;
    let current = current_version();
    (latest > current)
        .then(|| format!("Update available: {current} -> {latest} (run `always update`)"))
}

// ── What the daemon currently knows ─────────────────────────────────────────

#[derive(Clone)]
struct Known {
    current: Version,
    latest: Version,
    url: String,
    can_apply: bool,
    apply_hint: String,
}

static KNOWN: Mutex<Option<Known>> = Mutex::new(None);
static ANNOUNCED: Mutex<Option<Version>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

fn release_url(tag: &str) -> String {
    format!("https://github.com/{}/releases/tag/{tag}", release::REPO)
}

fn update_event(k: &Known) -> DaemonEvent {
    DaemonEvent::UpdateAvailable {
        current: k.current.to_string(),
        latest: k.latest.to_string(),
        url: k.url.clone(),
        can_apply: k.can_apply.to_string(),
        apply_hint: k.apply_hint.clone(),
    }
}

/// The `UpdateAvailable` frame for a client that just connected, if an
/// update is known.
pub fn initial_burst_event() -> Option<DaemonEvent> {
    KNOWN.lock().as_ref().map(update_event)
}

fn set_known(latest: Version, url: String) -> Known {
    let (can_apply, apply_hint) = match apply::plan_for_host(Context::Gui) {
        Ok(_) => (true, String::new()),
        Err(why) => (false, why),
    };
    let known = Known {
        current: current_version(),
        latest,
        url,
        can_apply,
        apply_hint,
    };
    *KNOWN.lock() = Some(known.clone());
    known
}

fn send(event: DaemonEvent) {
    global_broadcaster().send(event);
}

// ── Check ───────────────────────────────────────────────────────────────────

/// One check against GitHub. `user_initiated`: the GUI asked, so the answer
/// is always broadcast; a background check only speaks when it finds an
/// update it has not announced yet.
pub fn run_check(user_initiated: bool) -> CheckOutcome {
    let current = current_version();
    let outcome = release::fetch_latest(&Endpoints::github(), &current);
    tracing::info!(result = outcome.result(), "update_check_done");
    match &outcome {
        CheckOutcome::Offline(e) | CheckOutcome::Failed(e) => {
            tracing::debug!(detail = %e, "update_check_not_completed");
        }
        CheckOutcome::RateLimited { retry_after_secs } => {
            tracing::debug!(?retry_after_secs, "update_check_rate_limited");
        }
        CheckOutcome::Skipped(why) => tracing::debug!(%why, "update_check_release_ignored"),
        _ => {}
    }

    if outcome.is_definitive() {
        let (latest, url) = match &outcome {
            CheckOutcome::Available(r) => (Some(r.version.to_string()), Some(r.html_url.clone())),
            CheckOutcome::UpToDate { latest } => (Some(latest.to_string()), None),
            _ => (None, None),
        };
        save_stored(&Stored {
            checked_at: now_unix(),
            latest,
            url,
        });
    }

    if user_initiated {
        send(DaemonEvent::UpdateCheckDone {
            result: outcome.result().to_string(),
            current: current.to_string(),
            latest: outcome.latest_string(),
        });
    }

    match &outcome {
        CheckOutcome::Available(r) => {
            tracing::info!(current = %current, latest = %r.version, "update_available");
            let url = if r.html_url.is_empty() {
                release_url(&r.tag)
            } else {
                r.html_url.clone()
            };
            let known = set_known(r.version.clone(), url);
            let first_sight = ANNOUNCED.lock().as_ref() != Some(&r.version);
            if user_initiated || first_sight {
                *ANNOUNCED.lock() = Some(r.version.clone());
                send(update_event(&known));
            }
        }
        CheckOutcome::UpToDate { .. } | CheckOutcome::NoRelease | CheckOutcome::Skipped(_) => {
            *KNOWN.lock() = None;
        }
        _ => {}
    }
    outcome
}

// ── Background task ─────────────────────────────────────────────────────────

/// Start the background check thread (notify only). Never blocks the caller
/// and never touches audio or the UDS server.
pub fn spawn_background() {
    if crate::config::instance() != "prod" {
        tracing::debug!("update_check_skipped: not the production instance");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(background_loop);
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "update_check_thread_failed");
    }
}

fn background_loop() {
    apply::remove_leftover_exe();
    if !BUSY.load(Ordering::SeqCst) {
        apply::sweep_stale_downloads();
    }
    // Replay what the last check learned, so a client that connects before
    // the first check of this run still sees a known update.
    if let Some(stored) = load_stored()
        && let Some(latest) = stored
            .latest
            .as_deref()
            .and_then(|v| Version::parse(v).ok())
        && latest > current_version()
    {
        let url = stored
            .url
            .unwrap_or_else(|| release_url(&format!("v{latest}")));
        set_known(latest, url);
    }

    std::thread::sleep(START_DELAY);
    let mut first = true;
    loop {
        if background_enabled() {
            let last = load_stored().map(|s| s.checked_at);
            if is_due(now_unix(), last, first) {
                run_check(false);
            }
        } else if first {
            tracing::info!("update_check_disabled");
        }
        first = false;
        std::thread::sleep(POLL);
    }
}

// ── UDS handlers ────────────────────────────────────────────────────────────

/// `CheckForUpdates` from the GUI.
pub fn request_check() {
    let spawned = std::thread::Builder::new()
        .name("update-check-now".into())
        .spawn(|| {
            run_check(true);
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "update_check_thread_failed");
    }
}

/// `InstallUpdate` from the GUI.
pub fn request_install() {
    let spawned = std::thread::Builder::new()
        .name("update-install".into())
        .spawn(gui_install);
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "update_install_thread_failed");
    }
}

/// Held while an install runs; one at a time.
struct BusyGuard;

impl BusyGuard {
    fn acquire() -> Option<Self> {
        BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

fn fail(stage: &str, message: impl Into<String>) {
    let message = message.into();
    tracing::warn!(stage, %message, "update_failed");
    send(DaemonEvent::UpdateFailed {
        stage: stage.to_string(),
        message,
    });
}

fn progress_event(p: Progress) -> DaemonEvent {
    match p {
        Progress::Downloading { asset, percent } => DaemonEvent::UpdateProgress {
            phase: "downloading".to_string(),
            percent: percent.map(|p| p.to_string()).unwrap_or_default(),
            detail: asset,
        },
        Progress::Verifying => DaemonEvent::UpdateProgress {
            phase: "verifying".to_string(),
            percent: String::new(),
            detail: String::new(),
        },
    }
}

fn gui_install() {
    let Some(busy) = BusyGuard::acquire() else {
        fail("refused", "An update is already being installed.");
        return;
    };
    let plan = match apply::plan_for_host(Context::Gui) {
        Ok(plan) => plan,
        Err(why) => {
            fail("refused", why);
            return;
        }
    };
    let current = current_version();
    let endpoints = Endpoints::github();
    let release: Release = match release::fetch_latest(&endpoints, &current) {
        CheckOutcome::Available(r) => r,
        CheckOutcome::UpToDate { latest } => {
            send(DaemonEvent::UpdateCheckDone {
                result: "up_to_date".to_string(),
                current: current.to_string(),
                latest: latest.to_string(),
            });
            return;
        }
        other @ (CheckOutcome::NoRelease | CheckOutcome::Skipped(_)) => {
            send(DaemonEvent::UpdateCheckDone {
                result: other.result().to_string(),
                current: current.to_string(),
                latest: String::new(),
            });
            return;
        }
        other => {
            fail("check", other.describe(&current));
            return;
        }
    };
    tracing::info!(current = %current, latest = %release.version, "update_install_started");

    let staged = (|| {
        let verifier = Verifier::embedded()?;
        let downloader = Downloader::new(&endpoints)?;
        apply::stage(&release, &plan, &verifier, &downloader, &mut |p| {
            send(progress_event(p));
        })
    })();
    let staged = match staged {
        Ok(s) => s,
        Err(e) => {
            fail(e.stage(), e.to_string());
            return;
        }
    };

    let log = crate::always::telemetry::get_log_directory().join("update-install.log");
    let log_for_msg = log.clone();
    let staged_for_exit = staged.clone();
    let spawned =
        apply::spawn_installer_detached(&staged, &plan, &release.tag, &log, move |status| {
            // Reached only when this process survived the installer: it failed
            // before quitting the app, or the install replaced a tarball binary.
            match status {
                Ok(s) if s.success() => tracing::info!("update_installer_finished"),
                Ok(s) => fail(
                    "install",
                    format!(
                        "The installer exited with {s}. See {}",
                        log_for_msg.display()
                    ),
                ),
                Err(e) => fail(
                    "install",
                    format!("The installer could not be awaited: {e}"),
                ),
            }
            staged_for_exit.discard();
            drop(busy);
        });
    match spawned {
        Ok(()) => send(DaemonEvent::UpdateProgress {
            phase: "installing".to_string(),
            percent: String::new(),
            detail: format!("Installing {}", release.tag),
        }),
        Err(e) => {
            staged.discard();
            fail(e.stage(), e.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_out_by_env_or_pref() {
        // Default: on.
        assert!(background_enabled_from(None, None));
        // Pref off.
        assert!(!background_enabled_from(None, Some(false)));
        assert!(background_enabled_from(None, Some(true)));
        // Env off wins over a pref that says on, in every spelling.
        for v in ["0", "false", "FALSE", "no", "off", " 0 "] {
            assert!(!background_enabled_from(Some(v), Some(true)), "{v}");
            assert!(!background_enabled_from(Some(v), None), "{v}");
        }
        // Env cannot force-enable over a pref that says off.
        assert!(!background_enabled_from(Some("1"), Some(false)));
        // Unrelated env values change nothing.
        assert!(background_enabled_from(Some("1"), None));
        assert!(background_enabled_from(Some(""), None));
    }

    #[test]
    fn check_cadence_start_gap_then_24h() {
        let h = 3600;
        // Never checked: due, at start and later.
        assert!(is_due(1_000_000, None, true));
        assert!(is_due(1_000_000, None, false));
        // At start: skip when the last definitive check is < 1 h old.
        assert!(!is_due(1_000_000, Some(1_000_000 - 59 * 60), true));
        assert!(is_due(1_000_000, Some(1_000_000 - h), true));
        // Afterwards: only every 24 h.
        assert!(!is_due(1_000_000, Some(1_000_000 - 23 * h), false));
        assert!(is_due(1_000_000, Some(1_000_000 - 24 * h), false));
        // A clock set backwards never makes it due early.
        assert!(!is_due(1_000, Some(2_000), false));
    }

    #[test]
    fn update_event_carries_the_contract_fields() {
        let k = Known {
            current: Version::new(0, 1, 0),
            latest: Version::new(0, 2, 0),
            url: release_url("v0.2.0"),
            can_apply: false,
            apply_hint: "Run `always update`.".to_string(),
        };
        let json = update_event(&k).to_json_line().unwrap();
        assert_eq!(
            json.trim_end(),
            r#"{"type":"UpdateAvailable","data":{"current":"0.1.0","latest":"0.2.0","url":"https://github.com/LivioGama/always/releases/tag/v0.2.0","can_apply":"false","apply_hint":"Run `always update`."}}"#
        );
    }

    #[test]
    fn progress_maps_to_wire_strings() {
        let ev = progress_event(Progress::Downloading {
            asset: "a.tar.gz".to_string(),
            percent: Some(42),
        });
        assert!(matches!(
            ev,
            DaemonEvent::UpdateProgress { ref phase, ref percent, .. } if phase == "downloading" && percent == "42"
        ));
        assert!(matches!(
            progress_event(Progress::Verifying),
            DaemonEvent::UpdateProgress { ref phase, ref percent, .. } if phase == "verifying" && percent.is_empty()
        ));
    }

    #[test]
    fn busy_guard_allows_one_install_at_a_time() {
        // Serialised with other BUSY users by living in one test.
        let first = BusyGuard::acquire().expect("free");
        assert!(BusyGuard::acquire().is_none());
        drop(first);
        assert!(BusyGuard::acquire().is_some());
    }
}
