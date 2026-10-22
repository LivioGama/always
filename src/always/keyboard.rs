//! Global keyboard shortcut handler — pause toggle, auto-enter toggle, force-paste.
//!
//! Default shortcuts: ⌃⌥P (pause), ⌃⌥A (auto-enter), ⌃⌥V (paste-anyway last filtered).
//! All configurable via `always config set shortcut_pause ctrl+alt+p`
//! (live-reloaded via the `ReloadShortcuts` UDS command — no restart).
//!
//! ## Cross-platform layout
//!
//! `Combo` (parsing + matching) is portable — modifier flags + a single
//! key character string. The actual global hotkey listener that converts
//! OS key events into `Combo::matches_name` calls is platform-specific:
//!
//! * **macOS** (`target_os = "macos"`): uses `rdev` to subscribe to system
//!   keyboard events. (P2 will replace `rdev` with a thin `CGEventTap`
//!   wrapper to drop the unmaintained dep.)
//! * **Windows**: `RegisterHotKey` + a thread-local `GetMessage` loop.
//!   Windows matches the full modifier+key chord natively, so no raw
//!   keypress stream is needed. Live reload goes through a `WM_APP`
//!   thread message that unregisters and re-registers the combos.
//! * **Linux/X11**: uses `rdev` to observe system keyboard events. On
//!   Wayland, global shortcuts remain unavailable; use the CLI instead.

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use std::sync::Arc;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use std::thread;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use std::time::Duration;

use anyhow::Result;

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use super::{clipboard_watcher, config as always_config, correction, event, log, paste, pause};

// ─── Live-reloadable shortcut state ───────────────────────────────────
//
// The keyboard listener's CGEventTap callback captures an `Arc<RwLock<Shortcuts>>`
// and reads the current combos on every keypress. `reload_shortcuts()` swaps
// the contents, so a Settings change takes effect immediately — no daemon
// restart needed. The read lock is held for microseconds (copy 6 Combo
// values), which is safe inside the time-budgeted CGEventTap callback.

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
struct Shortcuts {
    pause: Combo,
    auto_enter: Combo,
    force_paste: Combo,
    log_correction: Combo,
    correction_dialog: Combo,
    master_pause: Combo,
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
static SHARED_SHORTCUTS: std::sync::OnceLock<Arc<parking_lot::RwLock<Shortcuts>>> =
    std::sync::OnceLock::new();

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn shared_shortcuts() -> &'static Arc<parking_lot::RwLock<Shortcuts>> {
    SHARED_SHORTCUTS.get_or_init(|| {
        let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
            load_shortcuts();
        Arc::new(parking_lot::RwLock::new(Shortcuts {
            pause,
            auto_enter,
            force_paste,
            log_correction,
            correction_dialog,
            master_pause,
        }))
    })
}

/// Re-read shortcuts from the prefs DB and swap them into the live
/// listener. Called by the `ReloadShortcuts` UDS command — safe to
/// call any time after `start_keyboard_listener` has run.
#[cfg(target_os = "macos")]
pub fn reload_shortcuts() {
    let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
        load_shortcuts();
    let new = Shortcuts {
        pause,
        auto_enter,
        force_paste,
        log_correction,
        correction_dialog,
        master_pause,
    };
    // Check if Fn is now the master-pause shortcut — if the state
    // transitioned to/from Fn, the Fn listener needs to start/stop.
    let was_fn;
    let is_fn = new.master_pause.key_char == "fn";
    {
        let shared = shared_shortcuts();
        let mut guard = shared.write();
        was_fn = guard.master_pause.key_char == "fn";
        *guard = new;
    }
    if is_fn && !was_fn {
        start_fn_listener();
    }
    tracing::info!("shortcuts_reloaded");
}

/// Windows: swap the live combos, then ask the listener thread to
/// unregister + re-register its `RegisterHotKey` calls.
#[cfg(target_os = "windows")]
pub fn reload_shortcuts() {
    let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
        load_shortcuts();
    {
        let shared = shared_shortcuts();
        let mut guard = shared.write();
        *guard = Shortcuts {
            pause,
            auto_enter,
            force_paste,
            log_correction,
            correction_dialog,
            master_pause,
        };
    }
    windows_post_reload();
    tracing::info!("shortcuts_reloaded");
}

#[cfg(target_os = "linux")]
pub fn reload_shortcuts() {
    let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
        load_shortcuts();
    *shared_shortcuts().write() = Shortcuts {
        pause,
        auto_enter,
        force_paste,
        log_correction,
        correction_dialog,
        master_pause,
    };
    tracing::info!("shortcuts_reloaded");
}

/// Event-based tracking of Command key state as a fallback when
/// CGEventSourceFlagsState is unreliable. Updated by the keyboard listener.
#[cfg(target_os = "macos")]
static CMD_HELD_EVENT: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static OPTION_HELD_EVENT: AtomicBool = AtomicBool::new(false);

/// Raw check of CGEventSourceFlagsState - can be unreliable on some systems.
#[cfg(target_os = "macos")]
fn is_cmd_held_cg() -> bool {
    unsafe extern "C" {
        fn CGEventSourceFlagsState(state_id: i32) -> u64;
    }
    const HID_SYSTEM_STATE: i32 = 1;
    const CG_EVENT_FLAG_COMMAND: u64 = 0x0010_0000;
    let flags = unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) };
    flags & CG_EVENT_FLAG_COMMAND != 0
}

/// True when the user is physically holding ⌘ right now.
///
/// Uses a conservative hybrid approach to minimize false positives:
/// 1. Primary: CGEventSourceFlagsState for real hardware state (avoids stuck events)
/// 2. Fallback: Event-based tracking when CG is unreliable
/// 3. Strict debounce: Requires BOTH methods to agree across multiple checks
///
/// This addresses the issue where CGEventSourceFlagsState can report false
/// positives on some systems. We now require both methods to agree AND
/// maintain that state across multiple checks to avoid transient false positives.
#[cfg(target_os = "macos")]
pub fn is_cmd_held() -> bool {
    // Quick single check first - if either method definitely says not held, return fast
    let cg_result = is_cmd_held_cg();
    let event_result = CMD_HELD_EVENT.load(Ordering::Relaxed);

    // If both agree it's not held, we're confident
    if !cg_result && !event_result {
        return false;
    }

    // If they disagree, be conservative and assume not held (avoid false positives)
    if cg_result != event_result {
        return false;
    }

    // Both agree it's held - do debounced checks to confirm it's stable
    let mut held_count = 0;
    let checks = 5;
    let delay = Duration::from_millis(10);

    for _ in 0..checks {
        let cg = is_cmd_held_cg();
        let event = CMD_HELD_EVENT.load(Ordering::Relaxed);

        // Require both methods to still agree and report held
        if cg && event {
            held_count += 1;
        } else {
            // If either method changes its mind, immediately return false
            return false;
        }
        std::thread::sleep(delay);
    }

    // Require unanimous agreement across all checks
    held_count == checks
}

#[cfg(not(target_os = "macos"))]
pub fn is_cmd_held() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn is_option_held_cg() -> bool {
    unsafe extern "C" {
        fn CGEventSourceFlagsState(state_id: i32) -> u64;
    }
    const HID_SYSTEM_STATE: i32 = 1;
    const CG_EVENT_FLAG_ALTERNATE: u64 = 0x0008_0000;
    let flags = unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) };
    flags & CG_EVENT_FLAG_ALTERNATE != 0
}

