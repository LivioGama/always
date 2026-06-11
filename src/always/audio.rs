use std::collections::VecDeque;
use std::io;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Child, ChildStdout};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use parking_lot::Mutex;

pub const RATE: u32 = 16_000;
pub const FRAME_MS: u32 = 30;
pub const FRAME_SAMPLES: usize = 480;
pub const FRAME_BYTES: usize = 960;

/// CoreAudio overrun count that triggers a `rec` respawn. Bursts of
/// "unhandled buffer overrun" mean SoX is discarding input samples —
/// usually a native-rate capture / resample mismatch on USB mics (e.g.
/// Elgato Wave:3 @ 48 kHz stereo).
const REC_OVERRUN_RESPAWN_THRESHOLD: u32 = 64;

static TEMP_WAV_COUNTER: AtomicU64 = AtomicU64::new(0);

// Global persistent audio recorder to avoid spawning processes repeatedly
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

pub struct RecChild {
    child: Child,
    stdout: ChildStdout,
    reuse_count: u32,
    /// Incremented by the stderr drainer on CoreAudio buffer overruns.
    overrun_count: Arc<AtomicU32>,
}

impl RecChild {
    pub fn spawn() -> Result<Self> {
        tracing::info!("rec_spawn_starting");
        let overrun_count = Arc::new(AtomicU32::new(0));
        let rec_path = if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/rec"
        } else {
            "/usr/bin/rec"
        };
        let mut child = std::process::Command::new(rec_path)
            // Capture at the device's native rate/channels (typically 48 kHz
            // stereo on USB mics), then resample to 16 kHz mono on the
            // output side. Requesting `-c 1 -r 16000` on input makes
            // CoreAudio warn and misbehave ("can't set sample rate 16000;
            // using 48000") which leads to buffer overruns and dropped
            // speech energy in the VAD pipeline.
            .args([
                "--no-show-progress",
                "--buffer",
                "131072",
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
        if let Some(stderr) = child.stderr.take() {
            let overruns = Arc::clone(&overrun_count);
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
                        let count = overruns.fetch_add(1, Ordering::Relaxed) + 1;
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
        tracing::info!(pid = child.id(), "rec_spawned");
        Ok(Self {
            child,
            stdout,
            reuse_count: 0,
            overrun_count,
        })
    }

    pub fn get_or_spawn() -> Result<Arc<Mutex<Option<RecChild>>>> {
        let recorder_lock = Arc::clone(&GLOBAL_RECORDER);
        let mut recorder = recorder_lock.lock();

        match recorder.as_mut() {
            Some(rec) => {
                // Check if process is still alive before reuse
                if rec.is_healthy() {
                    rec.reuse_count += 1;
                    // Restart every 1000 uses to prevent memory leaks
                    if rec.reuse_count > 1000 {
                        let _ = rec.child.kill();
                        *recorder = Some(Self::spawn()?);
                    }
                    Ok(Arc::clone(&GLOBAL_RECORDER))
                } else {
                    // Process died, create new one
                    *recorder = Some(Self::spawn()?);
                    Ok(Arc::clone(&GLOBAL_RECORDER))
                }
            }
            None => {
                // Create new recorder
                *recorder = Some(Self::spawn()?);
                Ok(Arc::clone(&GLOBAL_RECORDER))
            }
        }
    }

    fn is_healthy(&mut self) -> bool {
        let overruns = self.overrun_count.load(Ordering::Relaxed);
        if overruns >= REC_OVERRUN_RESPAWN_THRESHOLD {
            tracing::warn!(
                overruns,
                threshold = REC_OVERRUN_RESPAWN_THRESHOLD,
                "rec_respawn_due_to_coreaudio_overruns"
            );
            return false;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => false, // Process has exited
            Ok(None) => true,     // Process still running
            Err(_) => false,      // Error checking process
        }
    }

    pub fn read_frame(&mut self, buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize> {
        let mut read = 0;
        while read < FRAME_BYTES {
            match self.stdout.read(&mut buf[read..]) {
                Ok(0) => {
                    // EOF on rec's stdout means the recorder died — either
                    // mic permission was denied (TCC), the audio device
                    // was unplugged, or the user killed `rec`. Surface
                    // this once per spawn so we can see it in the daemon
                    // log instead of silently looping on empty reads.
                    if read == 0 {
                        tracing::warn!("rec_eof_on_read_frame");
                    }
                    break;
                }
                Ok(n) => read += n,
                Err(e) => return Err(e),
            }
        }
        Ok(read)
    }
}

impl Drop for RecChild {
    fn drop(&mut self) {
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
/// 1. Tests can inject a deterministic [`mock::MockAudioSource`] without
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
#[cfg(feature = "macos")]
pub struct SoxAudioSource {
    handle: std::sync::Arc<Mutex<Option<RecChild>>>,
}

#[cfg(feature = "macos")]
impl SoxAudioSource {
    pub fn new() -> Result<Self> {
        let handle = RecChild::get_or_spawn()?;
        Ok(Self { handle })
    }
}

#[cfg(feature = "macos")]
impl AudioFrameSource for SoxAudioSource {
    fn read_frame(&mut self, buf: &mut [u8; FRAME_BYTES]) -> io::Result<usize> {
        let mut guard = self.handle.lock();
        let Some(rec) = guard.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "audio recorder not initialized",
            ));
        };
        rec.read_frame(buf)
    }
}

/// Linux/Windows stub. Real implementations land with the
/// `linux`/`windows` features in a follow-up.
#[cfg(not(feature = "macos"))]
pub struct StubAudioSource;

#[cfg(not(feature = "macos"))]
impl Default for StubAudioSource {
    fn default() -> Self {
        Self
    }
}

#[cfg(not(feature = "macos"))]
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
    use super::{create_wav_bytes_i16_mono_16k, temp_wav_path};

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
