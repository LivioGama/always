/// Cross-platform audio capture via cpal — replaces SoX `rec` on ALL platforms.
///
/// Uses the host API (CoreAudio on macOS, ALSA on Linux, WASAPI on Windows)
/// in **shared mode** by default. This means the microphone is never acquired
/// exclusively — other apps can still use it, and the system volume never
/// drops when Always starts listening.
///
/// Frame format matches the existing constants:
/// - 16 000 Hz sample rate
/// - Mono (1 channel)
/// - 16-bit little-endian PCM
/// - 30 ms frames (480 samples, 960 bytes)
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use parking_lot::Mutex;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::{AudioFrameSource, FRAME_BYTES, FRAME_SAMPLES, RATE};

// ─── Ring buffer ───────────────────────────────────────────────────

struct FrameRing {
    frames: std::collections::VecDeque<[u8; FRAME_BYTES]>,
    closed: bool,
    /// Timestamp of the most recently pushed frame.
    last_captured_at: Option<std::time::Instant>,
}

impl FrameRing {
    fn new() -> Self {
        Self {
            frames: std::collections::VecDeque::with_capacity(1_000),
            closed: false,
            last_captured_at: None,
        }
    }

    fn push(&mut self, frame: [u8; FRAME_BYTES]) {
        if self.frames.len() >= 1000 {
            self.frames.pop_front();
            tracing::info!(capacity = 1000, "audio_ring_full_dropping_oldest");
        }
        self.frames.push_back(frame);
        self.last_captured_at = Some(std::time::Instant::now());
    }

    fn close(&mut self) {
        self.closed = true;
    }