#[cfg(target_os = "macos")]
pub fn is_option_held() -> bool {
    is_option_held_cg() || OPTION_HELD_EVENT.load(Ordering::Relaxed)
}

#[cfg(not(target_os = "macos"))]
pub fn is_option_held() -> bool {
    false
}

/// A parsed keyboard combo: modifier flags + a single character key.
#[allow(dead_code)] // fields are unused on unsupported platforms
#[derive(Clone, Debug)]
struct Combo {
    ctrl: bool,
    shift: bool,
    alt: bool,
    key_char: String,
}

#[allow(dead_code)] // listener body uses these on desktop targets
impl Combo {
    /// Parse `"ctrl+alt+p"` (case-insensitive, any modifier order).
    /// Requires at least one modifier and exactly one key character,
    /// OR a standalone `"fn"` (the Fn/Globe key on macOS keyboards,
    /// which fires no ctrl/shift/alt flags and is the only modifier-less
    /// shortcut we accept).
    fn from_str(s: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut key_char: Option<String> = None;

        for part in s.to_lowercase().split('+') {
            match part.trim() {
                "ctrl" | "control" => ctrl = true,
                "shift" => shift = true,
                "alt" | "option" => alt = true,
                "meta" | "cmd" | "command" => {}
                c if !c.is_empty() => key_char = Some(c.to_string()),
                _ => {}
            }
        }

        let key_char = key_char?;
        // `"fn"` is the only modifier-less shortcut we accept — the Fn
        // key on Apple keyboards fires as a flagsChanged event with no
        // ctrl/shift/alt, so the standard "must have a modifier" guard
        // would reject it. The Fn listener is a separate CGEventTap
        // (see `start_fn_listener`), not rdev.
        if key_char == "fn" {
            return Some(Combo {
                ctrl: false,
                shift: false,
                alt: false,
                key_char,
            });
        }
        if !ctrl && !shift && !alt {
            return None;
        }
        if key_char.len() != 1 && key_char != "space" {
            return None;
        }

        Some(Combo {
            ctrl,
            shift,
            alt,
            key_char,
        })
    }

    /// Match a fired event by its modifier state + the key's portable
    /// shortcut name (e.g. `"p"`, `"space"`, `"a"`).
    fn matches_name(&self, ctrl: bool, shift: bool, alt: bool, key_name: &str) -> bool {
        self.ctrl == ctrl && self.shift == shift && self.alt == alt && self.key_char == key_name
    }
}

#[allow(dead_code)] // referenced only in tests + macos listener
fn default_shortcuts() -> (Combo, Combo, Combo, Combo, Combo, Combo) {
    (
        // ⌃⌥P is strictly per-app: toggle the focused app's place on
        // the resumed allowlist. Global pause lives on ⌃⌥⇧P below.
        Combo::from_str("ctrl+alt+p").expect("default pause shortcut is valid"),
        Combo::from_str("ctrl+alt+a").expect("default auto-enter shortcut is valid"),
        Combo::from_str("ctrl+alt+v").expect("default force-paste shortcut is valid"),
        // ⌃⌥X is the hotkey-driven correction capture: read the user's
        // current selection, diff it against the most recent paste, and
        // append the resulting `(wrong → right)` pairs to the glossary.
        Combo::from_str("ctrl+alt+x").expect("default log-correction shortcut is valid"),
        // ⌃⌥W opens the dialog-driven correction path: type the
        // intended spelling, daemon diffs against the last transcript.
        Combo::from_str("ctrl+alt+w").expect("default correction-dialog shortcut is valid"),
        // ⌃⌥⇧P flips the master (global) pause switch.
        Combo::from_str("ctrl+alt+shift+p").expect("default master-pause shortcut is valid"),
    )
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
fn resolve_shortcuts(
    pause: Option<&str>,
    auto_enter: Option<&str>,
    force_paste: Option<&str>,
    log_correction: Option<&str>,
    correction_dialog: Option<&str>,
    master_pause: Option<&str>,
) -> (Combo, Combo, Combo, Combo, Combo, Combo) {
    let (
        default_pause,
        default_auto_enter,
        default_force_paste,
        default_log_correction,
        default_correction_dialog,
        default_master_pause,
    ) = default_shortcuts();
    let pause_combo = pause.and_then(Combo::from_str).unwrap_or(default_pause);
    let auto_enter_combo = auto_enter
        .and_then(Combo::from_str)
        .unwrap_or(default_auto_enter);
    let force_paste_combo = force_paste
        .and_then(Combo::from_str)
        .unwrap_or(default_force_paste);
    let log_correction_combo = log_correction
        .and_then(Combo::from_str)
        .unwrap_or(default_log_correction);
    let correction_dialog_combo = correction_dialog
        .and_then(Combo::from_str)
        .unwrap_or(default_correction_dialog);
    let master_pause_combo = master_pause
        .and_then(Combo::from_str)
        .unwrap_or(default_master_pause);

    (
        pause_combo,
        auto_enter_combo,
        force_paste_combo,
        log_correction_combo,
        correction_dialog_combo,
        master_pause_combo,
    )
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn load_shortcuts() -> (Combo, Combo, Combo, Combo, Combo, Combo) {
    let defaults = default_shortcuts();

    let Ok(conn) = crate::db::open() else {
        return defaults;
    };
    let Ok(prefs) = crate::db::get_preferences(&conn) else {
        return defaults;
    };

    resolve_shortcuts(
        prefs.shortcut_pause.as_deref(),
        prefs.shortcut_auto_enter.as_deref(),
        prefs.shortcut_force_paste.as_deref(),
        prefs.shortcut_log_correction.as_deref(),
        prefs.shortcut_correction_dialog.as_deref(),
        prefs.shortcut_master_pause.as_deref(),
    )
}

// ----------------------------------------------------------------------
// macOS implementation (rdev-backed; will become CGEventTap in P2.2)
// ----------------------------------------------------------------------
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn key_to_shortcut_name(key: &rdev::Key) -> Option<&'static str> {
    use rdev::Key;
    match key {
        Key::KeyA => Some("a"),
        Key::KeyB => Some("b"),
        Key::KeyC => Some("c"),
        Key::KeyD => Some("d"),
        Key::KeyE => Some("e"),
        Key::KeyF => Some("f"),
        Key::KeyG => Some("g"),
        Key::KeyH => Some("h"),
        Key::KeyI => Some("i"),
        Key::KeyJ => Some("j"),
        Key::KeyK => Some("k"),
        Key::KeyL => Some("l"),
        Key::KeyM => Some("m"),
        Key::KeyN => Some("n"),
        Key::KeyO => Some("o"),
        Key::KeyP => Some("p"),
        Key::KeyQ => Some("q"),
        Key::KeyR => Some("r"),
        Key::KeyS => Some("s"),
        Key::KeyT => Some("t"),
        Key::KeyU => Some("u"),
        Key::KeyV => Some("v"),
        Key::KeyW => Some("w"),
        Key::KeyX => Some("x"),
        Key::KeyY => Some("y"),
        Key::KeyZ => Some("z"),
        Key::Space => Some("space"),
        Key::Function => Some("fn"),
        _ => None,
    }
}

/// Resolution of a per-app pause chord press. Pure data so the
/// decision is unit-testable without a live focus monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // consumed by platform listeners
enum ChordAction {
    /// Toggle this bundle's place on the resumed-allowlist.
    TogglePerApp(String),
    /// No real app is focused (none reported yet, or Always itself is
    /// frontmost) — nothing sensible to toggle; give feedback instead.
    NoFocusedApp,
}

/// Decide what the per-app pause chord should do for the currently
/// focused app. Strictly per-app: master pause never hijacks this
/// chord (it has its own), and Always's own windows are never a valid
/// toggle target.
#[allow(dead_code)] // referenced only in tests + macos listener
fn pause_chord_action(current_app: Option<&str>) -> ChordAction {
    match current_app {
        Some(bundle) if !crate::always::per_app::is_own_bundle_id(bundle) => {
            ChordAction::TogglePerApp(bundle.to_string())
        }
        _ => ChordAction::NoFocusedApp,
    }
}

/// Per-app chord for `bundle`: paused here (for any reason) → resume
/// here (allowlist it, lift idle / watchdog pauses); listening here →
/// take it off the allowlist. Master on → allowlist toggle only. See
/// `pause_commands::per_app_plan`.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn handle_per_app_pause_hotkey(bundle: &str) {
    // Shared with every other pause control: decides by what the user
    // sees (paused here → resume here), see `pause_commands::per_app_plan`.
    let (out, now_listed) = match crate::always::pause_commands::per_app_chord(bundle) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, bundle, "hotkey_set_app_paused_failed");
            return;
        }
    };
    if let Ok(log_path) = always_config::configured_log_path()
        && let Ok(mut logger) = log::Logger::open(&log_path)
    {
        logger.write(log::Event::PauseToggled {
            paused: out.effective,
        });
    }
    tracing::info!(
        bundle,
        now_listed,
        effective = out.effective,
        changed = out.changed,
        scope = "app",
        "hotkey_pause_chord"
    );
}

