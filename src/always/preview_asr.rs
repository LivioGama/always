//! Live-preview ASR helper: Parakeet EOU 160 ms on the Neural Engine.
//!
//! The model runs in a separate process (`always-preview-asr`, Swift +
//! FluidAudio, source in `preview-asr/`) so its CoreML/ANE state, memory
//! and any crash stay out of the daemon. It is spawned once, warmed, and
//! kept resident; each utterance is a `begin`/`audio…`/`end` sequence
//! tagged with a sequence number so late output from a finished utterance
//! is never shown under the next one.
//!
//! **Preview only.** Text from this engine is never pasted and never used
//! as a final — see SPEC.md §5 — except as the rescue for a chunked tail
//! whose Apple pass failed ([`PreviewAsr::transcribe_span`], SPEC.md §4). English only: other languages keep the
//! Apple recognizer's own partials.
//!
//! Wire protocol: see the header of `preview-asr/Sources/.../main.swift`.
//! Audio is handed to a writer thread through a bounded channel with
//! `try_send`, so a stalled helper can never block the capture loop — the
//! preview just drops frames.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Executable name inside `Always.app/Contents/MacOS/`.
pub const HELPER_NAME: &str = "always-preview-asr";

/// Frames buffered toward the helper before new audio is dropped (~3 s of
/// 30 ms frames). Generous: the helper decodes faster than real time.
const WRITE_QUEUE_FRAMES: usize = 100;

/// Minimum gap between respawn attempts after the helper died.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(30);

/// Output of the helper for one utterance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewEvent {
    Partial(String),
    Final(String),
}

pub type PreviewSink = Arc<dyn Fn(PreviewEvent) + Send + Sync>;

enum Frame {
    Begin(u32),
    Audio(u32, Vec<i16>),
    End(u32),
}

fn encode(frame: &Frame) -> Vec<u8> {
    let (op, seq, payload): (u8, u32, &[i16]) = match frame {
        Frame::Begin(s) => (b'B', *s, &[]),
        Frame::Audio(s, samples) => (b'A', *s, samples.as_slice()),
        Frame::End(s) => (b'E', *s, &[]),
    };
    let mut out = Vec::with_capacity(9 + payload.len() * 2);
    out.push(op);
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&((payload.len() * 2) as u32).to_le_bytes());
    for s in payload {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Parse one stdout line of the helper. Returns `(seq, event)` for
/// utterance output, `None` for anything else (ready/error/garbage).
fn parse_line(line: &str) -> Option<(u32, PreviewEvent)> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let seq = v.get("seq")?.as_u64()? as u32;
    let text = v.get("text")?.as_str()?.trim().to_string();
    match v.get("ev")?.as_str()? {
        "partial" => Some((seq, PreviewEvent::Partial(text))),
        "final" => Some((seq, PreviewEvent::Final(text))),
        _ => None,
    }
}

struct Process {
    child: Child,
    tx: SyncSender<Frame>,
}

pub struct PreviewAsr {
    bin: std::path::PathBuf,
    proc_: Mutex<Option<Process>>,
    alive: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    last_spawn: Mutex<Option<Instant>>,
    next_seq: AtomicU32,
    /// Sink for the utterance currently being decoded.
    current: Arc<Mutex<Option<(u32, PreviewSink)>>>,
    dropped_frames: Arc<AtomicU64>,
}

