use std::collections::VecDeque;
use std::io;
use std::io::Read;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::Child;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use parking_lot::{Condvar, Mutex};

pub const RATE: u32 = 16_000;
pub const FRAME_MS: u32 = 30;
pub const FRAME_SAMPLES: usize = 480;
pub const FRAME_BYTES: usize = 960;

/// CoreAudio overrun RATE that triggers a `rec` respawn: at least this
/// many "unhandled buffer overrun" lines inside one
/// [`REC_OVERRUN_WINDOW_SECS`] window. Sustained bursts mean SoX is
/// discarding input samples — usually a native-rate capture / resample
/// mismatch on USB mics (e.g. Elgato Wave:3 @ 48 kHz stereo).
///
/// Rate-based, NOT lifetime-cumulative: the old lifetime counter never
/// decayed, so one benign backpressure episode (event loop deliberately
/// not draining the pipe) condemned a healthy recorder at the next
/// `get_or_spawn` — measured at 291 respawns in 2 days, each paying the
/// ~4.5 s device cold start (see `READ_FRAME_TIMEOUT_MS`).
const REC_OVERRUN_RESPAWN_THRESHOLD: u32 = 64;

/// Width of the overrun-counting window (see above).
const REC_OVERRUN_WINDOW_SECS: u64 = 60;

/// Max wall-clock wait for the next frame before declaring the recorder
/// *wedged* (alive, but permanently producing neither data nor EOF) and
/// recycling it. This exists only to stop a stuck `rec` from holding the
/// global recorder mutex forever and deadlocking every audio caller — it
/// is NOT a real-time-quality knob.
///
/// It must sit well above the device's legitimate startup and delivery
/// gaps, or it truncates live speech. With the old `--buffer 131072` the
/// first bytes took ~4.5 s and mid-stream gaps reached ~4.1 s — that was
/// SoX's 4.096 s stdout buffer, not the device (see [`SOX_BUFFER_BYTES`]).
/// The value is kept at 15 s anyway: a device cold start (USB
/// re-enumeration, Bluetooth handoff) can still take seconds, and a false
/// wedge resets the recorder mid-utterance. An earlier 2–3 s value did
/// exactly that and cut speech off.
const READ_FRAME_TIMEOUT_MS: i32 = 15_000;

// Compile-time floor: the wedge timeout must clear a slow device start
// with margin, or it truncates live speech. A regression once shipped a
// 2-3s value that cut utterances off — keep this guard.
const _: () = assert!(READ_FRAME_TIMEOUT_MS >= 10_000);

/// Conservative hygiene recycle for a healthy recorder. The old call-count
/// guard (1000 `get_or_spawn` calls ≈ every 100 s while media plays) paid a
/// device cold start and dropped audio on every recycle to guard against a
/// SoX memory leak nobody has measured. Health checks cover the real
/// failure modes; this 24 h cap is the remaining backstop.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const REC_MAX_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// SoX `--buffer`, in bytes. SoX uses it both as its processing block
/// size AND as the full-buffering size of its stdout (`formats.c`:
/// `setvbuf(fp, NULL, _IOFBF, bufsiz)`, never flushed per block). So it
/// is exactly how much audio SoX accumulates before a single write
/// reaches the daemon: 4096 B = 128 ms of 16 kHz mono i16.
///
/// It was 131072 (4.096 s) — sized as a "stall budget" for the single
/// event-loop thread that used to read the pipe directly. The cost was
/// invisible in every log: audio reached the VAD in 4.1 s bursts, so the
/// listening badge, the end-of-utterance cut and the paste were each up
/// to 4 s late, while timestamps taken at processing time looked instant
/// (measured 2026-09-24: 1098 of ~1250 inter-event gaps at 3.84–4.22 s).
/// The stall budget now lives in the reader thread's ring
/// ([`RING_CAPACITY_FRAMES`]), so SoX's buffer only has to cover
/// CoreAudio's callback cadence.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const SOX_BUFFER_BYTES: usize = 4096;

/// Frames buffered between the reader thread and the consumer (VAD loop,
/// enrollment, wake-on-voice probe). The event-loop thread legitimately
/// stops consuming for a while — transcription, grammar and paste of the
/// previous utterance, embedding inference, a gated pause — and audio
/// captured meanwhile must be kept (the user may still be talking), not
/// left to back up into SoX until CoreAudio discards it. Past the cap
/// the OLDEST frames are dropped: 1000 × 30 ms = 30 s, ~1 MB.
const RING_CAPACITY_FRAMES: usize = 1_000;

/// Bytes the reader thread asks for per `read(2)`: several SoX output
/// blocks, so a backlog drains in one syscall.
const READER_CHUNK_BYTES: usize = 16 * 1024;

/// Bytes of 16 kHz mono i16 audio per millisecond.
const BYTES_PER_MS: u64 = (RATE as u64 * 2) / 1000;

/// One 30 ms frame plus the (estimated) moment its last sample was
/// captured.
struct TimedFrame {
    bytes: [u8; FRAME_BYTES],
    captured_at: Instant,
}

#[derive(Default)]
struct RingState {
    frames: VecDeque<TimedFrame>,
    /// The producer hit EOF or a read error: no more frames will arrive.
    closed: bool,
    /// Frames dropped because the ring was full, in total.
    overflowed: u64,
    /// Frames dropped in the current overflow episode — the consumer has
    /// not taken a frame since the ring filled. Expected during a gated
    /// pause (nobody reads); logged once when it starts and once when it
    /// ends, never per frame.
    overflow_run: u64,
    /// Frames captured before this instant are discarded instead of
    /// delivered — the tail of a gated stretch that was still inside
    /// SoX when the gate lifted (I6). Set by `drain_pending`.
    discard_before: Option<Instant>,
}

/// Bounded single-producer / single-consumer queue of captured frames.
/// Platform-agnostic so its behaviour is unit-tested on every target;
/// only [`RecChild`] (macOS) feeds it from a real recorder today.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct FrameRing {
    state: Mutex<RingState>,
    ready: Condvar,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl FrameRing {
    fn new() -> Self {
        Self {
            state: Mutex::new(RingState::default()),
            ready: Condvar::new(),
        }
    }

    fn push(&self, frame: TimedFrame) {
        let mut st = self.state.lock();
        if st.frames.len() >= RING_CAPACITY_FRAMES {
            st.frames.pop_front();
            st.overflowed += 1;
            st.overflow_run += 1;
            if st.overflow_run == 1 {
                tracing::info!(
                    capacity = RING_CAPACITY_FRAMES,
                    "audio_ring_full_dropping_oldest"
                );
            }
        }
        st.frames.push_back(frame);
        drop(st);
        self.ready.notify_one();
    }

    /// Close the current overflow episode, if any, with one summary line.
    fn end_overflow_run(st: &mut RingState) {
        if st.overflow_run > 0 {
            tracing::info!(
                dropped_frames = st.overflow_run,
                dropped_secs = st.overflow_run as f64 * FRAME_MS as f64 / 1000.0,
                "audio_ring_overflow_ended"
            );
            st.overflow_run = 0;
        }
    }

    fn close(&self) {
        self.state.lock().closed = true;
        self.ready.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    /// Next deliverable frame, oldest first. `Ok(None)` = the producer
    /// closed and everything queued was consumed (EOF);
    /// `Err(TimedOut)` = nothing arrived within `timeout`.
    fn pop(&self, timeout: Duration) -> io::Result<Option<TimedFrame>> {
        let deadline = Instant::now() + timeout;
        let mut st = self.state.lock();
        loop {
            while let Some(frame) = st.frames.pop_front() {
                if st.discard_before.is_some_and(|cut| frame.captured_at < cut) {
                    continue;
                }
                Self::end_overflow_run(&mut st);
                return Ok(Some(frame));
            }
            if st.closed {
                return Ok(None);
            }
            if self.ready.wait_until(&mut st, deadline).timed_out() && st.frames.is_empty() {
                return if st.closed {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "no audio from rec within timeout — recorder wedged",
                    ))
                };
            }
        }
    }

    /// Most recent deliverable frame, discarding everything older. Waits
    /// like [`Self::pop`] when nothing is queued.
    fn pop_newest(&self, timeout: Duration) -> io::Result<Option<TimedFrame>> {
        {
            let mut st = self.state.lock();
            if st.frames.len() > 1 {
                let keep_from = st.frames.len() - 1;
                st.frames.drain(..keep_from);
            }
        }
        self.pop(timeout)
    }

    /// Copy of the most recently queued frame, without consuming anything.
    /// Non-blocking: `None` when nothing is queued.
    fn peek_newest(&self) -> Option<[u8; FRAME_BYTES]> {
        self.state.lock().frames.back().map(|f| f.bytes)
    }

    /// Discard everything queued, plus any frame captured before now that
    /// is still in flight inside SoX. Returns the queued audio dropped,
    /// in bytes.
    fn discard_pending(&self) -> usize {
        let mut st = self.state.lock();
        Self::end_overflow_run(&mut st);
        let dropped = st.frames.len() * FRAME_BYTES;
        st.frames.clear();
        st.discard_before = Some(Instant::now());
        dropped
    }
}

