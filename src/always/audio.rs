use std::io;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use std::collections::VecDeque;
use once_cell::sync::Lazy;

use anyhow::{Context, Result};

pub const RATE: u32 = 16_000;
pub const FRAME_MS: u32 = 30;
pub const FRAME_SAMPLES: usize = 480;
pub const FRAME_BYTES: usize = 960;

static TEMP_WAV_COUNTER: AtomicU64 = AtomicU64::new(0);

// Global persistent audio recorder to avoid spawning processes repeatedly
static GLOBAL_RECORDER: Lazy<Arc<Mutex<Option<RecChild>>>> =
    Lazy::new(|| Arc::new(Mutex::new(None)));

// Memory pool for audio buffers to reduce allocations
static AUDIO_BUFFER_POOL: Lazy<Arc<Mutex<VecDeque<Vec<i16>>>>> =
    Lazy::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(10))));

pub struct AudioBuffer {
    buffer: Vec<i16>,
    pool: Arc<Mutex<VecDeque<Vec<i16>>>>,
}

impl AudioBuffer {
    pub fn get() -> Self {
        let pool = Arc::clone(&AUDIO_BUFFER_POOL);
        let buffer = {
            let mut pool_lock = pool.lock().unwrap();
            pool_lock.pop_front().unwrap_or_else(|| Vec::with_capacity(16000)) // 1 second at 16kHz
        };
        Self { buffer, pool }
    }

    pub fn as_mut(&mut self) -> &mut Vec<i16> {
        &mut self.buffer
    }

    pub fn as_slice(&self) -> &[i16] {
        &self.buffer
    }
}

impl Drop for AudioBuffer {
    fn drop(&mut self) {
        // Return buffer to pool
        self.buffer.clear();
        let mut pool_lock = self.pool.lock().unwrap();
        if pool_lock.len() < 10 { // Limit pool size
            pool_lock.push_back(std::mem::take(&mut self.buffer));
        }
    }
}

pub struct RecChild {
    child: Child,
    stdout: ChildStdout,
    reuse_count: u32,
}

impl RecChild {
    pub fn spawn() -> Result<Self> {
        let mut child = std::process::Command::new("/opt/homebrew/bin/rec")
            .args([
                "--no-show-progress",
                "-c",
                "1",
                "-r",
                "16000",
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
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("Failed to run 'rec' command. Install SoX: brew install sox")?;
        let stdout = child.stdout.take().context("sox stdout missing")?;
        Ok(Self { child, stdout, reuse_count: 0 })
    }

    pub fn get_or_spawn() -> Result<Arc<Mutex<Option<RecChild>>>> {
        let recorder_lock = Arc::clone(&GLOBAL_RECORDER);
        let mut recorder = recorder_lock.lock().unwrap();

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
                Ok(0) => break,
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
    wav_data.extend_from_slice(&1u16.to_le_bytes());  // PCM format
    wav_data.extend_from_slice(&1u16.to_le_bytes());  // Mono
    wav_data.extend_from_slice(&RATE.to_le_bytes());  // Sample rate
    wav_data.extend_from_slice(&(RATE * 2).to_le_bytes()); // Byte rate
    wav_data.extend_from_slice(&2u16.to_le_bytes());  // Block align
    wav_data.extend_from_slice(&16u16.to_le_bytes()); // Bits per sample
    wav_data.extend_from_slice(b"data");
    wav_data.extend_from_slice(&(data_size as u32).to_le_bytes());

    // Write audio data directly
    for sample in samples {
        wav_data.extend_from_slice(&sample.to_le_bytes());
    }

    Ok(wav_data)
}

pub fn write_wav_i16_mono_16k(path: &Path, samples: &[i16]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create utterance cache directory")?;
    }

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).context("failed to create WAV")?;
    for sample in samples {
        writer
            .write_sample(*sample)
            .context("failed to write sample")?;
    }
    writer.finalize().context("failed to finalize WAV")?;
    Ok(())
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
    use super::{temp_wav_path, create_wav_bytes_i16_mono_16k};

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
}