/// ctrl+option+shift+P: global pause/resume. When ANY global source is
/// pausing (user master pause, audio-output watchdog, mic conflict) OR
/// the idle watchdog has auto-paused, the chord clears them ALL — an
/// explicit user resume overrides the watchdogs until their next
/// transition, so dictating over music or into notes during a call is
/// one chord away. Otherwise it sets the user master pause.
///
/// Idle is cleared BEFORE `clear_global_pauses` so the recompute inside
/// sees idle=false and produces the correct effective state. The old
/// order (clear_global_pauses → set_idle_auto_paused) left
/// EFFECTIVE_PAUSED stale at true because `set_idle_auto_paused` is a
/// plain setter that does not recompute.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
/// Minimum interval between two master-pause toggles. A physical press
/// that reaches the handler twice (two taps, key repeat) toggles once;
/// the second call inside this window is swallowed. 150ms sits well
/// below deliberate rapid tapping (~4-5 Hz ceiling ≈ 200ms+) while
/// swallowing the 2-4ms double-delivery window.
const MASTER_PAUSE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
static MASTER_PAUSE_LAST_FIRE: parking_lot::Mutex<Option<std::time::Instant>> =
    parking_lot::Mutex::new(None);

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn handle_master_pause_hotkey() {
    // Re-entrancy guard: a physical press must produce ONE logical toggle.
    // The master mute is reachable through independent event sources (the
    // flagsChanged Fn tap, and historically rdev's KeyPress(Function) arm);
    // any second source firing ~2ms later re-toggles and cancels the first,
    // which the user experiences as "mutes and immediately unmutes". The
    // source bug is fixed at each arm, but the invariant lives HERE so a
    // future tap or listener regression can never double-toggle again.
    {
        let mut last = MASTER_PAUSE_LAST_FIRE.lock();
        if let Some(t) = *last {
            let elapsed = t.elapsed();
            if elapsed < MASTER_PAUSE_DEBOUNCE {
                tracing::debug!(
                    elapsed_ms = elapsed.as_millis() as u64,
                    "master_pause_debounced"
                );
                return;
            }
        }
        *last = Some(std::time::Instant::now());
    }

    let was_master = pause::is_master_paused();
    let crate::always::pause_commands::Outcome {
        effective,
        changed,
        master,
    } = crate::always::pause_commands::toggle_global();
    // The "Paused/Resumed everywhere" flash describes the MASTER switch. A
    // press that only lifted idle / a watchdog pause left master untouched
    // (off → off): saying "Resumed everywhere" would be wrong, so that press
    // keeps just the plain Resumed flash `toggle_global` already sent.
    if master != was_master {
        event::global_broadcaster().pause_scope_toggled("master", None, master);
    }
    if let Ok(log_path) = always_config::configured_log_path()
        && let Ok(mut logger) = log::Logger::open(&log_path)
    {
        logger.write(log::Event::PauseToggled { paused: effective });
    }
    tracing::info!(
        master,
        effective,
        changed,
        scope = "master",
        "hotkey_pause_chord"
    );
}

/// Ask macOS for Input Monitoring (listen-event) access.
///
/// `rdev`'s listener is a *listen-only* `CGEventTap`, which macOS gates behind
/// the **Input Monitoring** permission (`kTCCServiceListenEvent`) — NOT
/// Accessibility. `rdev` only calls `CGEventTapCreate` and never requests this
/// access, so without an explicit request the tap silently receives no events
/// and "Always" never even appears in System Settings → Privacy & Security →
/// Input Monitoring. Calling `CGRequestListenEventAccess` triggers the one-time
/// system prompt and registers the responsible app in that list so the user can
/// enable it. It also self-heals after a bundle-id bump (which resets TCC
/// grants). Safe to call repeatedly — once granted it's a cheap status read.
/// Tri-state Input Monitoring status for the daemon's event tap:
/// 0 = unknown (listener not started yet), 1 = granted, 2 = denied.
/// Read by `uds_server` so every connecting client gets the status in
/// its initial state burst (a startup broadcast would be lost — no
/// clients are connected yet when the listener starts).
static INPUT_MONITORING_STATUS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn input_monitoring_status() -> Option<bool> {
    match INPUT_MONITORING_STATUS.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn set_input_monitoring_status(granted: bool) {
    INPUT_MONITORING_STATUS.store(
        if granted { 1 } else { 2 },
        std::sync::atomic::Ordering::Relaxed,
    );
    event::global_broadcaster().shortcut_listener_status(granted);
}

#[cfg(target_os = "macos")]
fn request_input_monitoring_access() {
    // Preflight ONLY (cheap status read, never prompts). The GUI is the
    // sole requester: it calls IOHIDRequestAccess, which reliably surfaces
    // "Always" in System Settings on macOS 26 — the daemon's own
    // CGRequestListenEventAccess did NOT (faceless helper), so that call
    // was removed. The status is stored + broadcast so the GUI banner can
    // show "shortcuts inactive" instead of leaving hotkeys silently dead.
    unsafe extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
    }
    let granted = unsafe { CGPreflightListenEventAccess() };
    set_input_monitoring_status(granted);
    if granted {
        tracing::info!("input_monitoring_access_granted");
    } else {
        tracing::warn!(
            "input_monitoring_access_missing: global shortcuts are inert until \
             you enable Always in System Settings → Privacy & Security → \
             Input Monitoring"
        );
    }
}