/// Reader-thread body: pull raw bytes from `reader`, cut them into
/// frames, stamp each with its estimated capture time, and queue them.
/// Returns (after closing the ring) on EOF or a read error.
///
/// Capture-time estimate: the newest byte of each read was produced
/// "just now"; every earlier byte is older by its distance from the
/// newest at 32 bytes/ms. So a frame's stamp is back-dated by the audio
/// that follows it in the same read — exact for a burst, and it still
/// holds when the reader itself was late.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn pump_frames<R: Read>(mut reader: R, ring: &FrameRing) {
    let mut chunk = vec![0u8; READER_CHUNK_BYTES];
    let mut pending: Vec<u8> = Vec::with_capacity(READER_CHUNK_BYTES + FRAME_BYTES);
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let now = Instant::now();
                capture_heartbeat(&chunk[..n]);
                pending.extend_from_slice(&chunk[..n]);
                let complete = pending.len() / FRAME_BYTES;
                for i in 0..complete {
                    let start = i * FRAME_BYTES;
                    let end = start + FRAME_BYTES;
                    let newer_bytes = (pending.len() - end) as u64;
                    let age = Duration::from_micros(newer_bytes * 1000 / BYTES_PER_MS);
                    let mut bytes = [0u8; FRAME_BYTES];
                    bytes.copy_from_slice(&pending[start..end]);
                    ring.push(TimedFrame {
                        bytes,
                        captured_at: now.checked_sub(age).unwrap_or(now),
                    });
                }
                pending.drain(..complete * FRAME_BYTES);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                tracing::warn!(error = %e, "rec_reader_error");
                break;
            }
        }
    }
    ring.close();
}

static TEMP_WAV_COUNTER: AtomicU64 = AtomicU64::new(0);

// Global persistent audio recorder to avoid spawning processes repeatedly
#[cfg(target_os = "macos")]
static GLOBAL_RECORDER: LazyLock<Arc<Mutex<Option<RecChild>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(None)));

// Memory pool for audio buffers to reduce allocations
static AUDIO_BUFFER_POOL: LazyLock<Arc<Mutex<VecDeque<Vec<i16>>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(10))));

pub struct AudioBuffer {
    buffer: Vec<i16>,
    pool: Arc<Mutex<VecDeque<Vec<i16>>>>,
}

impl AudioBuffer {
    pub fn get() -> Self {
        let pool = Arc::clone(&AUDIO_BUFFER_POOL);
        let buffer = {
            let mut pool_lock = pool.lock();
            pool_lock
                .pop_front()
                .unwrap_or_else(|| Vec::with_capacity(16000)) // 1 second at 16kHz
        };
        Self { buffer, pool }
    }

    #[allow(clippy::should_implement_trait)] // `as_mut` returns the inner Vec, not a generic AsMut
    pub fn as_mut(&mut self) -> &mut Vec<i16> {
        &mut self.buffer
    }

    pub fn as_slice(&self) -> &[i16] {
        &self.buffer
    }
}

impl Drop for AudioBuffer {
    fn drop(&mut self) {
        // Return buffer to pool. parking_lot is poison-free, so this can never panic.
        self.buffer.clear();
        let mut pool_lock = self.pool.lock();
        if pool_lock.len() < 10 {
            pool_lock.push_back(std::mem::take(&mut self.buffer));
        }
    }
}

/// Tumbling-window overrun counter shared between the stderr drainer
/// (writer) and `is_healthy` (reader). A window older than
/// [`REC_OVERRUN_WINDOW_SECS`] restarts from 1 on the next overrun, so
/// only a *sustained* storm crosses the respawn threshold.
#[cfg(target_os = "macos")]
struct OverrunWindow {
    /// Epoch seconds of the current window's first overrun.
    window_start: AtomicU64,
    count: AtomicU32,
}

