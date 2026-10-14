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
//! as a final — see SPEC.md §5. English only: other languages keep the
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
        let Some(p) = guard.as_ref() else { return false };
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
        })
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

/// One utterance's feed into the helper. Dropping it ends the utterance.
pub struct PreviewUtterance {
    asr: Arc<PreviewAsr>,
    seq: u32,
    started: Instant,
    samples: usize,
}

impl PreviewUtterance {
    pub fn feed(&mut self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        self.samples += samples.len();
        self.asr.send(Frame::Audio(self.seq, samples.to_vec()));
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
        tracing::debug!(
            seq = self.seq,
            audio_secs = self.samples as f64 / 16_000.0,
            wall_ms = self.started.elapsed().as_millis() as u64,
            dropped_frames = self.asr.dropped_frames.load(Ordering::Relaxed),
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
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
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
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_ascii_lowercase());
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
        assert_eq!(encode(&Frame::Begin(258)), vec![b'B', 2, 1, 0, 0, 0, 0, 0, 0]);
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

    #[test]
    fn english_detection_for_explicit_codes() {
        assert!(lang_is_english("en"));
        assert!(lang_is_english("EN-us"));
        assert!(!lang_is_english("fr"));
        assert!(!lang_is_english("es"));
    }
}