impl PreviewAsr {
    fn new(bin: std::path::PathBuf) -> Arc<Self> {
        Arc::new(Self {
            bin,
            proc_: Mutex::new(None),
            alive: Arc::new(AtomicBool::new(false)),
            ready: Arc::new(AtomicBool::new(false)),
            last_spawn: Mutex::new(None),
            next_seq: AtomicU32::new(1),
            current: Arc::new(Mutex::new(None)),
            dropped_frames: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Spawn the helper if it is not running (respecting the backoff).
    fn ensure_spawned(&self) -> bool {
        if self.alive.load(Ordering::Acquire) {
            return true;
        }
        {
            let mut last = self.last_spawn.lock();
            if last.is_some_and(|t| t.elapsed() < RESPAWN_BACKOFF) {
                return false;
            }
            *last = Some(Instant::now());
        }
        let mut guard = self.proc_.lock();
        if let Some(mut old) = guard.take() {
            let _ = old.child.kill();
            let _ = old.child.wait();
        }
        let spawned = Command::new(&self.bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(error) => {
                tracing::warn!(error = %error, bin = %self.bin.display(), "preview_asr_spawn_failed");
                return false;
            }
        };
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = mpsc::sync_channel::<Frame>(WRITE_QUEUE_FRAMES);
        self.alive.store(true, Ordering::Release);
        self.ready.store(false, Ordering::Release);
        let spawned_at = Instant::now();

        let alive = Arc::clone(&self.alive);
        std::thread::Builder::new()
            .name("preview-asr-writer".into())
            .spawn(move || writer_loop(stdin, rx, alive))
            .ok();

        let alive = Arc::clone(&self.alive);
        let ready = Arc::clone(&self.ready);
        let current = Arc::clone(&self.current);
        std::thread::Builder::new()
            .name("preview-asr-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if let Some((seq, event)) = parse_line(&line) {
                        // Clone the sink out so it never runs under the lock.
                        let sink = current
                            .lock()
                            .as_ref()
                            .filter(|(s, _)| *s == seq)
                            .map(|(_, sink)| Arc::clone(sink));
                        if let Some(sink) = sink {
                            sink(event);
                        }
                    } else if line.contains("\"ev\":\"ready\"") {
                        ready.store(true, Ordering::Release);
                        tracing::info!(
                            line = %line.trim(),
                            since_spawn_ms = spawned_at.elapsed().as_millis() as u64,
                            "preview_asr_ready"
                        );
                    } else if line.contains("\"ev\":\"error\"") {
                        tracing::warn!(line = %line.trim(), "preview_asr_error");
                    }
                }
                alive.store(false, Ordering::Release);
                ready.store(false, Ordering::Release);
                tracing::warn!("preview_asr_exited");
            })
            .ok();

        tracing::info!(pid = child.id(), bin = %self.bin.display(), "preview_asr_spawned");
        *guard = Some(Process { child, tx });
        true
    }

    fn send(&self, frame: Frame) -> bool {
        let guard = self.proc_.lock();
        let Some(p) = guard.as_ref() else {
            return false;
        };
        match p.tx.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.dropped_frames.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.alive.load(Ordering::Acquire) && self.ready.load(Ordering::Acquire)
    }

    /// Begin a new utterance. `None` when the helper is not ready — the
    /// caller then simply has no Parakeet preview for this utterance.
    pub fn begin(self: &Arc<Self>, sink: PreviewSink) -> Option<PreviewUtterance> {
        if !self.ensure_spawned() || !self.is_ready() {
            return None;
        }
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed).max(1);
        *self.current.lock() = Some((seq, sink));
        if !self.send(Frame::Begin(seq)) {
            *self.current.lock() = None;
            return None;
        }
        Some(PreviewUtterance {
            asr: Arc::clone(self),
            seq,
            started: Instant::now(),
            samples: 0,
            level: LevelNormalizer::new(),
        })
    }

    /// Recover the words of one finished audio span when the pasting
    /// recognizer failed on it (SPEC.md §4, chunked tail): feed the whole
    /// span as one utterance and wait up to `timeout` for the helper's
    /// `final`. `None` when the helper is not already warm (never spawns
    /// or waits for a cold load), a frame could not be queued, or no
    /// final arrived in time.
    pub fn transcribe_span(self: &Arc<Self>, samples: &[i16], timeout: Duration) -> Option<String> {
        if samples.is_empty() || !self.is_ready() {
            return None;
        }
        let (tx, rx) = mpsc::sync_channel::<String>(1);
        let mut utt = self.begin(Arc::new(move |ev| {
            if let PreviewEvent::Final(text) = ev {
                let _ = tx.try_send(text);
            }
        }))?;
        // ~1 s frames: a 15 s tail is 15 of the 100 queue slots.
        let mut queued = true;
        for part in samples.chunks(16_000) {
            utt.samples += part.len();
            let boosted = utt.level.process(part);
            queued &= self.send(Frame::Audio(utt.seq, boosted));
        }
        // End WITHOUT detaching the sink (Drop detaches first, which would
        // discard the very `final` this call waits for).
        queued &= self.send(Frame::End(utt.seq));
        let seq = utt.seq;
        std::mem::forget(utt);
        let text = if queued {
            rx.recv_timeout(timeout).ok()
        } else {
            None
        };
        let mut cur = self.current.lock();
        if cur.as_ref().is_some_and(|(s, _)| *s == seq) {
            *cur = None;
        }
        text
    }
}