#[cfg(target_os = "macos")]
impl OverrunWindow {
    fn new() -> Self {
        Self {
            window_start: AtomicU64::new(0),
            count: AtomicU32::new(0),
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Record one overrun; returns the count within the current window.
    fn record(&self) -> u32 {
        let now = Self::now_secs();
        let start = self.window_start.load(Ordering::Relaxed);
        if now.saturating_sub(start) >= REC_OVERRUN_WINDOW_SECS {
            self.window_start.store(now, Ordering::Relaxed);
            self.count.store(1, Ordering::Relaxed);
            1
        } else {
            self.count.fetch_add(1, Ordering::Relaxed) + 1
        }
    }

    /// Overrun count within the current window, or 0 once the window
    /// has aged out.
    fn windowed_count(&self) -> u32 {
        let start = self.window_start.load(Ordering::Relaxed);
        if Self::now_secs().saturating_sub(start) >= REC_OVERRUN_WINDOW_SECS {
            return 0;
        }
        self.count.load(Ordering::Relaxed)
    }
}

#[cfg(target_os = "macos")]
pub struct RecChild {
    child: Child,
    /// Filled by the reader thread that owns `rec`'s stdout. That thread
    /// is the only reader of the recorder (I4); it exits on EOF, which
    /// `Drop` causes by killing the child.
    ring: Arc<FrameRing>,
    /// Capture stamp of the frame most recently returned by
    /// [`Self::read_frame`] / [`Self::read_newest_frame`].
    last_captured_at: Option<Instant>,
    reuse_count: u32,
    /// Wall-clock age anchor for the daily hygiene recycle (see
    /// `get_or_spawn`).
    spawned_at: std::time::Instant,
    /// Fed by the stderr drainer on CoreAudio buffer overruns.
    overruns: Arc<OverrunWindow>,
}

/// Executable name of the echo-cancelling capture helper inside
/// `Always.app/Contents/MacOS/` (source: `capture/always-capture.swift`).
pub const CAPTURE_HELPER_NAME: &str = "always-capture";

/// How long a freshly spawned capture helper may take to deliver its first
/// bytes before the recorder falls back to `rec`. VPIO start-up measured
/// ~1.6 s on a MacBook Air (vs ~0.55 s for `rec`); 4 s leaves margin under
/// load without stalling capture for long when VPIO is broken.
#[cfg(target_os = "macos")]
const CAPTURE_HELPER_FIRST_BYTES_MS: i32 = 4_000;

/// Set after the capture helper failed to start or stay alive; the daemon
/// then uses `rec` for the rest of its life instead of paying the start-up
/// timeout on every respawn. Cleared by a default-input-device change.
#[cfg(target_os = "macos")]
static CAPTURE_HELPER_FAILED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Same, for the helper's plain (`--no-vp`) mode: set when even plain
/// capture failed, so the daemon goes straight to `rec` if one exists.
#[cfg(target_os = "macos")]
static CAPTURE_PLAIN_FAILED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// SoX `rec`, if installed: Homebrew (Apple Silicon, then Intel), the
/// system, then `PATH`. Never required on macOS — the bundled capture
/// helper covers capture — but kept as the last resort for dev builds.
pub fn find_rec() -> Option<PathBuf> {
    let fixed = [
        "/opt/homebrew/bin/rec",
        "/usr/local/bin/rec",
        "/usr/bin/rec",
    ];
    fixed
        .iter()
        .map(PathBuf::from)
        .chain(
            std::env::var_os("PATH")
                .map(|p| {
                    std::env::split_paths(&p)
                        .map(|d| d.join("rec"))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )
        .find(|p| p.is_file())
}

/// Pref `capture_aec` (applied at config load). Default OFF.
static AEC_PREF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Apply the `capture_aec` preference (NULL/None = OFF). Takes effect at
/// the next recorder spawn.
///
/// Off by default since 2026-10-16 (v0.1.0 user report: micro-cuts and
/// crackling in Apple Music / on a speaker while Always ran). The
/// voice-processing (VPIO) canceller changes what every app plays: measured
/// on a MacBook Air, coreaudiod restarts the OUTPUT device's IO when the
/// unit starts and again when it stops (an audible cut), logs Audio IO
/// Overloads at both edges, and ducks every other app to −15 dB for ~2 s,
/// then −4 dB for as long as it runs — even at the minimum ducking level.
/// `capture_aec true`
/// still opts in (the helper is then started with `--vp`).
pub fn set_aec_pref(enabled: Option<bool>) {
    AEC_PREF.store(
        enabled.unwrap_or(false),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Voice-processing echo cancellation (opt-in, see `set_aec_pref`).
/// Env `ALWAYS_AEC` overrides the pref either way (`0/false/off`,
/// `1/true/on`); an explicit `ALWAYS_CAPTURE_BIN` (tests, replay) implies on.
pub fn aec_enabled() -> bool {
    aec_decision(
        std::env::var("ALWAYS_AEC").ok().as_deref(),
        std::env::var_os("ALWAYS_CAPTURE_BIN").is_some(),
        AEC_PREF.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// [`aec_enabled`] with its inputs explicit (env `ALWAYS_AEC`, whether
/// `ALWAYS_CAPTURE_BIN` is set, the pref).
fn aec_decision(env_aec: Option<&str>, capture_bin_set: bool, pref: bool) -> bool {
    match env_aec {
        Some("0") | Some("false") | Some("off") => return false,
        Some("1") | Some("true") | Some("on") => return true,
        _ => {}
    }
    capture_bin_set || pref
}

/// Capture-helper arguments: the voice-processing canceller only when echo
/// cancellation was asked for, otherwise plain capture. Plain is explicit
/// (`--no-vp`), never implied by an empty argument list.
fn capture_helper_args(aec: bool) -> &'static [&'static str] {
    if aec { &["--vp"] } else { &["--no-vp"] }
}

/// Locate the capture helper: `ALWAYS_CAPTURE_BIN`, then next to the
/// daemon executable (the app bundle's `Contents/MacOS`), then the repo's
/// build output (dev/test runs from `target/`).
pub fn locate_capture_helper() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ALWAYS_CAPTURE_BIN") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()))
    {
        let p = dir.join(CAPTURE_HELPER_NAME);
        if p.is_file() {
            return Some(p);
        }
    }
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("capture")
        .join(CAPTURE_HELPER_NAME);
    dev.is_file().then_some(dev)
}

/// Outcome of [`wait_readable`].
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Readiness {
    /// Nothing arrived within the timeout.
    TimedOut,
    /// Bytes are readable and the writer end is still open.
    Data,
    /// The writer end is closed (`POLLHUP`): the process exited or closed
    /// its stdout. macOS reports `POLLIN|POLLHUP` for this, and it does so
    /// BEFORE the exit is visible to `waitpid`, so `try_wait` cannot be
    /// relied on to confirm it.
    HungUp,
}

/// Block until `fd` is readable (or hung up) or `timeout_ms` elapses. Raw `poll(2)`
/// rather than a `libc`/`nix` dependency — one struct, one extern. Used
/// only for the capture helper's first-bytes check, before its stdout is
/// handed to the reader thread.
#[cfg(target_os = "macos")]
fn wait_readable(fd: i32, timeout_ms: i32) -> io::Result<Readiness> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    const POLLIN: i16 = 0x0001;
    const POLLHUP: i16 = 0x0010;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: u32, timeout: i32) -> i32;
    }

    let mut pfd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };
    loop {
        let rc = unsafe { poll(&mut pfd, 1, timeout_ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(if rc == 0 {
            Readiness::TimedOut
        } else if pfd.revents & POLLHUP != 0 {
            Readiness::HungUp
        } else {
            Readiness::Data
        });
    }
}

/// When the capture helper last switched its echo canceller from bypass to
/// engaged because ANOTHER process started output (not for one of our own
/// cues). Microseconds on [`aec_clock`]; 0 = never.
static AEC_OUTPUT_ENGAGED_US: AtomicU64 = AtomicU64::new(0);

fn aec_clock() -> &'static std::time::Instant {
    static BASE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(std::time::Instant::now)
}

/// Track the helper's echo-canceller transitions from its stderr. Only the
/// bypass→engaged edge caused by other output matters: until it fires,
/// newly started playback reaches the mic unfiltered (the helper reacts to
/// the output starting; it cannot precede it).
fn note_capture_helper_line(line: &str) {
    if line.contains("aec engaged (output pids") {
        let us = aec_clock().elapsed().as_micros() as u64;
        AEC_OUTPUT_ENGAGED_US.store(us.max(1), Ordering::Relaxed);
    }
}

/// Instant of the last bypass→engaged edge caused by other playback.
pub(crate) fn aec_output_engaged_at() -> Option<std::time::Instant> {
    match AEC_OUTPUT_ENGAGED_US.load(Ordering::Relaxed) {
        0 => None,
        us => Some(*aec_clock() + std::time::Duration::from_micros(us)),
    }
}

/// Test hook: pretend the helper reported an output-triggered engage now.
#[cfg(test)]
pub(crate) fn note_capture_helper_line_for_test(line: &str) {
    note_capture_helper_line(line);
}

#[cfg(target_os = "macos")]
impl RecChild {
    /// Spawn the recorder. Order:
    /// 1. the capture helper with echo cancellation (`--vp`) — only when
    ///    AEC was opted into (off by default, see `set_aec_pref`);
    /// 2. the same helper in plain mode (`--no-vp`) — the default, or VPIO
    ///    unavailable on this Mac/device;
    /// 3. SoX `rec`, only if it happens to be installed (dev machines).
    ///
    /// The helper ships inside `Always.app`, so a fresh Mac never needs
    /// Homebrew or SoX (before this, any helper failure — or AEC turned
    /// off — fell through to `/opt/homebrew/bin/rec`, which a new user
    /// does not have, and dictation silently never started). Whatever is
    /// chosen, exactly ONE recorder process exists (I4) — a helper that
    /// fails its start-up check is killed and reaped before the next try.
    pub fn spawn() -> Result<Self> {
        Self::spawn_from(locate_capture_helper(), aec_enabled())
    }

    /// [`Self::spawn`] with the helper location and AEC choice explicit.
    fn spawn_from(helper: Option<PathBuf>, aec: bool) -> Result<Self> {
        if let Some(helper) = helper.as_deref() {
            if aec && !CAPTURE_HELPER_FAILED.load(Ordering::Relaxed) {
                match Self::spawn_capture_helper(helper, capture_helper_args(true)) {
                    Ok(rec) => return Ok(rec),
                    Err(error) => {
                        CAPTURE_HELPER_FAILED.store(true, Ordering::Relaxed);
                        tracing::warn!(error = %error, "capture_helper_fallback_to_plain");
                    }
                }
            }
            if !CAPTURE_PLAIN_FAILED.load(Ordering::Relaxed) {
                match Self::spawn_capture_helper(helper, capture_helper_args(false)) {
                    Ok(rec) => return Ok(rec),
                    Err(error) => {
                        CAPTURE_PLAIN_FAILED.store(true, Ordering::Relaxed);
                        tracing::warn!(error = %error, "capture_helper_plain_fallback_to_rec");
                    }
                }
            }
        }
        match find_rec() {
            Some(rec_path) => Self::spawn_rec(&rec_path),
            None => {
                // Re-arm plain mode: whatever made it fail (no input device
                // yet) may be gone by the next attempt, and there is nothing
                // else to fall back to. Plain failures are fast (the helper
                // exits at once); echo cancellation stays off until the next
                // input-device change (`force_respawn`) so the gated loop's
                // 100 ms retries never each wait out the 4 s VPIO start-up.
                CAPTURE_PLAIN_FAILED.store(false, Ordering::Relaxed);
                match helper {
                    Some(h) => anyhow::bail!(
                        "microphone capture failed: '{}' produced no audio (check System Settings → Privacy & Security → Microphone, and that an input device is connected)",
                        h.display()
                    ),
                    None => anyhow::bail!(
                        "microphone capture unavailable: '{CAPTURE_HELPER_NAME}' is missing from the app bundle and SoX `rec` is not installed — reinstall Always"
                    ),
                }
            }
        }
    }

    /// Spawn `always-capture` (with `args`, e.g. `--no-vp`) and require its
    /// first bytes within [`CAPTURE_HELPER_FIRST_BYTES_MS`].
    fn spawn_capture_helper(path: &std::path::Path, args: &[&str]) -> Result<Self> {
        use std::os::fd::AsRawFd as _;
        tracing::info!(path = %path.display(), args = ?args, "capture_helper_spawn_starting");
        let started = std::time::Instant::now();
        let mut child = std::process::Command::new(path)
            .args(args)
            // stdin carries status-cue commands (`play <path>`): cues are
            // rendered through the helper's voice-processing output so they
            // are never ducked by it (see `capture_cue_play`).
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to run '{}'", path.display()))?;
        let stdout = child
            .stdout
            .take()
            .context("capture helper stdout missing")?;
        let cue_stdin = child.stdin.take();
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader};
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        note_capture_helper_line(trimmed);
                        tracing::info!(line = %trimmed, "capture_helper_stderr");
                    }
                }
            });
        }
        let readiness = wait_readable(stdout.as_raw_fd(), CAPTURE_HELPER_FIRST_BYTES_MS)?;
        // Readable could also be EOF (helper exited, e.g. VPIO unavailable).
        // The hang-up flag is authoritative: `try_wait` alone races, since
        // the pipe closes before the exit becomes waitable, which let a dead
        // helper through as "healthy" and skipped the `--no-vp` fallback.
        let exited = child.try_wait().ok().flatten();
        if readiness != Readiness::Data || exited.is_some() {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "capture helper produced no audio within {CAPTURE_HELPER_FIRST_BYTES_MS} ms (exit: {exited:?})"
            );
        }
        tracing::info!(
            pid = child.id(),
            first_bytes_ms = started.elapsed().as_millis() as u64,
            "capture_helper_spawned"
        );
        let ring = Self::start_reader(stdout, started, "capture-reader")?;
        *CAPTURE_CUE_SINK.lock() = cue_stdin.map(|s| (child.id(), s));
        crate::always::status_sound::capture_route_changed();
        Ok(Self {
            child,
            ring,
            last_captured_at: None,
            reuse_count: 0,
            spawned_at: std::time::Instant::now(),
            overruns: Arc::new(OverrunWindow::new()),
        })
    }

    /// Start the dedicated reader thread that owns the recorder's stdout
    /// and feeds the frame ring. It drains the pipe continuously so the
    /// recorder never blocks on write, whatever the event-loop thread is
    /// doing.
    fn start_reader(
        stdout: std::process::ChildStdout,
        spawned_at: Instant,
        name: &str,
    ) -> Result<Arc<FrameRing>> {
        let ring = Arc::new(FrameRing::new());
        let reader_ring = Arc::clone(&ring);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                // Log when the first audio lands: the device cold
                // start, measured instead of assumed.
                let mut first = FirstRead {
                    inner: stdout,
                    spawned_at,
                    logged: false,
                };
                pump_frames(&mut first, &reader_ring);
            })
            .with_context(|| format!("failed to start {name} thread"))?;
        Ok(ring)
    }

    fn spawn_rec(rec_path: &std::path::Path) -> Result<Self> {
        tracing::info!(path = %rec_path.display(), "rec_spawn_starting");
        let overruns = Arc::new(OverrunWindow::new());
        let buffer_arg = SOX_BUFFER_BYTES.to_string();
        let mut child = std::process::Command::new(rec_path)
            // Capture at the device's native rate/channels (typically 48 kHz
            // stereo on USB mics), then resample to 16 kHz mono on the
            // output side. Requesting `-c 1 -r 16000` on input makes
            // CoreAudio warn and misbehave ("can't set sample rate 16000;
            // using 48000") which leads to buffer overruns and dropped
            // speech energy in the VAD pipeline.
            .args([
                "--no-show-progress",
                // Small on purpose: this is SoX's stdout granularity, i.e.
                // the minimum delay before any sample reaches the daemon.
                // See SOX_BUFFER_BYTES for why it must not grow back.
                "--buffer",
                buffer_arg.as_str(),
                "-t",
                "raw",
                "-e",
                "signed-integer",
                "-b",
                "16",
                "-",
                "remix",
                "-",
                "rate",
                "16000",
                "channels",
                "1",
            ])
            .stdout(std::process::Stdio::piped())
            // Capture stderr instead of dropping it on the floor. SoX
            // emits permission-denial / device-busy errors to stderr;
            // previously those were silently lost which meant a mic TCC
            // denial looked identical to a healthy idle daemon.
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to run '{}'", rec_path.display()))?;
        let stdout = child.stdout.take().context("sox stdout missing")?;
        let ring = Self::start_reader(stdout, Instant::now(), "rec-reader")?;
        if let Some(stderr) = child.stderr.take() {
            let overruns = Arc::clone(&overruns);
            // Drain stderr on a background thread; log every non-empty
            // line as a daemon warning so SoX/CoreAudio errors surface
            // in the structured log instead of disappearing.
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader};
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if trimmed.contains("buffer overrun") {
                        // Overruns while capture is deliberately gated are
                        // backpressure we created ourselves — nobody drains
                        // the pipe during a gate, SoX blocks, CoreAudio
                        // discards. Expected, not a device fault: don't
                        // count them toward a respawn.
                        if crate::always::pause::should_gate_capture() {
                            continue;
                        }
                        let count = overruns.record();
                        // Avoid flooding the log — first + every 32nd.
                        if count == 1 || count.is_multiple_of(32) {
                            tracing::warn!(count, line = %trimmed, "rec_coreaudio_overrun");
                        }
                        continue;
                    }
                    tracing::warn!(line = %trimmed, "rec_stderr");
                }
            });
        }
        tracing::info!(
            pid = child.id(),
            buffer_bytes = SOX_BUFFER_BYTES,
            "rec_spawned"
        );
        Ok(Self {
            child,
            ring,
            last_captured_at: None,
            reuse_count: 0,
            spawned_at: std::time::Instant::now(),
            overruns,
        })
    }

    /// Wall-clock age of this recorder process.
    fn age(&self) -> std::time::Duration {
        self.spawned_at.elapsed()
    }

    pub fn get_or_spawn() -> Result<Arc<Mutex<Option<RecChild>>>> {
        let recorder_lock = Arc::clone(&GLOBAL_RECORDER);
        let mut recorder = recorder_lock.lock();

        let needs_respawn = match recorder.as_mut() {
            Some(rec) => {
                if rec.is_healthy() {
                    rec.reuse_count += 1;
                    // Time-based hygiene cap ONLY. The previous guard
                    // recycled after 1000 *calls* — but the gated loop
                    // calls get_or_spawn every 100 ms tick (wake-on-voice
                    // probe, pipe drain), so a user with media playing
                    // recycled a healthy recorder every ~100 s, each time
                    // paying the ~4.5 s device cold start and dropping
                    // audio mid-window. Real failure modes are covered
                    // below and in `is_healthy`: process death, wedge
                    // timeout, overrun storms, device switches. A daily
                    // quiet recycle remains as a conservative backstop
                    // against slow SoX memory growth.
                    rec.age() > REC_MAX_LIFETIME
                } else {
                    true
                }
            }
            None => true,
        };
        if needs_respawn {
            // Drop (kill + wait) the old recorder BEFORE spawning the
            // replacement, so two `rec` processes never hold the input
            // device at the same time — spawning first briefly ran both
            // against the same CoreAudio device.
            *recorder = None;
            *recorder = Some(Self::spawn()?);
        }
        Ok(Arc::clone(&GLOBAL_RECORDER))
    }

    /// Force-evict the current recorder and spawn a fresh one bound to
    /// whatever macOS now reports as the default input device.
    ///
    /// Triggered by the `RespawnRecorder` UDS command when the Swift app
    /// observes `kAudioHardwarePropertyDefaultInputDevice` change. The
    /// running `rec` opened the input device at spawn time and will not
    /// follow a default-input switch on its own, so without this the
    /// user would have to relaunch Always to use a newly selected mic.
    ///
    /// Same I4 "drop old before spawn new" discipline as
    /// [`get_or_spawn`]: the old `RecChild` is dropped (kill + wait)
    /// before the replacement is spawned, so two `rec` processes never
    /// hold the input device at the same time.
    pub fn force_respawn() -> Result<()> {
        let recorder_lock = Arc::clone(&GLOBAL_RECORDER);
        let mut recorder = recorder_lock.lock();
        if recorder.is_some() {
            tracing::info!("rec_respawn_due_to_default_input_change");
        }
        // A new input device gets a fresh chance at echo cancellation.
        CAPTURE_HELPER_FAILED.store(false, Ordering::Relaxed);
        CAPTURE_PLAIN_FAILED.store(false, Ordering::Relaxed);
        // Drop (kill + wait) the old recorder BEFORE spawning the
        // replacement — same I4 invariant as `get_or_spawn`.
        *recorder = None;
        *recorder = Some(Self::spawn()?);
        Ok(())
    }

    fn is_healthy(&mut self) -> bool {
        let overruns = self.overruns.windowed_count();
        if overruns >= REC_OVERRUN_RESPAWN_THRESHOLD {
            tracing::warn!(
                overruns,
                threshold = REC_OVERRUN_RESPAWN_THRESHOLD,
                window_secs = REC_OVERRUN_WINDOW_SECS,
                "rec_respawn_due_to_coreaudio_overruns"
            );
            return false;
        }
        // The reader thread only stops on EOF or a pipe read error; either
        // way this recorder will never deliver audio again, even if the
        // child process itself is still alive.
        if self.ring.is_closed() {
            return false;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => false, // Process has exited
            Ok(None) => true,     // Process still running
            Err(_) => false,      // Error checking process
        }
    }

    /// Throw away all audio captured so far, and report how many seconds
    /// of it were queued.
    ///
    /// `rec` never stops capturing — it runs for the daemon's lifetime and
    /// the reader thread keeps queueing frames. So while capture is gated
    /// (mic taken by another app, user muted, idle-paused) the ring fills
    /// with audio from exactly the period Always was supposed to be deaf.
    /// Without this, the first read after resuming returns that backlog
    /// and Always transcribes and pastes speech that belonged to the
    /// other app — the "I unblocked the mic and it pasted anyway" bug
    /// (I6). Frames captured before this call that are still inside SoX
    /// are discarded on arrival too.
    ///
    /// Never blocks.
    pub fn drain_pending(&mut self) -> f64 {
        let dropped_bytes = self.ring.discard_pending();
        // 16 kHz mono i16 = 32 000 bytes per second.
        dropped_bytes as f64 / 32_000.0
    }

    /// Next frame in capture order. `Ok(FRAME_BYTES)` = `buf` filled;
    /// `Ok(0)` = the recorder died (EOF); `Err(TimedOut)` = wedged.
    pub fn read_frame(&mut self, buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize> {
        let frame = self
            .ring
            .pop(Duration::from_millis(READ_FRAME_TIMEOUT_MS as u64));
        self.deliver(frame, buf)
    }

    /// Like [`Self::read_frame`], but skips any backlog and returns the
    /// most recently captured frame — for probes that ask "is someone
    /// speaking NOW?" and would otherwise judge audio seconds old.
    pub fn read_newest_frame(&mut self, buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize> {
        let frame = self
            .ring
            .pop_newest(Duration::from_millis(READ_FRAME_TIMEOUT_MS as u64));
        self.deliver(frame, buf)
    }

    /// The newest queued frame, left in the queue (wake-on-voice probe:
    /// "is someone speaking NOW?" without disturbing the next read).
    pub fn peek_newest_frame(&self) -> Option<[u8; FRAME_BYTES]> {
        self.ring.peek_newest()
    }

    /// Capture stamp of the frame last returned by a read — the clock
    /// latency metrics measure from, since "now" can be later than the
    /// moment the user actually spoke.
    pub fn last_captured_at(&self) -> Option<Instant> {
        self.last_captured_at
    }

    fn deliver(
        &mut self,
        frame: io::Result<Option<TimedFrame>>,
        buf: &mut [u8; FRAME_BYTES],
    ) -> io::Result<usize> {
        match frame {
            Ok(Some(frame)) => {
                buf.copy_from_slice(&frame.bytes);
                self.last_captured_at = Some(frame.captured_at);
                Ok(FRAME_BYTES)
            }
            Ok(None) => {
                // EOF on rec's stdout means the recorder died — either mic
                // permission was denied (TCC), the audio device was
                // unplugged, or the user killed `rec`.
                tracing::warn!("rec_eof_on_read_frame");
                Ok(0)
            }
            Err(e) => {
                tracing::warn!("rec_read_frame_timeout");
                Err(e)
            }
        }
    }
}

/// The running capture helper's stdin `(pid, pipe)`, for status cues.
static CAPTURE_CUE_SINK: Mutex<Option<(u32, std::process::ChildStdin)>> = Mutex::new(None);

/// Send one line to the capture helper's cue channel. False when no helper
/// is running (plain `rec`) or the pipe is closed — the caller falls back to
/// in-process playback.
pub fn capture_cue_send(line: &str) -> bool {
    use std::io::Write as _;
    let mut sink = CAPTURE_CUE_SINK.lock();
    let Some((_, stdin)) = sink.as_mut() else {
        return false;
    };
    if stdin
        .write_all(line.as_bytes())
        .and_then(|_| stdin.write_all(b"\n"))
        .and_then(|_| stdin.flush())
        .is_err()
    {
        *sink = None;
        return false;
    }
    true
}

/// Whether cues currently route through the capture helper.
pub fn capture_cue_available() -> bool {
    CAPTURE_CUE_SINK.lock().is_some()
}

/// Opt-in capture heartbeat (`ALWAYS_CAPTURE_HEARTBEAT=1`, used by the launch
/// smoke test): every 5 s, log how many frames were read and their RMS, so a
/// test can prove audio is actually flowing without needing speech.
fn capture_heartbeat(buf: &[u8]) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var("ALWAYS_CAPTURE_HEARTBEAT").as_deref() == Ok("1")) {
        return;
    }
    static FRAMES: AtomicU64 = AtomicU64::new(0);
    static SUM_SQ: parking_lot::Mutex<(f64, u64, Option<std::time::Instant>)> =
        parking_lot::Mutex::new((0.0, 0, None));
    // Counted in bytes (gated drains read partial frames); reported as
    // 30 ms frame equivalents.
    FRAMES.fetch_add(buf.len() as u64, Ordering::Relaxed);
    let mut acc = SUM_SQ.lock();
    for c in buf.as_chunks::<2>().0.iter() {
        let v = i16::from_le_bytes([c[0], c[1]]) as f64 / 32768.0;
        acc.0 += v * v;
        acc.1 += 1;
    }
    let now = std::time::Instant::now();
    let since = *acc.2.get_or_insert(now);
    if now.duration_since(since) >= std::time::Duration::from_secs(5) {
        let rms = (acc.0 / acc.1.max(1) as f64).sqrt();
        tracing::info!(
            frames_total = FRAMES.load(Ordering::Relaxed) / FRAME_BYTES as u64,
            rms,
            "capture_heartbeat"
        );
        *acc = (0.0, 0, Some(now));
    }
}