/// Read the `auto_learn_corrections` preference from the DB.
/// Defaults to `true` when the DB or column is unavailable — the
/// safe default is to learn, matching pre-toggle behavior.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn auto_learn_corrections_enabled() -> bool {
    let Ok(conn) = crate::db::open() else {
        return true;
    };
    let Ok(prefs) = crate::db::get_preferences(&conn) else {
        return true;
    };
    prefs.auto_learn_corrections.unwrap_or(true)
}

/// Spawn a background LLM second-opinion extraction. Runs in a
/// detached thread with its own tokio runtime so it doesn't block
/// the keyboard listener. New pairs are written to the glossary
/// (when `auto_apply` is true) or just logged for review.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn spawn_llm_extraction(
    original: String,
    corrected: String,
    already_found: Vec<crate::always::correction::CorrectionPair>,
    auto_apply: bool,
) {
    use crate::always::correction_extract;

    // No API key → skip silently.
    let Some(api_key) = correction_extract::get_extraction_api_key() else {
        return;
    };

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!(error = %e, "llm_extraction_runtime_failed");
                return;
            }
        };

        let model = correction_extract::get_extraction_model();
        let result = rt.block_on(correction_extract::extract_corrections_via_llm(
            &original,
            &corrected,
            &api_key,
            &model,
            &already_found,
        ));

        match result {
            Ok(new_pairs) if !new_pairs.is_empty() => {
                tracing::info!(
                    count = new_pairs.len(),
                    "llm_extraction_found_additional_pairs"
                );
                crate::always::correction_metrics::record(
                    crate::always::correction_metrics::MetricEvent::LlmExtractionFound,
                );
                if auto_apply {
                    for p in &new_pairs {
                        event::global_broadcaster()
                            .correction_logged(p.wrong.clone(), p.right.clone());
                    }
                    if let Err(e) =
                        crate::always::correction::apply_pairs_to_glossary_with_provenance(
                            &new_pairs,
                            crate::always::correction::Provenance::Llm,
                        )
                    {
                        tracing::error!(error = %e, "llm_extraction_apply_failed");
                    }
                } else {
                    tracing::info!("llm_extraction_skipped_apply_auto_learn_off");
                }
            }
            Ok(_) => {
                tracing::debug!("llm_extraction_no_additional_pairs");
                crate::always::correction_metrics::record(
                    crate::always::correction_metrics::MetricEvent::LlmExtractionEmpty,
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "llm_extraction_failed");
                crate::always::correction_metrics::record(
                    crate::always::correction_metrics::MetricEvent::LlmExtractionFailed,
                );
            }
        }
    });
}

#[cfg(target_os = "macos")]
pub fn start_keyboard_listener() -> Result<()> {
    use rdev::{EventType, Key, listen};

    // Must run before the tap is created — see `request_input_monitoring_access`.
    request_input_monitoring_access();

    // Initialize the shared shortcut state and capture an Arc to it.
    // The listener closure reads from this on every keypress, so
    // `reload_shortcuts()` can swap combos without restarting the tap.
    let shortcuts = Arc::clone(shared_shortcuts());
    let master_pause_is_fn = shortcuts.read().master_pause.key_char == "fn";

    // rdev's listen() creates a CGEventTap that macOS disables after
    // ~10-15s (TapDisabledByTimeout). rdev's callback never handles
    // this event type, so the tap stays dead and all shortcuts stop.
    // We wrap listen() in a restart loop with a watchdog that stops
    // the run loop after 10s, forcing listen() to return so we can
    // recreate the tap. This is not a startup retry — the tap starts
    // instantly and restarts with no user-visible delay.
    thread::spawn(move || {
        let mut ctrl_pressed = false;
        let mut shift_pressed = false;
        let mut alt_pressed = false;

        let result = listen(move |event| {
            // NOTHING that can block belongs in here. This closure IS the
            // CGEventTap callback: macOS runs it synchronously on the
            // WindowServer event-dispatch path and gives it a strict time
            // budget. A `tracing::info!` here serialised a JSON record and
            // wrote it to disk on EVERY key press and release; under disk
            // or CPU contention that overran the budget, macOS killed the
            // tap with kCGEventTapDisabledByTimeout, and the restart loop
            // below immediately rebuilt it into the same overrun. Keep this
            // callback to atomics and cheap comparisons only.
            match event.event_type {
                EventType::KeyPress(Key::ControlLeft) | EventType::KeyPress(Key::ControlRight) => {
                    ctrl_pressed = true;
                }
                EventType::KeyRelease(Key::ControlLeft)
                | EventType::KeyRelease(Key::ControlRight) => {
                    ctrl_pressed = false;
                }
                EventType::KeyPress(Key::ShiftLeft) | EventType::KeyPress(Key::ShiftRight) => {
                    shift_pressed = true;
                }
                EventType::KeyRelease(Key::ShiftLeft) | EventType::KeyRelease(Key::ShiftRight) => {
                    shift_pressed = false;
                }
                EventType::KeyPress(Key::Alt) | EventType::KeyPress(Key::AltGr) => {
                    alt_pressed = true;
                    OPTION_HELD_EVENT.store(true, Ordering::Relaxed);
                }
                EventType::KeyRelease(Key::Alt) | EventType::KeyRelease(Key::AltGr) => {
                    alt_pressed = false;
                    OPTION_HELD_EVENT.store(false, Ordering::Relaxed);
                }
                EventType::KeyPress(Key::MetaLeft) | EventType::KeyPress(Key::MetaRight) => {
                    CMD_HELD_EVENT.store(true, Ordering::Relaxed);
                }
                EventType::KeyRelease(Key::MetaLeft) | EventType::KeyRelease(Key::MetaRight) => {
                    CMD_HELD_EVENT.store(false, Ordering::Relaxed);
                }
                // NOTE: Fn/Globe key is NOT handled here. rdev's CGEventTap
                // forwards keyDown/keyUp; macOS ALSO surfaces Fn as
                // flagsChanged, which rdev drops — but on some macOS
                // versions rdev delivers Fn as KeyPress(Function) too. A
                // dedicated CGEventTap (`start_fn_listener`) watches
                // flagsChanged for keycode 63 and is started only when Fn
                // is the configured master-pause shortcut. It is the single
                // owner: handling Fn here as well double-fired the master
                // toggle (two `hotkey_pause_chord` events 2-4ms apart —
                // mutes then instantly unmutes), and when Fn was NOT the
                // configured shortcut, every Fn press toggled the mute
                // unintentionally.
                EventType::KeyPress(ref key) => {
                    // Fn/Globe is owned EXCLUSIVELY by the flagsChanged tap
                    // (`start_fn_listener`). On this macOS version rdev ALSO
                    // delivers Fn as KeyPress(Function); handling it here too
                    // meant every press ran the master toggle twice, ~2ms
                    // apart (measured: two `hotkey_pause_chord` events per
                    // `fn_key_pressed`, master True then False) — the key
                    // muted and instantly unmuted itself. This arm is the
                    // double-fire, whatever the old comment above claimed.
                    if matches!(key, Key::Function) {
                        return;
                    }
                    // Any user keystroke ends "right after a paste".
                    crate::always::voice_edit::note_user_input();
                    let Some(name) = key_to_shortcut_name(key) else {
                        if pause::countdown_active() {
                            pause::countdown_request_cancel();
                            pause::dictation_buffer_clear();
                        }
                        return;
                    };
                    if pause::countdown_active() {
                        pause::countdown_request_cancel();
                        pause::dictation_buffer_clear();
                    }
                    // Read the current shortcuts from shared state —
                    // `reload_shortcuts()` may have swapped them since
                    // the last keypress. The read lock is held for
                    // microseconds (copy 6 Combo values), safe inside
                    // the CGEventTap callback's time budget.
                    let s = shortcuts.read();
                    if s.master_pause
                        .matches_name(ctrl_pressed, shift_pressed, alt_pressed, name)
                    {
                        drop(s);
                        handle_master_pause_hotkey();
                    } else if s
                        .pause
                        .matches_name(ctrl_pressed, shift_pressed, alt_pressed, name)
                    {
                        drop(s);
                        dispatch_pause_chord();
                    } else if s.auto_enter.matches_name(
                        ctrl_pressed,
                        shift_pressed,
                        alt_pressed,
                        name,
                    ) {
                        drop(s);
                        dispatch_auto_enter();
                    } else if s.force_paste.matches_name(
                        ctrl_pressed,
                        shift_pressed,
                        alt_pressed,
                        name,
                    ) {
                        drop(s);
                        dispatch_force_paste();
                    } else if s.log_correction.matches_name(
                        ctrl_pressed,
                        shift_pressed,
                        alt_pressed,
                        name,
                    ) {
                        drop(s);
                        dispatch_log_correction();
                    } else if s.correction_dialog.matches_name(
                        ctrl_pressed,
                        shift_pressed,
                        alt_pressed,
                        name,
                    ) {
                        drop(s);
                        dispatch_correction_dialog();
                    }
                }
                _ => {
                    if matches!(event.event_type, EventType::ButtonPress(_)) {
                        // A click can move the caret: no longer "right after a paste".
                        crate::always::voice_edit::note_user_input();
                    }
                    if pause::countdown_active()
                        && matches!(event.event_type, EventType::ButtonPress(_))
                    {
                        pause::countdown_request_cancel();
                        pause::dictation_buffer_clear();
                    }
                }
            }
        });

        if let Err(error) = result {
            tracing::error!(?error, "keyboard_listener_error");
            set_input_monitoring_status(false);
        } else {
            tracing::info!("keyboard_listener_started");
            set_input_monitoring_status(true);
        }
    });

    thread::sleep(Duration::from_millis(100));

    // Start the Fn key listener alongside rdev. rdev's CGEventTap only
    // forwards keyDown/keyUp events — it drops flagsChanged, which is how
    // macOS delivers the Fn/Globe key. This separate tap catches that.
    if master_pause_is_fn {
        start_fn_listener();
    }

    Ok(())
}