fn writer_loop(mut stdin: ChildStdin, rx: mpsc::Receiver<Frame>, alive: Arc<AtomicBool>) {
    for frame in rx {
        if stdin.write_all(&encode(&frame)).is_err() || stdin.flush().is_err() {
            alive.store(false, Ordering::Release);
            break;
        }
    }
}

/// Speech level the preview engine is fed at: per-30 ms-frame RMS envelope
/// of −20 dBFS. Parakeet EOU has a hard input-level floor — measured
/// 2026-10-14 on a 16 kHz TTS sentence fed at real time through the
/// helper: utterance RMS −24.6/−27.4/−32.6 dBFS → first partial 401–409 ms,
/// −38.4 and −48.8 dBFS → NO text at all. Apple's recognizer, the VAD and
/// the speaker gate have no such floor, so only this feed is normalised.
const PREVIEW_TARGET_RMS: f32 = 0.1 * 32768.0;
/// Never boost more than +24 dB (room tone stays room tone).
const PREVIEW_MAX_GAIN: f32 = 15.85;
/// Frames quieter than −50 dBFS RMS never change the gain. Measured room
/// tone on the user's MacBook Air: −73…−83 dBFS bypassed, −55…−70 dBFS
/// through voice processing; quiet speech frames sit above −50.
const PREVIEW_VOICE_GATE: f32 = 0.003_16 * 32768.0;
/// Envelope release per 30 ms frame (≈ −10 dB/s); attack is instant.
const PREVIEW_ENV_RELEASE: f32 = 0.966;
const PREVIEW_GAIN_FRAME: usize = 480;

/// Level normaliser for the Parakeet feed. Why it exists: the capture
/// helper bypasses voice processing while nothing else plays, and the
/// bypassed (raw-mic) signal is quieter than the processed one — on the
/// user's MacBook Air the post-bypass utterances landed at −29…−37 dBFS
/// against a −23 dBFS median before, under Parakeet's floor. Boost-only
/// (gain ≥ 1): speech whose loud frames already reach −20 dBFS is passed
/// through bit-identical.
#[derive(Debug, Clone)]
pub(crate) struct LevelNormalizer {
    env: f32,
    gain: f32,
    peak_env: f32,
}

impl LevelNormalizer {
    pub(crate) fn new() -> Self {
        Self {
            env: 0.0,
            gain: 1.0,
            peak_env: 0.0,
        }
    }

    /// Gain currently applied (linear).
    pub(crate) fn gain(&self) -> f32 {
        self.gain
    }

    /// Loudest per-frame RMS seen (i16 units).
    pub(crate) fn peak_env(&self) -> f32 {
        self.peak_env
    }