/// `Read` adapter that logs, once, how long the recorder took to deliver
/// its first bytes after spawn.
#[cfg(target_os = "macos")]
struct FirstRead<R> {
    inner: R,
    spawned_at: Instant,
    logged: bool,
}

#[cfg(target_os = "macos")]
impl<R: Read> Read for FirstRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if !self.logged && n > 0 {
            self.logged = true;
            tracing::info!(
                ms_since_spawn = self.spawned_at.elapsed().as_millis() as u64,
                "rec_first_audio"
            );
        }
        Ok(n)
    }
}

#[cfg(target_os = "macos")]
impl Drop for RecChild {
    fn drop(&mut self) {
        {
            let mut sink = CAPTURE_CUE_SINK.lock();
            if sink
                .as_ref()
                .is_some_and(|(pid, _)| *pid == self.child.id())
            {
                *sink = None;
            }
        }
        // Killing the child closes its stdout; the reader thread then
        // sees EOF, closes the ring and exits on its own.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Create WAV file data in memory from raw i16 mono samples at 16kHz (optimized)
pub fn create_wav_bytes_i16_mono_16k(samples: &[i16]) -> Result<Vec<u8>> {
    // Pre-calculate size to avoid reallocations
    let data_size = samples.len() * 2; // 2 bytes per i16 sample
    let file_size = 44 + data_size; // WAV header is 44 bytes

    let mut wav_data = Vec::with_capacity(file_size);

    // Write WAV header directly to buffer (faster than using hound for small files)
    wav_data.extend_from_slice(b"RIFF");
    wav_data.extend_from_slice(&((file_size - 8) as u32).to_le_bytes());
    wav_data.extend_from_slice(b"WAVE");
    wav_data.extend_from_slice(b"fmt ");
    wav_data.extend_from_slice(&16u32.to_le_bytes()); // PCM format chunk size
    wav_data.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav_data.extend_from_slice(&1u16.to_le_bytes()); // Mono
    wav_data.extend_from_slice(&RATE.to_le_bytes()); // Sample rate
    wav_data.extend_from_slice(&(RATE * 2).to_le_bytes()); // Byte rate
    wav_data.extend_from_slice(&2u16.to_le_bytes()); // Block align
    wav_data.extend_from_slice(&16u16.to_le_bytes()); // Bits per sample
    wav_data.extend_from_slice(b"data");
    wav_data.extend_from_slice(&(data_size as u32).to_le_bytes());

    // Bulk write all samples as little-endian bytes (safe on little-endian platforms)
    // SAFETY: i16 is 2 bytes, alignment of [i16] is 2, alignment of [u8] is 1.
    // Slice from_raw_parts is safe because we don't escape the borrow.
    #[cfg(target_endian = "little")]
    {
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(samples.as_ptr() as *const u8, samples.len() * 2) };
        wav_data.extend_from_slice(bytes);
    }
    #[cfg(not(target_endian = "little"))]
    {
        for sample in samples {
            wav_data.extend_from_slice(&sample.to_le_bytes());
        }
    }

    Ok(wav_data)
}

/// Pluggable audio frame source.
///
/// The default production implementation ([`SoxAudioSource`]) shells out
/// to `/opt/homebrew/bin/rec` (SoX) on macOS. The trait exists so:
///
/// 1. Tests can inject a deterministic `mock::MockAudioSource` without
///    touching the user's microphone.
/// 2. Future Linux (`cpal`/ALSA) and Windows (`cpal`/WASAPI) backends can
///    drop in without touching the VAD loop.
pub trait AudioFrameSource: Send {
    /// Read one VAD-sized frame ([`FRAME_BYTES`] bytes of little-endian
    /// 16-bit mono PCM at [`RATE`] Hz). Implementations should fill the
    /// buffer fully or return `Ok(0)` to signal EOF.
    fn read_frame(&mut self, buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize>;
}

/// Production source backed by the SoX `rec` command.
///
/// Wraps the existing global `RecChild` pool via [`RecChild::get_or_spawn`].
/// Acquires the recorder lazily on first frame read.
#[cfg(target_os = "macos")]
pub struct SoxAudioSource {
    handle: std::sync::Arc<Mutex<Option<RecChild>>>,
}

#[cfg(target_os = "macos")]
impl SoxAudioSource {
    pub fn new() -> Result<Self> {
        let handle = RecChild::get_or_spawn()?;
        Ok(Self { handle })
    }
}

#[cfg(target_os = "macos")]
impl AudioFrameSource for SoxAudioSource {
    fn read_frame(&mut self, buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize> {
        let mut guard = self.handle.lock();
        let Some(rec) = guard.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "audio recorder not initialized",
            ));
        };
        match rec.read_frame(buf) {
            // Wedged recorder: evict from the shared slot (Drop kills the
            // child) so the next `get_or_spawn` recovers with a fresh one.
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                *guard = None;
                tracing::warn!("rec_timeout_recorder_reset");
                Err(e)
            }
            other => other,
        }
    }
}