/// Start a CGEventTap that listens for `flagsChanged` events to detect
/// the Fn/Globe key (keycode 63). The Fn key on macOS keyboards fires as
/// a modifier-flag change, not a keyDown — `rdev` never sees it. This
/// mirrors the approach from iris-sama's `shortcut-events.swift`.
#[cfg(target_os = "macos")]
fn start_fn_listener() {
    use std::os::raw::{c_int, c_void};
    use std::sync::atomic::AtomicPtr;

    const FN_FLAG: u64 = 0x800000;
    const FN_KEYCODE: i64 = 63;

    static FN_PREVIOUSLY_HELD: AtomicBool = AtomicBool::new(false);
    static FN_TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    unsafe extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events: u64,
            cb: unsafe extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void,
            info: *mut c_void,
        ) -> *mut c_void;
        fn CGEventTapEnable(tap: *mut c_void, enable: bool);
        fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
        fn CGEventGetFlags(event: *mut c_void) -> u64;
        fn CFMachPortCreateRunLoopSource(
            alloc: *mut c_void,
            port: *mut c_void,
            order: c_int,
        ) -> *mut c_void;
        fn CFRunLoopAddSource(rl: *mut c_void, src: *mut c_void, mode: *const c_void);
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopRun();
        fn CFRelease(cf: *const c_void);
        /// THE CoreFoundation global — not a string that merely spells the
        /// same thing. `kCFRunLoopCommonModes` is a pseudo-mode CF matches by
        /// POINTER IDENTITY against this symbol. Passing a separately
        /// allocated CFString with identical characters (which is what this
        /// code used to do) files the source under an ordinary custom mode
        /// that no run loop ever runs — so `CFRunLoopRun()` finds no sources
        /// in the default mode and returns `kCFRunLoopRunFinished` instantly,
        /// spinning the restart loop below. Measured: 264 restarts in 60s.
        static kCFRunLoopCommonModes: *const c_void;
    }

    const FLAGS_CHANGED: u64 = 1 << 12;

    unsafe extern "C" fn fn_tap_callback(
        _proxy: *mut c_void,
        type_: u32,
        event: *mut c_void,
        _user_info: *mut c_void,
    ) -> *mut c_void {
        // kCGEventTapDisabledByTimeout = 0xFFFFFFFE
        // kCGEventTapDisabledByUserInput = 0xFFFFFFFF
        if type_ == 0xFFFFFFFE || type_ == 0xFFFFFFFF {
            let tap = FN_TAP.load(Ordering::Relaxed);
            if !tap.is_null() {
                unsafe { CGEventTapEnable(tap, true) };
            }
            return event;
        }

        if type_ != 12 {
            return event;
        }
        let keycode = unsafe { CGEventGetIntegerValueField(event, 9) };
        let flags = unsafe { CGEventGetFlags(event) };

        if keycode == FN_KEYCODE {
            let fn_held = (flags & FN_FLAG) != 0;
            let was_held = FN_PREVIOUSLY_HELD.swap(fn_held, Ordering::Relaxed);
            if fn_held && !was_held {
                tracing::info!("fn_key_pressed");
                handle_master_pause_hotkey();
            }
        }
        event
    }

    thread::spawn(move || {
        let mut restart_count = 0u32;
        loop {
            let tap = unsafe {
                CGEventTapCreate(
                    0, // kCGHIDEventTap
                    0, // kCGHeadInsertEventTap
                    1, // kCGEventTapListenOption
                    FLAGS_CHANGED,
                    fn_tap_callback,
                    std::ptr::null_mut(),
                )
            };

            if tap.is_null() {
                tracing::warn!("fn_listener_tap_failed");
                set_input_monitoring_status(false);
                break;
            }

            FN_TAP.store(tap, Ordering::Relaxed);

            unsafe {
                let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
                if source.is_null() {
                    tracing::error!("fn_listener_runloop_source_failed");
                    set_input_monitoring_status(false);
                    CFRelease(tap as *const c_void);
                    break;
                }
                let rl = CFRunLoopGetCurrent();
                // Use the real constant (see the extern above). Nothing to
                // release: this global is owned by CoreFoundation.
                CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
                CGEventTapEnable(tap, true);
                if restart_count == 0 {
                    tracing::info!("fn_listener_started");
                }
                restart_count += 1;
                CFRunLoopRun();
                // Run loop exited — tap was disabled by timeout.
                // Release and recreate.
                CFRelease(source as *const c_void);
                CFRelease(tap as *const c_void);
                // Back off before rebuilding the tap. Without this the loop
                // is unbounded: a tap that dies immediately (callback overrun,
                // WindowServer under load) is recreated as fast as the CPU
                // allows, and CGEventTapCreate at kCGHIDEventTap level is a
                // synchronous call into WindowServer — hammering it starves
                // system-wide input dispatch and freezes the machine.
                // A healthy tap lives ~10s, so 250ms costs nothing in the
                // normal case and hard-caps a pathological one at 4 Hz.
                //
                // Logged at INFO (was DEBUG, i.e. invisible in shipped logs)
                // because the restart RATE is the only signal that
                // distinguishes a healthy tap from a spinning one.
                tracing::info!(restart_count, "fn_listener_restarting");
                thread::sleep(Duration::from_millis(250));
            }
        }
    });
}