    fn pop(&mut self, timeout: Duration) -> io::Result<Option<[u8; FRAME_BYTES]>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(frame) = self.frames.pop_front() {
                return Ok(Some(frame));
            }
            if self.closed {
                return Ok(None);
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no audio from cpal within timeout",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

// ─── Shared audio source ───────────────────────────────────────────

struct CaptureInput {
    device: cpal::Device,
    config: cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    sample_rate: u32,
    channels: usize,
}

pub struct SharedAudioSource {
    ring: Arc<Mutex<FrameRing>>,
    running: Arc<AtomicBool>,
    stream_drop: Option<StreamDrop>,
}

#[allow(dead_code)]
struct StreamDrop(thread::JoinHandle<()>);

/// Helper: join the stream thread when dropped.
impl Drop for StreamDrop {
    fn drop(&mut self) {
        // Detach — we just need the thread to stop holding Arc references.
    }
}

impl SharedAudioSource {
    pub fn new() -> Result<Self> {
        let ring = Arc::new(Mutex::new(FrameRing::new()));
        let running = Arc::new(AtomicBool::new(true));

        let device = Self::find_input_device()?;
        let (stream_config, sample_format) = Self::find_config(&device)?;
        let input_rate = stream_config.sample_rate;
        let input_channels = stream_config.channels as usize;
        // cpal 0.18: Device no longer has .name(); use Display.
        let device_name = format!("{}", device);

        let (tx, rx) = mpsc::channel::<[u8; FRAME_BYTES]>();
        let rx = Arc::new(Mutex::new(Some(rx)));
        let ring_clone = Arc::clone(&ring);
        let running_clone = Arc::clone(&running);

        let capture_thread = Self::run_capture(
            CaptureInput {
                device,
                config: stream_config,
                sample_format,
                sample_rate: input_rate,
                channels: input_channels,
            },
            tx,
            rx,
            ring_clone,
            running_clone,
        );

        tracing::info!(
            device = %device_name,
            input_sample_rate = input_rate,
            input_channels,
            "cpal_stream_started"
        );

        Ok(Self {
            ring,
            running,
            stream_drop: Some(StreamDrop(capture_thread)),
        })
    }

    fn find_input_device() -> Result<cpal::Device> {
        let host = cpal::default_host();
        if let Some(d) = host.default_input_device() {
            return Ok(d);
        }
        for d in host.devices().map_err(|e| anyhow::anyhow!("{}", e))? {
            if d.supported_input_configs()
                .map(|mut it| it.next().is_some())
                .unwrap_or(false)
            {
                return Ok(d);
            }
        }
        anyhow::bail!("No input device found")
    }

    fn find_config(device: &cpal::Device) -> Result<(cpal::StreamConfig, cpal::SampleFormat)> {
        let supported: Vec<_> = device
            .supported_input_configs()
            .map_err(|e| anyhow::anyhow!("Failed to list configs: {}", e))?
            .collect();
        pick_config(&supported)
            .context("No input config Always can read (needs an i16, u16 or f32 sample format)")
    }

    /// Run the capture stream + channel drainer in one thread.
    fn run_capture(
        input: CaptureInput,
        tx: mpsc::Sender<[u8; FRAME_BYTES]>,
        rx: Arc<Mutex<Option<mpsc::Receiver<[u8; FRAME_BYTES]>>>>,
        ring: Arc<Mutex<FrameRing>>,
        running: Arc<AtomicBool>,
    ) -> thread::JoinHandle<()> {
        let CaptureInput {
            device,
            config,
            sample_format,
            sample_rate: input_rate,
            channels: input_channels,
        } = input;
        let tx = Arc::new(Mutex::new(Some(tx)));
        let tx_clone = Arc::clone(&tx);
        let running_clone = Arc::clone(&running);
        let config = Arc::new(config);

        thread::spawn(move || {
            // Drain concurrently: the stream functions below block for as
            // long as capture runs, so draining afterwards (as before
            // 2026-10-20) meant no frame reached the ring while recording.
            // Returning drops the last sender, which ends the drain.
            thread::spawn(move || Self::drain_loop(rx, ring));
            match sample_format {
                cpal::SampleFormat::I16 => {
                    let tx = Arc::clone(&tx_clone);
                    let running = Arc::clone(&running_clone);
                    let cfg = Arc::clone(&config);
                    Self::start_stream_i16(&device, cfg, tx, running, input_rate, input_channels);
                }
                cpal::SampleFormat::U16 => {
                    let tx = Arc::clone(&tx_clone);
                    let running = Arc::clone(&running_clone);
                    let cfg = Arc::clone(&config);
                    Self::start_stream_u16(&device, cfg, tx, running, input_rate, input_channels);
                }
                cpal::SampleFormat::F32 => {
                    let tx = Arc::clone(&tx_clone);
                    let running = Arc::clone(&running_clone);
                    let cfg = Arc::clone(&config);
                    Self::start_stream_f32(&device, cfg, tx, running, input_rate, input_channels);
                }
                _ => {
                    tracing::error!(format = ?sample_format, "unsupported sample format");
                }
            }
        })
    }

    fn start_stream_i16(
        device: &cpal::Device,
        config: Arc<cpal::StreamConfig>,
        tx: Arc<Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>>,
        running: Arc<AtomicBool>,
        input_rate: u32,
        input_channels: usize,
    ) {
        // We need TWO Arc clones: one for the data callback, one for the
        // while loop. The data callback borrows `running` for its closure,
        // so we can't reuse it in the while loop.
        let running_cb = Arc::clone(&running);
        let tx_cb = Arc::clone(&tx);
        let cfg_cb = Arc::clone(&config);
        let mut framer = PcmFramer::new(input_rate, input_channels);

        let stream = device.build_input_stream(
            *cfg_cb,
            move |data: &[i16], _info| {
                if running_cb.load(Ordering::Relaxed) {
                    framer.push_i16(data, &tx_cb);
                }
            },
            move |err| {
                tracing::error!(error = %err, "cpal_stream_error");
            },
            None,
        );
        match stream {
            Ok(stream) => match stream.play() {
                Ok(()) => {
                    while running.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(100));
                    }
                }
                Err(error) => tracing::error!(%error, "cpal_stream_play_failed"),
            },
            Err(error) => tracing::error!(%error, "cpal_stream_build_failed"),
        }
    }