    pub(crate) fn process(&mut self, samples: &[i16]) -> Vec<i16> {
        if boost_disabled() {
            return samples.to_vec();
        }
        let frame_rms = |frame: &[i16]| {
            (frame.iter().map(|&s| (s as f32) * (s as f32)).sum::<f32>()
                / frame.len().max(1) as f32)
                .sqrt()
        };
        // Look-ahead over the whole block. The utterance opens with the
        // daemon's pre-roll (~480 ms handed over in one call): its
        // first frames are the quiet rise of the first word, and sizing
        // the gain on them alone pumped it to +24 dB right before the
        // loud syllable — which then clipped (measured −2.7 dBFS frames)
        // and Parakeet lost the first word. Seeding the envelope with the
        // block's loudest voiced frame sizes the gain on the word itself.
        let block_peak = samples
            .chunks(PREVIEW_GAIN_FRAME)
            .map(frame_rms)
            .fold(0.0f32, f32::max);
        if block_peak >= PREVIEW_VOICE_GATE && block_peak > self.env {
            self.env = block_peak;
        }
        let mut out = Vec::with_capacity(samples.len());
        for frame in samples.chunks(PREVIEW_GAIN_FRAME) {
            let rms = frame_rms(frame);
            self.peak_env = self.peak_env.max(rms);
            self.env = if rms >= self.env {
                rms
            } else {
                (self.env * PREVIEW_ENV_RELEASE).max(rms)
            };
            // Only plausible voice moves the gain: room tone holds it, so
            // a word onset after a pause is never hit by a gain that
            // drifted up on silence.
            let mut target = if rms >= PREVIEW_VOICE_GATE {
                (PREVIEW_TARGET_RMS / self.env).clamp(1.0, PREVIEW_MAX_GAIN)
            } else {
                self.gain
            };
            // Never clip: the frame's own peak bounds the gain (−0.3 dBFS).
            let peak = frame
                .iter()
                .map(|&s| (s as i32).unsigned_abs())
                .max()
                .unwrap_or(0) as f32;
            if peak > 0.0 {
                target = target.min((32_000.0 / peak).max(1.0));
            }
            // Cuts apply to the whole frame at once (the frame is known
            // before it is scaled, so this is look-ahead, not a click);
            // raises ramp across the frame.
            let g0 = if target < self.gain {
                target
            } else {
                self.gain
            };
            let n = frame.len() as f32;
            for (i, &s) in frame.iter().enumerate() {
                let g = g0 + (target - g0) * ((i + 1) as f32 / n);
                out.push(
                    (s as f32 * g)
                        .round()
                        .clamp(i16::MIN as f32, i16::MAX as f32) as i16,
                );
            }
            self.gain = target;
        }
        out
    }
}

/// `ALWAYS_PREVIEW_BOOST=0` turns the Parakeet level boost off (A/B in the
/// field without a rebuild).
fn boost_disabled() -> bool {
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var("ALWAYS_PREVIEW_BOOST").as_deref() == Ok("0"))
}

/// One utterance's feed into the helper. Dropping it ends the utterance.
pub struct PreviewUtterance {
    asr: Arc<PreviewAsr>,
    seq: u32,
    started: Instant,
    samples: usize,
    level: LevelNormalizer,
}

impl PreviewUtterance {
    pub fn feed(&mut self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        self.samples += samples.len();
        let boosted = self.level.process(samples);
        self.asr.send(Frame::Audio(self.seq, boosted));
    }
}

impl Drop for PreviewUtterance {
    fn drop(&mut self) {
        // Detach the sink first: once the utterance is over, nothing the
        // helper still says about it may reach the overlay.
        {
            let mut cur = self.asr.current.lock();
            if cur.as_ref().is_some_and(|(s, _)| *s == self.seq) {
                *cur = None;
            }
        }
        self.asr.send(Frame::End(self.seq));
        // Info, once per utterance: the input level and the boost applied
        // are what tell a quiet-mic miss from a helper fault in the log.
        let db = |v: f32| {
            if v > 0.0 {
                (20.0 * (v / 32768.0).log10() * 10.0).round() / 10.0
            } else {
                -120.0
            }
        };
        tracing::info!(
            seq = self.seq,
            audio_secs = self.samples as f64 / 16_000.0,
            wall_ms = self.started.elapsed().as_millis() as u64,
            dropped_frames = self.asr.dropped_frames.load(Ordering::Relaxed),
            peak_frame_dbfs = db(self.level.peak_env()),
            end_gain_db = (20.0 * self.level.gain().log10() * 10.0).round() / 10.0,
            "preview_asr_utterance_end"
        );
    }
}

/// Locate the helper binary: `ALWAYS_PREVIEW_ASR_BIN`, then next to the
/// daemon executable (the app bundle's `Contents/MacOS`), then the repo's
/// SwiftPM build output (dev/test runs from `target/`).
pub fn locate_helper() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("ALWAYS_PREVIEW_ASR_BIN") {
        let p = std::path::PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.to_path_buf()))
    {
        let p = dir.join(HELPER_NAME);
        if p.is_file() {
            return Some(p);
        }
    }
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("preview-asr/.build/release")
        .join(HELPER_NAME);
    dev.is_file().then_some(dev)
}