// ----------------------------------------------------------------------
// Shared dispatch — one body per chord, used by every platform listener
// ----------------------------------------------------------------------

/// Per-app pause chord (`⌃⌥P`): toggles the focused app's place on the
/// resumed allowlist. Without a focused app there is no sensible per-app
/// target — on macOS that means Always itself is frontmost; on Windows
/// focus tracking isn't wired yet so this currently falls back to the
/// master toggle rather than dead-ending the only "pause" hotkey users see.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn dispatch_pause_chord() {
    match pause_chord_action(pause::current_app().as_deref()) {
        ChordAction::TogglePerApp(bundle) => {
            handle_per_app_pause_hotkey(&bundle);
        }
        ChordAction::NoFocusedApp => {
            #[cfg(target_os = "windows")]
            {
                handle_master_pause_hotkey();
            }
            #[cfg(target_os = "linux")]
            {
                handle_master_pause_hotkey();
            }
            #[cfg(target_os = "macos")]
            {
                tracing::info!("hotkey_pause_chord_no_focused_app");
                event::global_broadcaster().pause_scope_toggled("none", None, pause::is_paused());
            }
        }
    }
}

/// `⌃⌥A` — auto-enter toggle.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn dispatch_auto_enter() {
    let new_state = pause::toggle_auto_enter();
    if new_state {
        event::global_broadcaster().auto_enter_enabled();
    } else {
        event::global_broadcaster().auto_enter_disabled();
    }
    if let Ok(log_path) = always_config::configured_log_path()
        && let Ok(mut logger) = log::Logger::open(&log_path)
    {
        logger.write(log::Event::AutoEnterToggled { enabled: new_state });
    }
}

/// `⌃⌥V` — paste the last filtered-out transcript anyway.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn dispatch_force_paste() {
    if let Some(text) = pause::take_last_filtered() {
        let text = crate::always::postprocess::local_cleanup(&text);
        let chars = text.len();
        tracing::info!(chars, "force_paste_filtered");
        if let Err(error) = paste::copy_to_clipboard(format!("{} ", text))
            .and_then(|_| paste::paste_text(pause::is_auto_enter_enabled()))
        {
            tracing::error!(?error, "force_paste_failed");
            pause::set_last_filtered(text);
            event::global_broadcaster().transcription_filtered("Force-paste failed — try again");
        } else {
            event::global_broadcaster().transcript_final(text.clone());
            if let Ok(log_path) = always_config::configured_log_path()
                && let Ok(mut logger) = log::Logger::open(&log_path)
            {
                logger.write(log::Event::ForcePastedFiltered { text: &text });
            }
        }
    } else {
        tracing::info!("force_paste_no_text");
        event::global_broadcaster().transcription_filtered("Nothing to paste — no held transcript");
    }
}

/// `⌃⌥X` — hotkey correction capture. On non-macOS platforms
/// `capture_via_hotkey` can't read the foreground selection yet, so this
/// reports `no_recent_paste` / `error` through the usual broadcast rather
/// than failing silently.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn dispatch_log_correction() {
    let auto_learn = auto_learn_corrections_enabled();
    match correction::capture_via_hotkey(clipboard_watcher::PASTE_WINDOW, auto_learn) {
        Ok(correction::CaptureOutcome::Applied {
            pairs,
            applied,
            original,
            corrected,
        }) => {
            for p in &pairs {
                event::global_broadcaster().correction_logged(p.wrong.clone(), p.right.clone());
            }
            event::global_broadcaster().correction_capture_result("applied");
            let _ = applied;
            // Spawn background LLM second-opinion extraction.
            spawn_llm_extraction(original, corrected, pairs, auto_learn);
        }
        Ok(correction::CaptureOutcome::Extracted {
            pairs,
            original,
            corrected,
        }) => {
            for p in &pairs {
                event::global_broadcaster().correction_logged(p.wrong.clone(), p.right.clone());
            }
            event::global_broadcaster().correction_capture_result("extracted_not_applied");
            // Still run LLM extraction for review queue.
            spawn_llm_extraction(original, corrected, pairs, auto_learn);
        }
        Ok(correction::CaptureOutcome::NoRecentPaste) => {
            tracing::debug!("log_correction_no_recent_paste");
            event::global_broadcaster().correction_capture_result("no_recent_paste");
        }
        Ok(correction::CaptureOutcome::NoChange) => {
            tracing::debug!("log_correction_no_change");
            event::global_broadcaster().correction_capture_result("no_change");
        }
        Ok(correction::CaptureOutcome::NoCorrectionPairs) => {
            tracing::debug!("log_correction_no_correction_pairs");
            event::global_broadcaster().correction_capture_result("no_correction_pairs");
        }
        Err(error) => {
            tracing::error!(?error, "log_correction_failed");
            event::global_broadcaster().correction_capture_result("error");
        }
    }
}

/// `⌃⌥W` — open the dialog-driven correction path.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn dispatch_correction_dialog() {
    let last = pause::last_transcript_for_correction().unwrap_or_default();
    event::global_broadcaster().correction_dialog_requested(last);
}

// ----------------------------------------------------------------------
// Windows: RegisterHotKey + GetMessage loop
// ----------------------------------------------------------------------

/// Post a `WM_APP` reload message to the listener thread so it can
/// unregister + re-register its hotkeys. Registration/unregistration
/// must happen on the thread that registered them, so `reload_shortcuts`
/// can't do it directly.
#[cfg(target_os = "windows")]
fn windows_post_reload() {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

    let tid = WINDOWS_LISTENER_TID.load(Ordering::Relaxed);
    if tid == 0 {
        return;
    }
    const WM_HOTKEY_RELOAD: u32 = WM_APP + 0x41;
    unsafe {
        let _ = PostThreadMessageW(tid, WM_HOTKEY_RELOAD, WPARAM(0), LPARAM(0));
    }
}