/// Linux/Windows stub. Real implementations land with the
/// `linux`/`windows` features in a follow-up.
#[cfg(not(target_os = "macos"))]
pub struct StubAudioSource;

#[cfg(not(target_os = "macos"))]
impl Default for StubAudioSource {
    fn default() -> Self {
        Self
    }
}

#[cfg(not(target_os = "macos"))]
impl AudioFrameSource for StubAudioSource {
    fn read_frame(&mut self, _buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "audio capture is not yet implemented for this platform",
        ))
    }
}

#[cfg(test)]
pub mod mock {
    //! In-memory test double for [`AudioFrameSource`].

    use super::{AudioFrameSource, FRAME_BYTES};
    use std::collections::VecDeque;
    use std::io;

    /// Returns canned frames in FIFO order; once exhausted yields EOF.
    pub struct MockAudioSource {
        frames: VecDeque<[u8; FRAME_BYTES]>,
    }

    impl MockAudioSource {
        pub fn new(frames: Vec<[u8; FRAME_BYTES]>) -> Self {
            Self {
                frames: frames.into(),
            }
        }

        /// Convenience: build a source of `n` silent frames.
        pub fn silence(n: usize) -> Self {
            Self::new(vec![[0u8; FRAME_BYTES]; n])
        }

        pub fn frames_remaining(&self) -> usize {
            self.frames.len()
        }
    }

