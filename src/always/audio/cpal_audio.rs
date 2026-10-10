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
        let device_name = device.name().unwrap_or_else(|_| "unknown".to_string());

        let (tx, rx) = mpsc::channel::<[u8; FRAME_BYTES]>();
        let rx = Arc::new(Mutex::new(Some(rx)));
        let ring_clone = Arc::clone(&ring);
        let running_clone = Arc::clone(&running);

        let capture_thread = Self::run_capture(
            device,
            stream_config,
            sample_format,
            tx,
            rx,
            ring_clone,
            running_clone,
        );

        tracing::info!(
            device = %device_name,
            sample_rate = RATE,
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

        // Try exact match: 16kHz in range, mono.
        for cfg in &supported {
            if cfg.channels() == 1
                && cfg.min_sample_rate() <= cpal::SampleRate(RATE)
                && cpal::SampleRate(RATE) <= cfg.max_sample_rate()
            {
                let sc = cfg.with_sample_rate(cpal::SampleRate(RATE));
                return Ok((sc.config(), cfg.sample_format()));
            }
        }

        // Fallback: closest mono config.
        let best = supported
            .iter()
            .filter(|c| c.channels() == 1)
            .min_by_key(|c| {
                let lo = c.min_sample_rate().0;
                let hi = c.max_sample_rate().0;
                if RATE >= lo && RATE <= hi {
                    0u32 // exact range match, prefer exact
                } else if RATE < lo {
                    lo - RATE
                } else {
                    RATE - hi
                }
            })
            .context("No compatible mono input config")?;

        let sc = best
            .try_with_sample_rate(cpal::SampleRate(RATE))
            .or_else(|| best.try_with_sample_rate(best.max_sample_rate()))
            .context("No compatible mono input config")?;

        Ok((sc.config(), best.sample_format()))
    }

    /// Run the capture stream + channel drainer in one thread.
    fn run_capture(
        device: cpal::Device,
        config: cpal::StreamConfig,
        sample_format: cpal::SampleFormat,
        tx: mpsc::Sender<[u8; FRAME_BYTES]>,
        rx: Arc<Mutex<Option<mpsc::Receiver<[u8; FRAME_BYTES]>>>>,
        ring: Arc<Mutex<FrameRing>>,
        running: Arc<AtomicBool>,
    ) -> thread::JoinHandle<()> {
        let tx = Arc::new(Mutex::new(Some(tx)));
        let tx_clone = Arc::clone(&tx);
        let running_clone = Arc::clone(&running);
        let config = Arc::new(config);

        thread::spawn(move || {
            match sample_format {
                cpal::SampleFormat::I16 => {
                    let tx = Arc::clone(&tx_clone);
                    let running = Arc::clone(&running_clone);
                    let cfg = Arc::clone(&config);
                    Self::start_stream_i16(&device, cfg, tx, running);
                }
                cpal::SampleFormat::U16 => {
                    let tx = Arc::clone(&tx_clone);
                    let running = Arc::clone(&running_clone);
                    let cfg = Arc::clone(&config);
                    Self::start_stream_u16(&device, cfg, tx, running);
                }
                cpal::SampleFormat::F32 => {
                    let tx = Arc::clone(&tx_clone);
                    let running = Arc::clone(&running_clone);
                    let cfg = Arc::clone(&config);
                    Self::start_stream_f32(&device, cfg, tx, running);
                }
                _ => {
                    tracing::error!(format = ?sample_format, "unsupported sample format");
                }
            }
            Self::drain_loop(rx, ring);
        })
    }

    fn start_stream_i16(
        device: &cpal::Device,
        config: Arc<cpal::StreamConfig>,
        tx: Arc<Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>>,
        running: Arc<AtomicBool>,
    ) {
        // We need TWO Arc clones: one for the data callback, one for the
        // while loop. The data callback borrows `running` for its closure,
        // so we can't reuse it in the while loop.
        let running_cb = Arc::clone(&running);
        let tx_cb = Arc::clone(&tx);
        let cfg_cb = Arc::clone(&config);

        let stream = device.build_input_stream(
            &cfg_cb,
            move |data: &[i16], _info| {
                if running_cb.load(Ordering::Relaxed) {
                    Self::push_frames_i16(data, &tx_cb);
                }
            },
            move |err| {
                tracing::error!(error = %err, "cpal_stream_error");
            },
            None,
        );
        if let Ok(stream) = stream
            && stream.play().is_ok()
        {
            while running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    fn start_stream_u16(
        device: &cpal::Device,
        config: Arc<cpal::StreamConfig>,
        tx: Arc<Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>>,
        running: Arc<AtomicBool>,
    ) {
        let running_cb = Arc::clone(&running);
        let tx_cb = Arc::clone(&tx);
        let cfg_cb = Arc::clone(&config);

        let stream = device.build_input_stream(
            &cfg_cb,
            move |data: &[u16], _info| {
                if running_cb.load(Ordering::Relaxed) {
                    Self::push_frames_u16(data, &tx_cb);
                }
            },
            move |err| {
                tracing::error!(error = %err, "cpal_stream_error");
            },
            None,
        );
        if let Ok(stream) = stream
            && stream.play().is_ok()
        {
            while running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    fn start_stream_f32(
        device: &cpal::Device,
        config: Arc<cpal::StreamConfig>,
        tx: Arc<Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>>,
        running: Arc<AtomicBool>,
    ) {
        let running_cb = Arc::clone(&running);
        let tx_cb = Arc::clone(&tx);
        let cfg_cb = Arc::clone(&config);

        let stream = device.build_input_stream(
            &cfg_cb,
            move |data: &[f32], _info| {
                if running_cb.load(Ordering::Relaxed) {
                    Self::push_frames_f32(data, &tx_cb);
                }
            },
            move |err| {
                tracing::error!(error = %err, "cpal_stream_error");
            },
            None,
        );
        if let Ok(stream) = stream
            && stream.play().is_ok()
        {
            while running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
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
                        Err(_) => continue,
                    }
                } else {
                    break;
                }
            };
            ring.lock().push(frame);
        }
    }

    fn push_frames_i16(data: &[i16], tx: &Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>) {
        let frames = data.len() / FRAME_SAMPLES;
        for i in 0..frames {
            let start = i * FRAME_SAMPLES;
            let mut frame = [0u8; FRAME_BYTES];
            for j in 0..FRAME_SAMPLES {
                let val = data[start + j];
                frame[j * 2..j * 2 + 2].copy_from_slice(&val.to_le_bytes());
            }
            if let Some(sender) = &*tx.lock() {
                let _ = sender.send(frame);
            }
        }
    }

    fn push_frames_u16(data: &[u16], tx: &Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>) {
        let frames = data.len() / FRAME_SAMPLES;
        for i in 0..frames {
            let start = i * FRAME_SAMPLES;
            let mut frame = [0u8; FRAME_BYTES];
            for j in 0..FRAME_SAMPLES {
                let idx = start + j;
                if idx < data.len() {
                    let signed = ((data[idx] as i32) - 32768) as i16;
                    frame[j * 2..j * 2 + 2].copy_from_slice(&signed.to_le_bytes());
                }
            }
            if let Some(sender) = &*tx.lock() {
                let _ = sender.send(frame);
            }
        }
    }

    fn push_frames_f32(data: &[f32], tx: &Mutex<Option<mpsc::Sender<[u8; FRAME_BYTES]>>>) {
        let frames = data.len() / FRAME_SAMPLES;
        for i in 0..frames {
            let start = i * FRAME_SAMPLES;
            let mut frame = [0u8; FRAME_BYTES];
            for j in 0..FRAME_SAMPLES {
                let idx = start + j;
                if idx < data.len() {
                    let sample = (data[idx] * 32767.0) as i16;
                    frame[j * 2..j * 2 + 2].copy_from_slice(&sample.to_le_bytes());
                }
            }
            if let Some(sender) = &*tx.lock() {
                let _ = sender.send(frame);
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
        let frame = self.ring.lock().pop(Duration::from_millis(15_000))?;
        match frame {
            Some(f) => {
                buf.copy_from_slice(&f);
                Ok(FRAME_BYTES)
            }
            None => Ok(0),
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

        let mut ring = rec.ring.lock();
        let frame = ring.pop(Duration::from_millis(15_000))?;
        match frame {
            Some(f) => {
                buf.copy_from_slice(&f);
                Ok(FRAME_BYTES)
            }
            None => Ok(0),
        }
    }
}

impl Default for CpalsAudioSource {
    fn default() -> Self {
        Self
    }
}