/// Is Parakeet (English-only) the right preview engine for `lang`?
/// `auto`/empty resolves to the system locale the Apple recognizer uses.
pub fn lang_is_english(lang: &str) -> bool {
    let lang = lang.trim().to_ascii_lowercase();
    if lang.is_empty() || lang == "auto" {
        return system_locale_is_english();
    }
    lang == "en" || lang.starts_with("en-") || lang.starts_with("en_")
}

fn system_locale_is_english() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        // The daemon is spawned by the GUI without a shell environment, so
        // LANG is unreliable; ask the same preference the Speech framework's
        // `Locale.current` reads.
        let out = Command::new("/usr/bin/defaults")
            .args(["read", "-g", "AppleLocale"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .to_ascii_lowercase()
            });
        match out {
            Some(l) => l.starts_with("en"),
            None => std::env::var("LANG").is_ok_and(|l| l.to_ascii_lowercase().starts_with("en")),
        }
    })
}

static GLOBAL: OnceLock<Option<Arc<PreviewAsr>>> = OnceLock::new();

/// The resident helper, spawned (and warmed) on first call. `None` when
/// the binary is not installed or `ALWAYS_PREVIEW_ASR=0`.
pub fn global() -> Option<Arc<PreviewAsr>> {
    GLOBAL
        .get_or_init(|| {
            if std::env::var("ALWAYS_PREVIEW_ASR").as_deref() == Ok("0") {
                tracing::info!("preview_asr_disabled_by_env");
                return None;
            }
            let Some(bin) = locate_helper() else {
                tracing::info!("preview_asr_helper_not_found");
                return None;
            };
            let asr = PreviewAsr::new(bin);
            asr.ensure_spawned();
            Some(asr)
        })
        .clone()
}