    fn start_stream_u16(
        device: &cpal::Device,
        config: Arc<cpal::StreamConfig>,
        tx: Arc<Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>>,
        running: Arc<AtomicBool>,
        input_rate: u32,
        input_channels: usize,
    ) {
        let running_cb = Arc::clone(&running);
        let tx_cb = Arc::clone(&tx);
        let cfg_cb = Arc::clone(&config);
        let mut framer = PcmFramer::new(input_rate, input_channels);
        let stream = device.build_input_stream(
            *cfg_cb,
            move |data: &[u16], _info| {
                if running_cb.load(Ordering::Relaxed) {
                    framer.push_u16(data, &tx_cb);
                }
            },
            move |err| {
                tracing::error!(error = %err, "cpal_stream_error");
            },
            None,
        );
        match stream {
            Ok(stream) => match stream.play() {
                Ok(()) => {
                    while running.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(100));
                    }
                }
                Err(error) => tracing::error!(%error, "cpal_stream_play_failed"),
            },
            Err(error) => tracing::error!(%error, "cpal_stream_build_failed"),
        }
    }

    fn start_stream_f32(
        device: &cpal::Device,
        config: Arc<cpal::StreamConfig>,
        tx: Arc<Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>>,
        running: Arc<AtomicBool>,
        input_rate: u32,
        input_channels: usize,
    ) {
        let running_cb = Arc::clone(&running);
        let tx_cb = Arc::clone(&tx);
        let cfg_cb = Arc::clone(&config);
        let mut framer = PcmFramer::new(input_rate, input_channels);
        let stream = device.build_input_stream(
            *cfg_cb,
            move |data: &[f32], _info| {
                if running_cb.load(Ordering::Relaxed) {
                    framer.push_f32(data, &tx_cb);
                }
            },
            move |err| {
                tracing::error!(error = %err, "cpal_stream_error");
            },
            None,
        );
        match stream {
            Ok(stream) => match stream.play() {
                Ok(()) => {
                    while running.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(100));
                    }
                }
                Err(error) => tracing::error!(%error, "cpal_stream_play_failed"),
            },
            Err(error) => tracing::error!(%error, "cpal_stream_build_failed"),
        }
    }

    fn drain_loop(
        rx: Arc<Mutex<Option<mpsc::Receiver<[u8; FRAME_BYTES]>>>>,
        ring: Arc<Mutex<FrameRing>>,
    ) {
        loop {
            let frame = {
                let rx_lock = rx.lock();
                if let Some(receiver) = &*rx_lock {
                    match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(f) => f,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        // The stream ended: without this the loop spun at full
                        // CPU forever, one thread per recorder respawn.
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    break;
                }
            };
            ring.lock().push(frame);
        }
    }
}

/// Converts an input device's interleaved samples into the daemon's
/// 16 kHz mono frames. State spans CPAL callbacks so neither arbitrary
/// callback boundaries nor stereo devices drop samples.
struct PcmFramer {
    input_rate: u32,
    input_channels: usize,
    rate_phase: u64,
    frame: [u8; FRAME_BYTES],
    samples_in_frame: usize,
}

impl PcmFramer {
    fn new(input_rate: u32, input_channels: usize) -> Self {
        Self {
            input_rate: input_rate.max(1),
            input_channels: input_channels.max(1),
            rate_phase: 0,
            frame: [0; FRAME_BYTES],
            samples_in_frame: 0,
        }
    }

    fn push_i16(&mut self, data: &[i16], tx: &Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>) {
        self.push_interleaved(
            data.chunks_exact(self.input_channels).map(|channels| {
                let sum: i64 = channels.iter().map(|sample| i64::from(*sample)).sum();
                (sum / channels.len() as i64) as i16
            }),
            tx,
        );
    }

    fn push_u16(&mut self, data: &[u16], tx: &Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>) {
        self.push_interleaved(
            data.chunks_exact(self.input_channels).map(|channels| {
                let sum: i64 = channels
                    .iter()
                    .map(|sample| i64::from(*sample) - 32768)
                    .sum();
                (sum / channels.len() as i64) as i16
            }),
            tx,
        );
    }

    fn push_f32(&mut self, data: &[f32], tx: &Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>) {
        self.push_interleaved(
            data.chunks_exact(self.input_channels).map(|channels| {
                let sum: f64 = channels
                    .iter()
                    .map(|sample| sample.clamp(-1.0, 1.0) as f64)
                    .sum();
                ((sum / channels.len() as f64) * 32767.0).round() as i16
            }),
            tx,
        );
    }

    fn push_interleaved(
        &mut self,
        samples: impl Iterator<Item = i16>,
        tx: &Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>,
    ) {
        for sample in samples {
            self.rate_phase += u64::from(RATE);
            while self.rate_phase >= u64::from(self.input_rate) {
                self.rate_phase -= u64::from(self.input_rate);
                let offset = self.samples_in_frame * 2;
                self.frame[offset..offset + 2].copy_from_slice(&sample.to_le_bytes());
                self.samples_in_frame += 1;
                if self.samples_in_frame == FRAME_SAMPLES {
                    if let Some(sender) = &*tx.lock() {
                        let _ = sender.send(self.frame);
                    }
                    self.frame = [0; FRAME_BYTES];
                    self.samples_in_frame = 0;
                }
            }
        }
    }
}