/// Thread id of the Windows hotkey listener (0 = not started).
#[cfg(target_os = "windows")]
static WINDOWS_LISTENER_TID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Map a [`Combo`] to Win32 `fsModifiers` + virtual-key code. Returns
/// `None` for combos Windows can't express (`fn`, modifier-less keys —
/// `Combo::from_str` already rejects most of them) or named keys we
/// don't translate.
#[cfg(target_os = "windows")]
fn combo_to_win32(c: &Combo) -> Option<(u32, u32)> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, VK_SPACE,
    };

    // `from_str` guarantees at least one of ctrl/alt/shift for anything
    // but "fn"; a config-supplied combo with none would swallow normal
    // keystrokes system-wide, so refuse it here too.
    if !c.ctrl && !c.alt && !c.shift {
        return None;
    }
    let mut mods = MOD_NOREPEAT;
    if c.ctrl {
        mods |= MOD_CONTROL;
    }
    if c.alt {
        mods |= MOD_ALT;
    }
    if c.shift {
        mods |= MOD_SHIFT;
    }
    let mods: HOT_KEY_MODIFIERS = mods;

    let vk = match c.key_char.as_str() {
        "space" => VK_SPACE.0 as u32,
        s if s.len() == 1 => {
            let b = s.as_bytes()[0];
            match b {
                b'a'..=b'z' => (b - b'a') as u32 + 0x41,
                b'0'..=b'9' => (b - b'0') as u32 + 0x30,
                _ => return None,
            }
        }
        // "fn" and any other named key: no virtual-key translation.
        _ => return None,
    };
    Some((mods.0, vk))
}

/// Windows global hotkeys via `RegisterHotKey`. The OS matches the full
/// chord itself, so this needs no raw key stream — just a message queue.
/// `hwnd = HWND_MESSAGE`-style (thread messages): passing `None` posts
/// `WM_HOTKEY` to this thread's queue, which `GetMessageW` drains.
#[cfg(target_os = "windows")]
pub fn start_keyboard_listener() -> Result<()> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_APP, WM_HOTKEY};

    const WM_HOTKEY_RELOAD: u32 = WM_APP + 0x41;

    // Slot ids — stable across reloads so dispatch never has to
    // re-map messages.
    const ID_PAUSE: i32 = 1;
    const ID_AUTO_ENTER: i32 = 2;
    const ID_FORCE_PASTE: i32 = 3;
    const ID_LOG_CORRECTION: i32 = 4;
    const ID_CORRECTION_DIALOG: i32 = 5;
    const ID_MASTER_PAUSE: i32 = 6;
    const ALL_IDS: [i32; 6] = [
        ID_PAUSE,
        ID_AUTO_ENTER,
        ID_FORCE_PASTE,
        ID_LOG_CORRECTION,
        ID_CORRECTION_DIALOG,
        ID_MASTER_PAUSE,
    ];

    let shortcuts = Arc::clone(shared_shortcuts());

    thread::spawn(move || unsafe {
        let tid = GetCurrentThreadId();
        WINDOWS_LISTENER_TID.store(tid, Ordering::Relaxed);
        tracing::debug!(tid, "hotkey_listener_thread_up");

        let register = |s: &Shortcuts| {
            let entries: [(i32, &Combo); 6] = [
                (ID_PAUSE, &s.pause),
                (ID_AUTO_ENTER, &s.auto_enter),
                (ID_FORCE_PASTE, &s.force_paste),
                (ID_LOG_CORRECTION, &s.log_correction),
                (ID_CORRECTION_DIALOG, &s.correction_dialog),
                (ID_MASTER_PAUSE, &s.master_pause),
            ];
            let mut registered = 0u32;
            for (id, combo) in entries {
                let Some((mods, vk)) = combo_to_win32(combo) else {
                    tracing::debug!(id, key = %combo.key_char, "hotkey_combo_skipped_windows");
                    continue;
                };
                match RegisterHotKey(
                    None,
                    id,
                    windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(mods),
                    vk,
                ) {
                    Ok(()) => registered += 1,
                    // ERROR_HOTKEY_ALREADY_REGISTERED (0x581) means another
                    // app owns the chord — warn but keep the rest.
                    Err(e) => {
                        tracing::warn!(id, key = %combo.key_char, error = %e, "register_hotkey_failed")
                    }
                }
            }
            registered
        };

        let n = register(&shortcuts.read());
        tracing::info!(registered = n, tid, "keyboard_listener_started");
        // RegisterHotKey needs no OS permission — report success so the
        // Permissions panel and Shortcuts UI reflect that hotkeys work.
        set_input_monitoring_status(true);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, Some(HWND::default()), 0, 0).as_bool() {
            match msg.message {
                WM_HOTKEY => match msg.wParam.0 as i32 {
                    ID_MASTER_PAUSE => handle_master_pause_hotkey(),
                    ID_PAUSE => dispatch_pause_chord(),
                    ID_AUTO_ENTER => dispatch_auto_enter(),
                    ID_FORCE_PASTE => dispatch_force_paste(),
                    ID_LOG_CORRECTION => dispatch_log_correction(),
                    ID_CORRECTION_DIALOG => dispatch_correction_dialog(),
                    _ => {}
                },
                m if m == WM_HOTKEY_RELOAD => {
                    for id in ALL_IDS {
                        let _ = UnregisterHotKey(None, id);
                    }
                    let n = register(&shortcuts.read());
                    tracing::info!(registered = n, "hotkeys_reregistered");
                }
                _ => {}
            }
        }
        WINDOWS_LISTENER_TID.store(0, Ordering::Relaxed);
    });

    Ok(())
}

// ----------------------------------------------------------------------
// Linux/X11 listener. Wayland intentionally reports unavailable because
// global input capture is denied to ordinary clients by the compositor.
// ----------------------------------------------------------------------
#[cfg(target_os = "linux")]
pub fn start_keyboard_listener() -> Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        tracing::warn!(
            "global_hotkeys_unavailable_wayland: use the CLI to toggle pause and auto-enter"
        );
        set_input_monitoring_status(false);
        return Ok(());
    }

    let shortcuts = Arc::clone(shared_shortcuts());
    thread::spawn(move || {
        use rdev::{EventType, Key, listen};

        let mut ctrl_pressed = false;
        let mut shift_pressed = false;
        let mut alt_pressed = false;
        set_input_monitoring_status(true);
        tracing::info!("x11_hotkey_listener_started");

        let result = listen(move |event| match event.event_type {
            EventType::KeyPress(Key::ControlLeft) | EventType::KeyPress(Key::ControlRight) => {
                ctrl_pressed = true;
            }
            EventType::KeyRelease(Key::ControlLeft) | EventType::KeyRelease(Key::ControlRight) => {
                ctrl_pressed = false;
            }
            EventType::KeyPress(Key::ShiftLeft) | EventType::KeyPress(Key::ShiftRight) => {
                shift_pressed = true;
            }
            EventType::KeyRelease(Key::ShiftLeft) | EventType::KeyRelease(Key::ShiftRight) => {
                shift_pressed = false;
            }
            EventType::KeyPress(Key::Alt) | EventType::KeyPress(Key::AltGr) => {
                alt_pressed = true;
            }
            EventType::KeyRelease(Key::Alt) | EventType::KeyRelease(Key::AltGr) => {
                alt_pressed = false;
            }
            EventType::KeyPress(ref key) => {
                crate::always::voice_edit::note_user_input();
                if pause::countdown_active() {
                    pause::countdown_request_cancel();
                    pause::dictation_buffer_clear();
                }
                let Some(name) = key_to_shortcut_name(key) else {
                    return;
                };
                let s = shortcuts.read();
                if s.master_pause
                    .matches_name(ctrl_pressed, shift_pressed, alt_pressed, name)
                {
                    drop(s);
                    handle_master_pause_hotkey();
                } else if s
                    .pause
                    .matches_name(ctrl_pressed, shift_pressed, alt_pressed, name)
                {
                    drop(s);
                    dispatch_pause_chord();
                } else if s
                    .auto_enter
                    .matches_name(ctrl_pressed, shift_pressed, alt_pressed, name)
                {
                    drop(s);
                    dispatch_auto_enter();
                } else if s
                    .force_paste
                    .matches_name(ctrl_pressed, shift_pressed, alt_pressed, name)
                {
                    drop(s);
                    dispatch_force_paste();
                } else if s.log_correction.matches_name(
                    ctrl_pressed,
                    shift_pressed,
                    alt_pressed,
                    name,
                ) {
                    drop(s);
                    dispatch_log_correction();
                } else if s.correction_dialog.matches_name(
                    ctrl_pressed,
                    shift_pressed,
                    alt_pressed,
                    name,
                ) {
                    drop(s);
                    dispatch_correction_dialog();
                }
            }
            EventType::ButtonPress(_) => {
                crate::always::voice_edit::note_user_input();
                if pause::countdown_active() {
                    pause::countdown_request_cancel();
                    pause::dictation_buffer_clear();
                }
            }
            _ => {}
        });

        if let Err(error) = result {
            tracing::error!(?error, "x11_hotkey_listener_error");
            set_input_monitoring_status(false);
        }
    });
    Ok(())
}