/// Start the helper ahead of the first utterance so its model load and
/// warm-up never land on the user's first word. Call at daemon start.
pub fn prewarm(lang: &str, apple_backend: bool) {
    if apple_backend && lang_is_english(lang) {
        let _ = global();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_encode_little_endian_with_byte_length() {
        let b = encode(&Frame::Audio(7, vec![1, -2]));
        assert_eq!(b, vec![b'A', 7, 0, 0, 0, 4, 0, 0, 0, 1, 0, 0xFE, 0xFF]);
        assert_eq!(
            encode(&Frame::Begin(258)),
            vec![b'B', 2, 1, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(encode(&Frame::End(1))[0], b'E');
    }

    #[test]
    fn parses_partial_and_final_lines_only() {
        assert_eq!(
            parse_line(r#"{"ev":"partial","seq":3,"text":" hello there ","t_ms":5}"#),
            Some((3, PreviewEvent::Partial("hello there".into())))
        );
        assert_eq!(
            parse_line(r#"{"ev":"final","seq":4,"text":"done","finalize_ms":9}"#),
            Some((4, PreviewEvent::Final("done".into())))
        );
        assert_eq!(parse_line(r#"{"ev":"ready","load_ms":1}"#), None);
        assert_eq!(parse_line("not json"), None);
    }

    fn tone(amp: f32, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| (amp * (i as f32 * 0.11).sin()) as i16)
            .collect()
    }

    fn rms(s: &[i16]) -> f32 {
        (s.iter().map(|&v| (v as f32).powi(2)).sum::<f32>() / s.len() as f32).sqrt()
    }

    #[test]
    fn loud_speech_passes_through_unchanged() {
        let mut n = LevelNormalizer::new();
        let x = tone(8000.0, 4800); // ≈ −15 dBFS RMS
        assert_eq!(n.process(&x), x);
        assert_eq!(n.gain(), 1.0);
    }

    #[test]
    fn quiet_speech_is_raised_to_target_within_a_frame() {
        let mut n = LevelNormalizer::new();
        let x = tone(400.0, 4800); // ≈ −41 dBFS RMS: below Parakeet's floor
        let y = n.process(&x);
        let tail = &y[960..];
        let got = rms(tail);
        assert!(
            (got - PREVIEW_TARGET_RMS).abs() / PREVIEW_TARGET_RMS < 0.05,
            "rms {got}"
        );
        // Same waveform, only scaled.
        assert!(
            y.iter()
                .zip(&x)
                .all(|(a, b)| (*a >= 0) == (*b >= 0) || *b == 0)
        );
    }

    #[test]
    fn gain_is_capped_and_room_tone_never_moves_it() {
        let mut n = LevelNormalizer::new();
        assert!(n.process(&vec![0; 4800]).iter().all(|&v| v == 0));
        let room = tone(20.0, 4800); // −64 dBFS room tone
        assert_eq!(n.process(&room), room);
        assert_eq!(n.gain(), 1.0);
        n.process(&tone(150.0, 4800)); // −47 dBFS: voice, very quiet
        assert!((n.gain() - PREVIEW_MAX_GAIN).abs() < 1e-3);
        // A pause after it holds the gain (no drift either way).
        n.process(&room);
        assert!((n.gain() - PREVIEW_MAX_GAIN).abs() < 1e-3);
    }

    #[test]
    fn loud_onset_after_quiet_pause_is_never_clipped() {
        let mut n = LevelNormalizer::new();
        n.process(&tone(150.0, 4800)); // very quiet voice: gain at max
        n.process(&vec![0; 4800]); // pause holds it
        // The loud frame is known before it is scaled: the cut applies to
        // all of it — no wrap, no clipping, unity gain at once.
        let y = n.process(&tone(30000.0, 480));
        assert_eq!(n.gain(), 1.0);
        assert_eq!(y, tone(30000.0, 480));
    }

    /// The 2026-10-14 onset failure: a pre-buffer whose first frames are
    /// the quiet rise of a word must not pump the gain to +24 dB onto the
    /// word's loud frames (which clipped to −2.7 dBFS frames and lost the
    /// first word). The block's loudest frame sizes the gain.
    #[test]
    fn prebuffer_block_is_sized_on_its_loudest_frame() {
        let mut n = LevelNormalizer::new();
        let mut block = tone(60.0, 480 * 4); // rise: ≈ −58 dBFS (below gate)
        block.extend(tone(300.0, 480 * 2)); // ≈ −44 dBFS
        block.extend(tone(6000.0, 480 * 6)); // the word: ≈ −18 dBFS
        let y = n.process(&block);
        assert!(
            y.iter().all(|&v| v != i16::MAX && v != i16::MIN),
            "no clipping"
        );
        // Loud part ≈ unchanged (gain ≈ 1), quiet rise not boosted 24 dB.
        let tail = &y[480 * 7..];
        assert!((rms(tail) / rms(&block[480 * 7..]) - 1.0).abs() < 0.05);
        assert!(rms(&y[480 * 4..480 * 6]) < 3.0 * rms(&block[480 * 4..480 * 6]));
    }

    /// Real helper, real time, quiet speech (TTS at −20 dB ≈ the bypassed
    /// raw-mic level): the boost must bring Parakeet's first partial back.
    /// `ALWAYS_PREVIEW_ASR_BIN=/Applications/Always.app/Contents/MacOS/always-preview-asr
    ///  cargo test --lib live_quiet_speech_preview -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the preview helper + `say`; run explicitly"]
    fn live_quiet_speech_preview() {
        let dir = std::env::temp_dir().join(format!("always-preview-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tts.wav");
        let ok = Command::new("/usr/bin/say")
            .args(["-o", wav.to_str().unwrap(), "--data-format=LEI16@16000"])
            .arg("Okay so I want to check how fast the live preview shows my words on the screen today.")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let bytes = std::fs::read(&wav).unwrap();
        let data = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
        let speech: Vec<i16> = bytes[data..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        let bin = locate_helper().expect("helper binary");
        for (gain, label) in [(0.5f32, "-6 dB"), (0.1, "-20 dB"), (0.03, "-30 dB")] {
            let asr = PreviewAsr::new(bin.clone());
            assert!(asr.ensure_spawned());
            let t = Instant::now();
            while !asr.is_ready() && t.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let first: Arc<Mutex<Option<(Duration, String)>>> = Arc::default();
            let f = Arc::clone(&first);
            let onset = Arc::new(Mutex::new(Instant::now()));
            let o = Arc::clone(&onset);
            let mut utt = asr
                .begin(Arc::new(move |ev| {
                    if let PreviewEvent::Partial(text) = ev
                        && !text.is_empty()
                    {
                        let at = Instant::now().saturating_duration_since(*o.lock());
                        f.lock().get_or_insert((at, text));
                    }
                }))
                .expect("helper ready");
            let scaled: Vec<i16> = speech.iter().map(|&s| (s as f32 * gain) as i16).collect();
            let rms_dbfs = 20.0 * (rms(&scaled) / 32768.0).log10();
            // The daemon opens every utterance with its 200 ms pre-buffer
            // (room tone before the VAD fired); latency counts from speech.
            // Real microphones never deliver digital zeros: room tone at
            // −75 dBFS (the measured bypassed floor) under and before it.
            let lead = 3_200usize;
            let mut rng = 0x2545_f491_u32;
            let fed: Vec<i16> = std::iter::repeat_n(0i16, lead)
                .chain(scaled.iter().copied())
                .map(|s| {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    s.saturating_add(((rng % 21) as i16) - 10)
                })
                .collect();
            let t0 = Instant::now();
            *onset.lock() = t0 + Duration::from_millis(200);
            for (i, frame) in fed.chunks(480).enumerate() {
                utt.feed(frame);
                let due = t0 + Duration::from_millis(30 * (i as u64 + 1));
                if let Some(d) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(d);
                }
            }
            drop(utt);
            std::thread::sleep(Duration::from_millis(300));
            let got = first.lock().clone();
            eprintln!(
                "live_quiet_speech_preview {label} (utterance rms {rms_dbfs:.1} dBFS): first partial {got:?}"
            );
            let (lat, _) = got.unwrap_or_else(|| panic!("no Parakeet partial at {label}"));
            assert!(
                lat < Duration::from_millis(700),
                "{label}: first partial {lat:?}"
            );
            if let Some(mut p) = asr.proc_.lock().take() {
                let _ = p.child.kill();
                let _ = p.child.wait();
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Real helper: a finished span fed at once comes back as one final
    /// (decode time printed: the chunked-tail rescue in `vad.rs` is sized
    /// on it).
    /// `cargo test --lib live_span_recovery -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the preview helper + `say`; run explicitly"]
    fn live_span_recovery() {
        let dir = std::env::temp_dir().join(format!("always-span-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = locate_helper().expect("helper binary");
        let asr = PreviewAsr::new(bin);
        assert!(asr.ensure_spawned());
        let t = Instant::now();
        while !asr.is_ready() && t.elapsed() < Duration::from_secs(300) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(asr.is_ready(), "helper not ready after {:?}", t.elapsed());
        eprintln!("live_span_recovery helper ready after {:?}", t.elapsed());
        for (i, sentence) in [
            "much expanded view, right? So then the music would be playing back.",
            "So the plan for this afternoon is to walk through the whole release checklist with the team and make sure every single item on it has an owner and a date.",
        ]
        .iter()
        .enumerate()
        {
            let wav = dir.join(format!("s{i}.wav"));
            assert!(Command::new("/usr/bin/say")
                .args(["-o", wav.to_str().unwrap(), "--data-format=LEI16@16000"])
                .arg(sentence)
                .status()
                .unwrap()
                .success());
            let bytes = std::fs::read(&wav).unwrap();
            let data = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
            let speech: Vec<i16> =
                bytes[data..].as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            let t0 = Instant::now();
            let text = asr.transcribe_span(&speech, Duration::from_secs(15));
            let took = t0.elapsed();
            eprintln!(
                "live_span_recovery audio={:.2}s took={took:?} text={text:?}",
                speech.len() as f64 / 16_000.0
            );
            let text = text.expect("final");
            assert!(text.to_lowercase().contains(if i == 0 { "music" } else { "checklist" }), "{text}");
        }
        if let Some(mut p) = asr.proc_.lock().take() {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn english_detection_for_explicit_codes() {
        assert!(lang_is_english("en"));
        assert!(lang_is_english("EN-us"));
        assert!(!lang_is_english("fr"));
        assert!(!lang_is_english("es"));
    }
}