    impl AudioFrameSource for MockAudioSource {
        fn read_frame(&mut self, buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize> {
            match self.frames.pop_front() {
                Some(frame) => {
                    buf.copy_from_slice(&frame);
                    Ok(FRAME_BYTES)
                }
                None => Ok(0),
            }
        }
    }
}

#[cfg(feature = "cpal")]
pub mod cpal_audio;

// Dictation capture. macOS reads the bundled capture helper
// (`always-capture`: Apple voice-processing echo cancellation, resampled
// to 16 kHz mono by AVAudioConverter) through `RecChild`; cpal is the
// capture path on the other platforms only. On macOS the cpal path took
// the device's native rate with no resampling (a 48 kHz-only mic was read
// as 16 kHz: slowed 3x, garbage transcripts), accepted mono configs only
// (stereo-only interfaces failed outright), dropped the tail of every
// callback buffer that was not a multiple of 480 samples, and ran beside
// the helper the gated loop spawns — two recorders on one mic.
#[cfg(all(feature = "cpal", not(target_os = "macos")))]
pub use cpal_audio::{CpalsAudioSource, force_respawn, get_or_spawn};

/// The process-wide dictation recorder (see the note above).
#[cfg(target_os = "macos")]
pub fn get_or_spawn() -> Result<Arc<Mutex<Option<RecChild>>>> {
    RecChild::get_or_spawn()
}

/// Rebind the dictation recorder to the current default input device.
#[cfg(target_os = "macos")]
pub fn force_respawn() -> Result<()> {
    RecChild::force_respawn()
}

pub fn temp_wav_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let counter = TEMP_WAV_COUNTER.fetch_add(1, Ordering::Relaxed);
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("always")
        .join(format!(
            "utterance-{}-{stamp}-{counter}.wav",
            std::process::id()
        ))
}

#[cfg(test)]
mod tests {
    use super::{FRAME_BYTES, FrameRing, RING_CAPACITY_FRAMES, TimedFrame, pump_frames};
    use super::{create_wav_bytes_i16_mono_16k, temp_wav_path};
    use std::io;
    use std::time::{Duration, Instant};

    fn frame(tag: u16, captured_at: Instant) -> TimedFrame {
        let mut bytes = [0u8; FRAME_BYTES];
        bytes[..2].copy_from_slice(&tag.to_le_bytes());
        TimedFrame { bytes, captured_at }
    }

    fn tag(f: &TimedFrame) -> u16 {
        u16::from_le_bytes([f.bytes[0], f.bytes[1]])
    }

    #[test]
    fn pump_delivers_whole_frames_in_order_then_eof() {
        let mut data = Vec::new();
        for i in 0..3u16 {
            let mut b = [0u8; FRAME_BYTES];
            b[..2].copy_from_slice(&i.to_le_bytes());
            data.extend_from_slice(&b);
        }
        data.extend_from_slice(&[7u8; 100]); // trailing partial frame
        let ring = FrameRing::new();
        pump_frames(io::Cursor::new(data), &ring);

        for want in 0..3u16 {
            let got = ring.pop(Duration::from_millis(10)).unwrap().unwrap();
            assert_eq!(tag(&got), want);
        }
        // The partial frame is never delivered; the closed ring reports EOF.
        assert!(ring.pop(Duration::from_millis(10)).unwrap().is_none());
    }