// ----------------------------------------------------------------------
// Listener abstraction for DI
// ----------------------------------------------------------------------

/// Pluggable global keyboard listener.
///
/// Today the macOS implementation lives entirely in
/// [`start_keyboard_listener`] (rdev-driven). This trait carves out the
/// surface so:
///
/// 1. The `event_loop` can take a `Box<dyn KeyEventListener>` and unit-test
///    pause / auto-enter / force-paste behavior without touching the OS.
/// 2. A future P2.2 `CGEventTap` implementation (and Linux/Windows
///    implementations) can drop in without changing callers.
pub trait KeyEventListener: Send + Sync {
    /// Start listening for global keyboard shortcuts. Implementations
    /// typically spawn a background thread and return immediately.
    fn start(&self) -> Result<()>;
}

/// Production listener — wraps the existing platform-specific
/// [`start_keyboard_listener`] entry point.
#[derive(Debug, Default, Clone, Copy)]
pub struct PlatformKeyEventListener;

impl KeyEventListener for PlatformKeyEventListener {
    fn start(&self) -> Result<()> {
        start_keyboard_listener()
    }
}

#[cfg(test)]
pub mod mock {
    //! In-memory test double for [`KeyEventListener`].

    use super::KeyEventListener;
    use anyhow::Result;
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[derive(Debug, Default, Clone)]
    pub struct MockKeyEventListener {
        starts: Arc<Mutex<u32>>,
    }

    impl MockKeyEventListener {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn start_count(&self) -> u32 {
            *self.starts.lock()
        }
    }

    impl KeyEventListener for MockKeyEventListener {
        fn start(&self) -> Result<()> {
            *self.starts.lock() += 1;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Combo, default_shortcuts};

    #[test]
    fn parses_shortcut_with_modifiers_and_key() {
        let combo = Combo::from_str("ctrl+alt+p").expect("valid shortcut");
        assert!(combo.matches_name(true, false, true, "p"));
        assert!(!combo.matches_name(true, true, true, "p"));
        assert!(!combo.matches_name(true, false, true, "a"));
    }

    #[test]
    fn rejects_invalid_shortcuts() {
        assert!(Combo::from_str("p").is_none());
        assert!(Combo::from_str("ctrl+alt").is_none());
        assert!(Combo::from_str("ctrl+alt+return").is_none());
    }

    #[test]
    fn parses_fn_only_shortcut() {
        let combo = Combo::from_str("fn").expect("fn is a valid modifier-less shortcut");
        assert!(combo.matches_name(false, false, false, "fn"));
        assert!(!combo.matches_name(true, false, false, "fn"));
    }

    #[test]
    fn matches_supported_key_names() {
        let combo = Combo::from_str("ctrl+alt+space").unwrap();
        assert!(combo.matches_name(true, false, true, "space"));
        assert!(!combo.matches_name(true, false, true, "p"));
    }

    #[test]
    fn defaults_parse_cleanly() {
        let _ = default_shortcuts();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn falls_back_to_defaults_for_invalid_configured_shortcuts() {
        let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
            super::resolve_shortcuts(
                Some("ctrl+alt+return"),
                Some("a"),
                Some("v"),
                Some("not a combo"),
                Some(""),
                Some("nope"),
            );
        assert!(pause.matches_name(true, false, true, "p"));
        assert!(auto_enter.matches_name(true, false, true, "a"));
        assert!(force_paste.matches_name(true, false, true, "v"));
        assert!(log_correction.matches_name(true, false, true, "x"));
        assert!(correction_dialog.matches_name(true, false, true, "w"));
        assert!(master_pause.matches_name(true, true, true, "p"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn uses_configured_shortcuts_when_valid() {
        let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
            super::resolve_shortcuts(
                Some("shift+x"),
                Some("ctrl+space"),
                Some("ctrl+shift+v"),
                Some("ctrl+alt+l"),
                Some("ctrl+alt+m"),
                Some("ctrl+shift+g"),
            );
        assert!(pause.matches_name(false, true, false, "x"));
        assert!(auto_enter.matches_name(true, false, false, "space"));
        assert!(force_paste.matches_name(true, true, false, "v"));
        assert!(log_correction.matches_name(true, false, true, "l"));
        assert!(correction_dialog.matches_name(true, false, true, "m"));
        assert!(master_pause.matches_name(true, true, false, "g"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn log_correction_default_is_ctrl_alt_x() {
        let (pause, auto_enter, force_paste, log_correction, correction_dialog, master_pause) =
            super::resolve_shortcuts(None, None, None, None, None, None);
        assert!(pause.matches_name(true, false, true, "p"));
        assert!(auto_enter.matches_name(true, false, true, "a"));
        assert!(force_paste.matches_name(true, false, true, "v"));
        assert!(log_correction.matches_name(true, false, true, "x"));
        assert!(correction_dialog.matches_name(true, false, true, "w"));
        assert!(master_pause.matches_name(true, true, true, "p"));
    }

    #[test]
    fn pause_chord_never_collides_with_master_chord() {
        // The defining regression: ⌃⌥P (no shift) must not match the
        // master chord, and ⌃⌥⇧P must not match the per-app chord.
        let (pause, _, _, _, _, master) = default_shortcuts();
        assert!(pause.matches_name(true, false, true, "p"));
        assert!(!pause.matches_name(true, true, true, "p"));
        assert!(master.matches_name(true, true, true, "p"));
        assert!(!master.matches_name(true, false, true, "p"));
    }

    #[test]
    fn chord_action_targets_focused_real_app() {
        use super::{ChordAction, pause_chord_action};
        assert_eq!(
            pause_chord_action(Some("com.apple.Safari")),
            ChordAction::TogglePerApp("com.apple.Safari".to_string())
        );
    }

    #[test]
    fn chord_action_refuses_own_bundles_and_none() {
        use super::{ChordAction, pause_chord_action};
        assert_eq!(pause_chord_action(None), ChordAction::NoFocusedApp);
        for own in crate::always::per_app::ALWAYS_OWN_BUNDLE_IDS {
            assert_eq!(pause_chord_action(Some(own)), ChordAction::NoFocusedApp);
        }
    }
}
