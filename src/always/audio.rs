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
    /// Fed by the stderr drainer on CoreAudio buffer overruns.
    overruns: Arc<OverrunWindow>,
}

#[cfg(target_os = "macos")]
impl RecChild {
    pub fn spawn() -> Result<Self> {
        tracing::info!("rec_spawn_starting");
        let overruns = Arc::new(OverrunWindow::new());
        let rec_path = if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/rec"
        } else {
            "/usr/bin/rec"
        };
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
            .with_context(|| format!("Failed to run '{rec_path}'. Install SoX"))?;
        let stdout = child.stdout.take().context("sox stdout missing")?;
        let spawned_at = Instant::now();
        let ring = Arc::new(FrameRing::new());
        {
            // Dedicated reader: drains the pipe continuously so SoX never
            // blocks on write, whatever the event-loop thread is doing.
            let ring = Arc::clone(&ring);
            std::thread::Builder::new()
                .name("rec-reader".into())
                .spawn(move || {
                    // Log when the first audio lands: the device cold
                    // start, measured instead of assumed.
                    let mut first = FirstRead {
                        inner: stdout,
                        spawned_at,
                        logged: false,
                    };
                    pump_frames(&mut first, &ring);
                })
                .context("failed to start rec reader thread")?;
        }
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
            overruns,
        })
    }

    pub fn get_or_spawn() -> Result<Arc<Mutex<Option<RecChild>>>> {
        let recorder_lock = Arc::clone(&GLOBAL_RECORDER);
        let mut recorder = recorder_lock.lock();

        let needs_respawn = match recorder.as_mut() {
            Some(rec) => {
                if rec.is_healthy() {
                    rec.reuse_count += 1;
                    // Restart every 1000 uses to prevent memory leaks
                    rec.reuse_count > 1000
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

#[cfg(feature = "cpal")]
pub use cpal_audio::{CpalsAudioSource, force_respawn, get_or_spawn};

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