    #[test]
    fn pump_backdates_frames_that_arrived_in_one_burst() {
        // One read of 16 KiB = 17 whole frames delivered at once — the
        // shape of a SoX output block. The oldest frame must be stamped
        // 16 × 30 ms before the newest, not "now" like all the others.
        let ring = FrameRing::new();
        pump_frames(io::Cursor::new(vec![0u8; 16 * 1024]), &ring);
        let mut stamps = Vec::new();
        while let Ok(Some(f)) = ring.pop(Duration::from_millis(10)) {
            stamps.push(f.captured_at);
        }
        assert_eq!(stamps.len(), 17);
        assert!(stamps.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(stamps[16] - stamps[0], Duration::from_millis(16 * 30));
    }

    #[test]
    fn pop_times_out_when_nothing_arrives() {
        let ring = FrameRing::new();
        let started = Instant::now();
        let err = ring.pop(Duration::from_millis(60)).err().expect("timeout");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(55));
    }

    #[test]
    fn full_ring_drops_the_oldest_frames() {
        let ring = FrameRing::new();
        let now = Instant::now();
        for i in 0..(RING_CAPACITY_FRAMES + 5) as u16 {
            ring.push(frame(i, now));
        }
        assert_eq!(ring.state.lock().overflow_run, 5);
        let first = ring.pop(Duration::from_millis(10)).unwrap().unwrap();
        assert_eq!(tag(&first), 5);
        assert_eq!(ring.state.lock().overflowed, 5);
        // Consuming again ends the overflow episode.
        assert_eq!(ring.state.lock().overflow_run, 0);
    }

    #[test]
    fn discard_pending_drops_backlog_and_audio_captured_before_it() {
        let ring = FrameRing::new();
        let before = Instant::now();
        ring.push(frame(1, before));
        ring.push(frame(2, before));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(ring.discard_pending(), 2 * FRAME_BYTES);
        // Still inside SoX when the gate lifted: captured before the
        // cut, delivered after it. Must be dropped (I6).
        ring.push(frame(3, before));
        // Captured after the cut: kept.
        ring.push(frame(4, Instant::now()));
        let got = ring.pop(Duration::from_millis(10)).unwrap().unwrap();
        assert_eq!(tag(&got), 4);
    }