impl SharedAudioSource {
    pub fn drain_pending(&mut self) -> f64 {
        let mut ring = self.ring.lock();
        let dropped = ring.frames.len() * FRAME_BYTES;
        ring.frames.clear();
        dropped as f64 / 32_000.0
    }

    /// Peek at the most recently captured frame without consuming it.
    /// Returns `None` when the ring is empty. Non-blocking.
    pub fn peek_newest_frame(&self) -> Option<[u8; FRAME_BYTES]> {
        let ring = self.ring.lock();
        ring.frames.back().copied()
    }

    /// Read the next frame (oldest first), blocking up to 15 s.
    /// Returns `Ok(FRAME_BYTES)` on success, `Ok(0)` on EOF (closed ring),
    /// or `Err(TimedOut)` when no frame arrives within the timeout.
    ///
    /// Also updates `last_captured_at` to match the moment the ring
    /// last had a frame pushed (approximating wall-clock capture time).
    pub fn read_frame(&mut self, buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize> {
        // Wait WITHOUT holding the ring: the drain thread needs its lock to
        // push. Waiting inside `pop` under the lock starved it until the
        // 15 s timeout, so every read timed out and the recorder reset.
        let deadline = std::time::Instant::now() + Duration::from_millis(15_000);
        loop {
            match self.ring.lock().pop(Duration::ZERO) {
                Ok(Some(f)) => {
                    buf.copy_from_slice(&f);
                    return Ok(FRAME_BYTES);
                }
                Ok(None) => return Ok(0),
                Err(e) if std::time::Instant::now() >= deadline => return Err(e),
                Err(_) => {}
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Capture stamp of the frame last returned by a read.
    pub fn last_captured_at(&self) -> Option<std::time::Instant> {
        self.ring.lock().last_captured_at
    }

    /// Force-respawn the recorder (triggered by UDS `RespawnRecorder`).
    pub fn respawn(&mut self) -> Result<()> {
        self.running.store(false, Ordering::SeqCst);
        let mut ring = self.ring.lock();
        ring.close();
        drop(ring);

        // Detach the old stream thread (join is not possible from a
        // Drop-typed struct).
        self.stream_drop = None;

        let mut new = SharedAudioSource::new()?;
        // Use clone() — Arc fields don't implement Copy, but they're
        // Cloneable, so incrementing the refcount is the right thing.
        self.ring = new.ring.clone();
        self.running = new.running.clone();
        self.stream_drop = new.stream_drop.take();
        Ok(())
    }
}

impl Drop for SharedAudioSource {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let mut ring = self.ring.lock();
        ring.close();
        self.stream_drop = None;
    }
}

// ─── Global singleton ──────────────────────────────────────────────

static GLOBAL_SOURCE: std::sync::LazyLock<Arc<Mutex<Option<SharedAudioSource>>>> =
    std::sync::LazyLock::new(|| Arc::new(Mutex::new(None)));

/// Get or create the global shared audio source.
pub fn get_or_spawn() -> Result<Arc<Mutex<Option<SharedAudioSource>>>> {
    let mut source = GLOBAL_SOURCE.lock();
    if source.is_none() {
        *source = Some(SharedAudioSource::new()?);
    }
    Ok(Arc::clone(&GLOBAL_SOURCE))
}

/// Force-respawn (triggered by UDS `RespawnRecorder`).
pub fn force_respawn() -> Result<()> {
    let mut source = GLOBAL_SOURCE.lock();
    if let Some(ref mut s) = source.as_mut() {
        s.respawn()?;
    }
    Ok(())
}

// ─── AudioFrameSource trait impl ───────────────────────────────────

pub struct CpalsAudioSource;

impl AudioFrameSource for CpalsAudioSource {
    fn read_frame(&mut self, buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize> {
        let source = get_or_spawn().map_err(|e| io::Error::other(e.to_string()))?;
        let mut guard = source.lock();
        let rec = guard.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "audio recorder not initialized",
            )
        })?;
        // Same wait as the shared source: never block the drain thread.
        rec.read_frame(buf)
    }
}

/// Choose the capture format. Only formats the capture callbacks can read
/// are considered: PipeWire's ALSA plugin lists U8 first, and taking it left
/// the daemon with a stream it could not decode — no audio, an error every
/// 100 ms (2026-10-20).
fn pick_config(
    supported: &[cpal::SupportedStreamConfigRange],
) -> Option<(cpal::StreamConfig, cpal::SampleFormat)> {
    let supported: Vec<_> = supported
        .iter()
        .filter(|c| {
            matches!(
                c.sample_format(),
                cpal::SampleFormat::I16 | cpal::SampleFormat::U16 | cpal::SampleFormat::F32
            )
        })
        .collect();

    // Prefer exact 16 kHz mono, then exact 16 kHz on any channel
    // layout. The capture callback downmixes/resamples as needed.
    for cfg in &supported {
        if cfg.channels() == 1 && cfg.min_sample_rate() <= RATE && RATE <= cfg.max_sample_rate() {
            let sc = cfg.with_sample_rate(RATE);
            return Some((sc.config(), cfg.sample_format()));
        }
    }
    for cfg in &supported {
        if cfg.min_sample_rate() <= RATE && RATE <= cfg.max_sample_rate() {
            let sc = cfg.with_sample_rate(RATE);
            return Some((sc.config(), cfg.sample_format()));
        }
    }

    // Fallback: closest supported format, preferring mono on ties.
    let best = supported
        .iter()
        .min_by_key(|c| {
            let lo = c.min_sample_rate();
            let hi = c.max_sample_rate();
            let distance = if RATE >= lo && RATE <= hi {
                0u32
            } else if RATE < lo {
                lo - RATE
            } else {
                RATE - hi
            };
            distance.saturating_mul(2) + u32::from(c.channels() != 1)
        })
        .copied()?;

    let selected_rate = RATE.clamp(best.min_sample_rate(), best.max_sample_rate());
    let sc = best.with_sample_rate(selected_rate);

    Some((sc.config(), best.sample_format()))
}

impl Default for CpalsAudioSource {
    fn default() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::{SampleFormat as F, SupportedBufferSize::Unknown, SupportedStreamConfigRange as R};

    #[test]
    fn frames_reach_a_waiting_reader_and_the_drain_ends_with_the_stream() {
        let (tx, rx) = mpsc::channel::<[u8; FRAME_BYTES]>();
        let ring = Arc::new(Mutex::new(FrameRing::new()));
        let drain = {
            let ring = Arc::clone(&ring);
            thread::spawn(move || {
                SharedAudioSource::drain_loop(Arc::new(Mutex::new(Some(rx))), ring)
            })
        };
        let mut source = SharedAudioSource {
            ring,
            running: Arc::new(AtomicBool::new(true)),
            stream_drop: None,
        };
        // The live order: the reader is already waiting when the frame lands.
        let producer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            tx.send([7; FRAME_BYTES]).unwrap();
            tx
        });
        let mut buf = [0u8; FRAME_BYTES];
        let started = std::time::Instant::now();
        assert_eq!(source.read_frame(&mut buf).unwrap(), FRAME_BYTES);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "reader starved the drain"
        );
        assert_eq!(buf[0], 7);
        // The last sender goes away with the stream: the drain must stop.
        drop(producer.join().unwrap());
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            drain.join().ok();
            done_tx.send(()).ok();
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "drain loop kept running after the stream ended"
        );
    }

    #[test]
    fn pick_config_skips_formats_capture_cannot_read() {
        // PipeWire's ALSA plugin, abridged: U8 is listed first.
        let pipewire = [
            R::new(1, 1, 384_000, Unknown, F::U8),
            R::new(1, 1, 384_000, Unknown, F::I16),
            R::new(1, 1, 384_000, Unknown, F::F32),
        ];
        let (cfg, format) = pick_config(&pipewire).unwrap();
        assert_eq!((format, cfg.channels, cfg.sample_rate), (F::I16, 1, RATE));
        assert!(pick_config(&[R::new(1, 1, 384_000, Unknown, F::U8)]).is_none());
        // Stereo-only F32 at 48 kHz: still chosen, downmixed/resampled later.
        let (cfg, format) = pick_config(&[R::new(2, 48_000, 48_000, Unknown, F::F32)]).unwrap();
        assert_eq!((format, cfg.channels, cfg.sample_rate), (F::F32, 2, 48_000));
    }
}