    #[test]
    fn pop_newest_skips_the_backlog() {
        let ring = FrameRing::new();
        let now = Instant::now();
        for i in 0..5u16 {
            ring.push(frame(i, now));
        }
        let got = ring.pop_newest(Duration::from_millis(10)).unwrap().unwrap();
        assert_eq!(tag(&got), 4);
        assert!(ring.pop(Duration::from_millis(20)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn silent_child_pipe_times_out_instead_of_hanging() {
        use std::process::{Command, Stdio};
        use std::sync::Arc;

        // `sleep` holds its stdout open without writing — the exact shape
        // of a wedged recorder (no bytes, no EOF).
        let mut child = Command::new("sleep")
            .arg("5")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn sleep");
        let stdout = child.stdout.take().expect("stdout");
        let ring = Arc::new(FrameRing::new());
        let pump_ring = Arc::clone(&ring);
        let pump = std::thread::spawn(move || pump_frames(stdout, &pump_ring));

        let err = ring.pop(Duration::from_millis(150)).err().expect("timeout");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);

        // Killing the child ends the reader thread via EOF.
        let _ = child.kill();
        let _ = child.wait();
        pump.join().expect("pump thread");
        assert!(ring.pop(Duration::from_millis(10)).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn child_output_is_framed_until_eof() {
        use std::process::{Command, Stdio};

        let mut child = Command::new("head")
            .args(["-c", &(2 * FRAME_BYTES).to_string(), "/dev/zero"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn head");
        let stdout = child.stdout.take().expect("stdout");
        let ring = FrameRing::new();
        pump_frames(stdout, &ring);
        let _ = child.wait();
        assert!(ring.pop(Duration::from_millis(10)).unwrap().is_some());
        assert!(ring.pop(Duration::from_millis(10)).unwrap().is_some());
        assert!(ring.pop(Duration::from_millis(10)).unwrap().is_none());
    }

    /// A capture helper that dies at start (e.g. VPIO unavailable → exit 4)
    /// must be rejected promptly so `spawn` falls back to `rec`, and must
    /// not be left running (I4: one recorder).
    #[cfg(target_os = "macos")]
    #[test]
    fn capture_helper_that_exits_is_rejected_fast() {
        let started = std::time::Instant::now();
        let r = super::RecChild::spawn_capture_helper(std::path::Path::new("/usr/bin/false"), &[]);
        assert!(r.is_err(), "an exiting helper must not become the recorder");
        assert!(
            started.elapsed().as_millis() < 2_000,
            "EOF must not wait out the timeout"
        );
    }

    /// A helper that streams bytes is accepted and its frames are readable
    /// through the normal `read_frame` path.
    #[cfg(target_os = "macos")]
    #[test]
    fn capture_helper_that_streams_is_accepted() {
        let mut rec =
            super::RecChild::spawn_capture_helper(std::path::Path::new("/usr/bin/yes"), &[])
                .expect("streaming helper accepted");
        let mut buf = [0u8; super::FRAME_BYTES];
        assert_eq!(rec.read_frame(&mut buf).expect("read"), super::FRAME_BYTES);
    }

    /// A helper that never writes is rejected after the first-bytes budget.
    #[cfg(target_os = "macos")]
    #[test]
    fn capture_helper_that_stays_silent_is_rejected() {
        let dir = std::env::temp_dir().join(format!("always-cap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("silent-helper");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = super::RecChild::spawn_capture_helper(&script, &[]);
        assert!(r.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `CAPTURE_HELPER_FAILED` / `CAPTURE_PLAIN_FAILED` are process-global;
    /// the spawn tests below set and assert them, so they must not
    /// interleave under the parallel test runner.
    #[cfg(target_os = "macos")]
    static CAPTURE_FLAGS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A helper whose echo-cancelling mode fails (VPIO unavailable — seen
    /// on a fresh macOS 26 VM: kAUInitialize -10875) must fall back to the
    /// SAME helper in plain `--no-vp` mode, never to SoX `rec` (absent on a
    /// new user's Mac). Stand-in helper: exits 4 unless given `--no-vp`.
    #[cfg(target_os = "macos")]
    #[test]
    fn helper_vp_failure_falls_back_to_plain_helper_not_rec() {
        use std::sync::atomic::Ordering;
        let _guard = CAPTURE_FLAGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("always-cap-plain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-capture");
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$1\" in --no-vp) exec /usr/bin/yes ;; *) exit 4 ;; esac\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::CAPTURE_HELPER_FAILED.store(false, Ordering::Relaxed);
        super::CAPTURE_PLAIN_FAILED.store(false, Ordering::Relaxed);

        let mut rec = super::RecChild::spawn_from(Some(script.clone()), true)
            .expect("plain helper must take over");
        assert!(
            super::CAPTURE_HELPER_FAILED.load(Ordering::Relaxed),
            "VP failure recorded"
        );
        assert!(
            !super::CAPTURE_PLAIN_FAILED.load(Ordering::Relaxed),
            "plain mode healthy"
        );
        let mut buf = [0u8; super::FRAME_BYTES];
        assert_eq!(rec.read_frame(&mut buf).expect("read"), super::FRAME_BYTES);
        assert!(rec.peek_newest_frame().is_some() || rec.read_frame(&mut buf).is_ok());
        drop(rec);
        super::CAPTURE_HELPER_FAILED.store(false, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// v0.1.0 report (micro-cuts / crackling in music while Always ran): the
    /// voice-processing canceller restarts the output device's IO and ducks
    /// every other app, so it must never run unless asked for. Unset pref
    /// = off; only an explicit opt-in (pref, `ALWAYS_AEC=1`, a test
    /// `ALWAYS_CAPTURE_BIN`) turns it on, and `ALWAYS_AEC=0` always wins.
    #[test]
    fn echo_cancellation_is_opt_in() {
        use super::aec_decision as d;
        assert!(!d(None, false, false), "default: no VPIO");
        assert!(d(None, false, true), "pref opt-in");
        assert!(d(Some("1"), false, false), "env opt-in");
        assert!(!d(Some("0"), true, true), "env kill switch wins");
        assert!(d(None, true, false), "test helper implies on");
        assert_eq!(super::capture_helper_args(false), &["--no-vp"]);
        assert_eq!(super::capture_helper_args(true), &["--vp"]);
    }

    /// Default (AEC off): the helper is started ONCE, in plain mode, with
    /// an explicit `--no-vp` — never first as a voice-processing unit
    /// (which alone restarts the output device's IO: a cut in the user's
    /// music). Stand-in helper records its arguments.
    #[cfg(target_os = "macos")]
    #[test]
    fn default_spawn_is_plain_helper_without_vpio() {
        use std::sync::atomic::Ordering;
        let _guard = CAPTURE_FLAGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("always-cap-default-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-capture");
        let argv = dir.join("argv");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1\" in --no-vp) exec /usr/bin/yes ;; *) exit 4 ;; esac\n",
                argv.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::CAPTURE_HELPER_FAILED.store(false, Ordering::Relaxed);
        super::CAPTURE_PLAIN_FAILED.store(false, Ordering::Relaxed);

        let mut rec =
            super::RecChild::spawn_from(Some(script.clone()), false).expect("plain helper starts");
        let mut buf = [0u8; super::FRAME_BYTES];
        assert_eq!(rec.read_frame(&mut buf).expect("read"), super::FRAME_BYTES);
        drop(rec);
        assert_eq!(
            std::fs::read_to_string(&argv).unwrap(),
            "--no-vp\n",
            "exactly one spawn, plain"
        );
        assert!(!super::CAPTURE_HELPER_FAILED.load(Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live: the real echo-cancelling helper delivers 16 kHz mono s16 at
    /// real-time rate. Needs microphone access; run with --ignored.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn live_capture_helper_streams_16k_mono() {
        let Some(path) = super::locate_capture_helper() else {
            panic!("always-capture not built (capture/build.sh)");
        };
        let mut rec =
            super::RecChild::spawn_capture_helper(&path, &["--vp"]).expect("helper starts");
        let mut buf = [0u8; super::FRAME_BYTES];
        // Discard start-up burst, then time 100 frames (3.0 s of audio).
        for _ in 0..10 {
            rec.read_frame(&mut buf).unwrap();
        }
        let t = std::time::Instant::now();
        for _ in 0..100 {
            assert_eq!(rec.read_frame(&mut buf).unwrap(), super::FRAME_BYTES);
        }
        let secs = t.elapsed().as_secs_f64();
        eprintln!("100 frames (3.0 s audio) in {secs:.2} s wall");
        assert!((2.6..3.4).contains(&secs), "not real-time 16 kHz: {secs}");
    }

    /// Live: with a broken helper (run with ALWAYS_CAPTURE_BIN=/usr/bin/false)
    /// `spawn` falls back to SoX `rec`, marks the helper failed, and audio
    /// still flows. Needs microphone access; run with --ignored.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn live_spawn_falls_back_to_rec_when_helper_fails() {
        if std::env::var("ALWAYS_CAPTURE_BIN").as_deref() != Ok("/usr/bin/false") {
            eprintln!("skipped: set ALWAYS_CAPTURE_BIN=/usr/bin/false");
            return;
        }
        let mut rec = super::RecChild::spawn().expect("fallback recorder");
        assert!(super::CAPTURE_HELPER_FAILED.load(std::sync::atomic::Ordering::Relaxed));
        let mut buf = [0u8; super::FRAME_BYTES];
        assert_eq!(rec.read_frame(&mut buf).unwrap(), super::FRAME_BYTES);
        eprintln!("fallback rec pid streaming");
    }

    #[test]
    fn temp_wav_paths_are_unique() {
        let first = temp_wav_path();
        let second = temp_wav_path();
        assert_ne!(first, second);
    }

    #[test]
    fn wav_bytes_creation_works() {
        let samples = vec![100, -100, 200, -200];
        let wav_data = create_wav_bytes_i16_mono_16k(&samples).unwrap();
        assert!(wav_data.len() > 44); // At least WAV header size
        assert!(wav_data.starts_with(b"RIFF"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn overrun_window_counts_within_window_and_ages_out() {
        use super::{OverrunWindow, REC_OVERRUN_RESPAWN_THRESHOLD, REC_OVERRUN_WINDOW_SECS};
        use std::sync::atomic::Ordering;

        let w = OverrunWindow::new();
        assert_eq!(w.windowed_count(), 0);
        for i in 1..=REC_OVERRUN_RESPAWN_THRESHOLD {
            assert_eq!(w.record(), i);
        }
        assert_eq!(w.windowed_count(), REC_OVERRUN_RESPAWN_THRESHOLD);

        // Age the window out: a stale window reports 0 (healthy) and the
        // next overrun restarts the count from 1 instead of accumulating
        // forever — the lifetime-counter behavior this replaced.
        w.window_start.store(
            OverrunWindow::now_secs() - REC_OVERRUN_WINDOW_SECS,
            Ordering::Relaxed,
        );
        assert_eq!(w.windowed_count(), 0);
        assert_eq!(w.record(), 1);
        assert_eq!(w.windowed_count(), 1);
    }

    #[test]
    fn audio_buffer_get_drop_get_does_not_leak() {
        use super::AudioBuffer;
        // Pool is process-global so other parallel tests share it; we
        // can't assert capacity reuse deterministically. Assert the basic
        // contract: get + drop + get does not panic and produces usable
        // buffers.
        let cap_before;
        {
            let mut buf = AudioBuffer::get();
            buf.as_mut().reserve(32_768);
            cap_before = buf.as_mut().capacity();
        }
        let _buf = AudioBuffer::get();
        assert!(cap_before >= 32_768);
    }

    #[test]
    fn audio_buffer_pool_caps_at_ten_entries() {
        use super::AudioBuffer;
        // Deluge the pool with 20 returns. The pool length is capped at 10
        // by Drop logic. We can only observe via the absence of OOM and
        // that subsequent gets succeed.
        let mut bufs = Vec::new();
        for _ in 0..20 {
            bufs.push(AudioBuffer::get());
        }
        drop(bufs);
        // Confirm a fresh get still works after the deluge.
        let _ = AudioBuffer::get();
    }

    #[test]
    fn mock_audio_source_yields_frames_then_eof() {
        use super::AudioFrameSource;
        use super::FRAME_BYTES;
        use super::mock::MockAudioSource;

        let mut src = MockAudioSource::silence(2);
        let mut buf = [0u8; FRAME_BYTES];

        assert_eq!(src.read_frame(&mut buf).unwrap(), FRAME_BYTES);
        assert_eq!(src.read_frame(&mut buf).unwrap(), FRAME_BYTES);
        // After exhaustion: EOF (Ok(0)).
        assert_eq!(src.read_frame(&mut buf).unwrap(), 0);
    }

    #[test]
    fn mock_audio_source_preserves_frame_contents() {
        use super::AudioFrameSource;
        use super::FRAME_BYTES;
        use super::mock::MockAudioSource;

        let mut frame_a = [0u8; FRAME_BYTES];
        frame_a[0] = 0xAB;
        frame_a[1] = 0xCD;
        let mut src = MockAudioSource::new(vec![frame_a]);

        let mut buf = [0u8; FRAME_BYTES];
        assert_eq!(src.read_frame(&mut buf).unwrap(), FRAME_BYTES);
        assert_eq!(buf[0], 0xAB);
        assert_eq!(buf[1], 0xCD);
    }
}

#[cfg(test)]
mod aec_edge_tracking_tests {
    #[test]
    fn only_output_triggered_engage_lines_set_the_edge() {
        use super::{aec_output_engaged_at, note_capture_helper_line_for_test as note};
        let before = aec_output_engaged_at();
        note("always-capture: aec engaged (cue) tap=7ch@44100");
        note("always-capture: aec bypassed (no other output) tap=7ch@44100");
        assert_eq!(aec_output_engaged_at(), before);
        let t = std::time::Instant::now();
        note(
            "always-capture: aec engaged (output pids [\"1252:com.apple.WebKit.GPU\"]) tap=7ch@44100",
        );
        let got = aec_output_engaged_at().expect("edge recorded");
        assert!(got + std::time::Duration::from_millis(5) >= t && got <= std::time::Instant::now());
    }
}
