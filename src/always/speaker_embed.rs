//! Speaker-embedding extraction for the "My Voice" verification gate.
//!
//! Wraps the WeSpeaker ResNet34 (VoxCeleb, English) ONNX model: 16 kHz
//! mono i16 PCM in, L2-normalised 256-dim voiceprint out. Two clips of
//! the same speaker score high cosine similarity; different speakers
//! (movie dialogue, other people in the room) score low. The daemon
//! compares each captured utterance against the enrolled voiceprint
//! and discards non-matching speech before it ever reaches STT.
//!
//! The model is 26.5 MB, so unlike Silero it is NOT `include_bytes!`d
//! into the binary — it is downloaded once (on first enrollment) into
//! the same per-user cache dir Silero materialises to, and verified
//! against a pinned sha256.
//!
//! Feature recipe matches WeSpeaker's reference `infer_onnx.py`
//! exactly — enrollment and runtime MUST share it or scores collapse:
//! waveform at int16 scale (raw cast, NO /32768), kaldi fbank with
//! 80 mel bins, 25 ms / 10 ms frames, dither 0, hamming window,
//! snip_edges, preemphasis 0.97; then per-utterance cepstral mean
//! subtraction; tensor `[1, T, 80]` → `embs [1, 256]` → L2 normalise.

use std::path::PathBuf;

use anyhow::{Context, Result};
use kaldi_fbank_rust_kautism::{FbankOptions, OnlineFbank};
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Value;
use parking_lot::Mutex;

pub const MODEL_FILE: &str = "wespeaker_en_voxceleb_resnet34.onnx";
pub const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34.onnx";
pub const MODEL_SHA256: &str = "5ef208a9da1453335308a6b6f4e6dfbd7e183a38b604de0a57664f45d257fe94";
pub const EMBEDDING_DIM: usize = 256;

const SAMPLE_RATE: f32 = 16_000.0;
const NUM_MEL_BINS: i32 = 80;
/// Minimum audio for a usable embedding. Half a second is the floor —
/// scores get noisier below ~1s but remain far more reliable than not
/// checking at all, and the runtime gate fails CLOSED for anything
/// shorter (a snippet too short to verify is dropped, never pasted).
pub const MIN_EMBED_SAMPLES: usize = 8_000; // 0.5 s @ 16 kHz

/// Where the model lives on disk (`~/Library/Caches/always/` on macOS
/// — same dir Silero materialises to).
pub fn model_path() -> Result<PathBuf> {
    let dir = dirs::cache_dir().context("no cache dir")?.join("always");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(MODEL_FILE))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// True if the model is already on disk with the right checksum. Cheap
/// enough (~30 ms for 26 MB) to call at gate/enrollment startup.
pub fn model_present() -> bool {
    let Ok(path) = model_path() else {
        return false;
    };
    match std::fs::read(&path) {
        Ok(bytes) => sha256_hex(&bytes) == MODEL_SHA256,
        Err(_) => false,
    }
}

/// Download the model if missing or corrupt. Blocking (first
/// enrollment only); ~26 MB from GitHub releases. Writes via a temp
/// file + rename so a killed download never leaves a half-written
/// model behind.
pub fn ensure_model() -> Result<PathBuf> {
    let path = model_path()?;
    if model_present() {
        return Ok(path);
    }
    tracing::info!(url = MODEL_URL, "speaker_model_download_started");
    let bytes = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?
        .get(MODEL_URL)
        .send()
        .context("speaker model download failed")?
        .error_for_status()?
        .bytes()?;
    let got = sha256_hex(&bytes);
    if got != MODEL_SHA256 {
        anyhow::bail!("speaker model checksum mismatch: expected {MODEL_SHA256}, got {got}");
    }
    let tmp = path.with_extension("onnx.part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    tracing::info!(bytes = bytes.len(), "speaker_model_download_done");
    Ok(path)
}

/// WeSpeaker ResNet34 speaker-embedding extractor.
///
/// `Session::run` takes `&mut self` in ort 2.0.0-rc.12, so the session
/// sits behind a `Mutex` — same shape as `SileroVad`. Inference cost
/// is well under 100 ms on Apple Silicon for a 15 s utterance, paid
/// once per utterance (not per frame).
pub struct SpeakerEmbedder {
    session: Mutex<Session>,
}

impl SpeakerEmbedder {
    /// Load the model from the cache dir. Errors if it hasn't been
    /// downloaded yet — call `ensure_model()` first on enrollment
    /// paths; the runtime gate treats a missing model as "gate off".
    pub fn new() -> Result<Self> {
        let path = model_path()?;
        // rc.12 builder errors carry non-Send/Sync payloads and won't
        // convert to anyhow via `?` — normalise to plain `ort::Error`
        // first (same workaround vad-rs uses).
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| -> ort::Error { e.into() })?
            .with_intra_threads(1)
            .map_err(|e| -> ort::Error { e.into() })?
            .with_inter_threads(1)
            .map_err(|e| -> ort::Error { e.into() })?
            .commit_from_file(&path)
            .with_context(|| format!("loading speaker model from {}", path.display()))?;
        Ok(Self {
            session: Mutex::new(session),
        })
    }

    /// Compute the L2-normalised 256-dim embedding of an utterance.
    /// `samples`: 16 kHz mono i16 PCM, at least `MIN_EMBED_SAMPLES`.
    pub fn embed(&self, samples: &[i16]) -> Result<Vec<f32>> {
        if samples.len() < MIN_EMBED_SAMPLES {
            anyhow::bail!(
                "utterance too short for speaker embedding: {} samples (< {MIN_EMBED_SAMPLES})",
                samples.len()
            );
        }
        let feats = compute_fbank_cmn(samples)?;
        let num_frames = feats.len() / NUM_MEL_BINS as usize;

        let tensor = ndarray::Array3::from_shape_vec((1, num_frames, NUM_MEL_BINS as usize), feats)
            .context("fbank tensor shape")?;
        let value = Value::from_array(tensor)?;

        let mut session = self.session.lock();
        let outputs = session.run(ort::inputs!["feats" => value])?;
        let (_, raw) = outputs
            .get("embs")
            .context("model output `embs` missing")?
            .try_extract_tensor::<f32>()?;
        if raw.len() != EMBEDDING_DIM {
            anyhow::bail!(
                "unexpected embedding size {} (want {EMBEDDING_DIM})",
                raw.len()
            );
        }
        Ok(l2_normalize(raw))
    }
}

/// Kaldi 80-bin log-mel fbank (WeSpeaker recipe) + per-utterance CMN.
/// Returns a flat `[T * 80]` row-major matrix.
fn compute_fbank_cmn(samples: &[i16]) -> Result<Vec<f32>> {
    let mut opts = FbankOptions::default();
    opts.frame_opts.samp_freq = SAMPLE_RATE;
    opts.frame_opts.dither = 0.0;
    // WeSpeaker trains with torchaudio's hamming window, not kaldi's
    // default povey. The pointer must outlive the C++ object, hence a
    // C-string literal (static lifetime).
    opts.frame_opts.window_type = c"hamming".as_ptr();
    opts.mel_opts.num_bins = NUM_MEL_BINS;
    let mut fbank = OnlineFbank::new(opts);

    // WeSpeaker feeds waveforms at int16 scale — cast, do NOT divide.
    let wave: Vec<f32> = samples.iter().map(|&s| s as f32).collect();
    fbank.accept_waveform(SAMPLE_RATE, &wave);
    fbank.input_finished();

    let num_frames = fbank.num_ready_frames();
    if num_frames <= 0 {
        anyhow::bail!("no fbank frames produced ({} samples)", samples.len());
    }
    let bins = NUM_MEL_BINS as usize;
    let mut feats = Vec::with_capacity(num_frames as usize * bins);
    for i in 0..num_frames {
        let frame = fbank.get_frame(i).context("fbank frame ready-count lied")?;
        feats.extend_from_slice(frame);
    }

    // Per-utterance CMN: subtract each mel bin's mean across frames.
    let n = num_frames as f32;
    let mut means = vec![0f32; bins];
    for frame in feats.chunks_exact(bins) {
        for (m, v) in means.iter_mut().zip(frame) {
            *m += v;
        }
    }
    for m in &mut means {
        *m /= n;
    }
    for frame in feats.chunks_exact_mut(bins) {
        for (v, m) in frame.iter_mut().zip(&means) {
            *v -= m;
        }
    }
    Ok(feats)
}

fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-10);
    v.iter().map(|x| x / norm).collect()
}

/// Cosine similarity of two L2-normalised embeddings (= dot product).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Process-wide embedder, loaded lazily on first use. Load failures
/// are NOT cached — the model may appear later (first enrollment
/// downloads it), so a missing model this utterance can succeed the
/// next. The `model_present()` fast-path keeps the retry cheap when
/// the file simply isn't there.
static GLOBAL: Mutex<Option<std::sync::Arc<SpeakerEmbedder>>> = Mutex::new(None);

pub fn global() -> Option<std::sync::Arc<SpeakerEmbedder>> {
    let mut slot = GLOBAL.lock();
    if slot.is_none() {
        if !model_present() {
            return None;
        }
        match SpeakerEmbedder::new() {
            Ok(e) => *slot = Some(std::sync::Arc::new(e)),
            Err(e) => {
                tracing::warn!(error = %e, "speaker_embedder_load_failed");
                return None;
            }
        }
    }
    slot.clone()
}

/// Combine per-step enrollment embeddings into one voiceprint:
/// element-wise sum, then L2-normalise (sherpa-onnx's
/// `SpeakerEmbeddingManager` convention — equivalent to the mean).
pub fn combine_embeddings(embeddings: &[Vec<f32>]) -> Result<Vec<f32>> {
    let first = embeddings.first().context("no embeddings to combine")?;
    let dim = first.len();
    let mut sum = vec![0f32; dim];
    for e in embeddings {
        if e.len() != dim {
            anyhow::bail!("embedding dim mismatch: {} vs {dim}", e.len());
        }
        for (s, v) in sum.iter_mut().zip(e) {
            *s += v;
        }
    }
    Ok(l2_normalize(&sum))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_voice(seed: u64, secs: f32) -> Vec<i16> {
        // Deterministic pseudo-speech: a fundamental + formant-ish
        // harmonics with slow pitch wobble, distinct per seed. Not real
        // speech, but distinct "speakers" produce distinct spectra.
        let n = (SAMPLE_RATE * secs) as usize;
        let f0 = 90.0 + (seed % 7) as f32 * 25.0;
        let f1 = 400.0 + (seed % 5) as f32 * 130.0;
        let f2 = 1200.0 + (seed % 3) as f32 * 300.0;
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE;
                let wobble = 1.0 + 0.02 * (2.0 * std::f32::consts::PI * 2.3 * t).sin();
                let s = 0.5 * (2.0 * std::f32::consts::PI * f0 * wobble * t).sin()
                    + 0.3 * (2.0 * std::f32::consts::PI * f1 * t).sin()
                    + 0.2 * (2.0 * std::f32::consts::PI * f2 * t).sin();
                (s * 8000.0) as i16
            })
            .collect()
    }

    #[test]
    fn cosine_of_identical_normalized_vectors_is_one() {
        let v = l2_normalize(&[1.0, 2.0, 3.0]);
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn combine_embeddings_is_normalized() {
        let a = l2_normalize(&[1.0, 0.0]);
        let b = l2_normalize(&[0.0, 1.0]);
        let c = combine_embeddings(&[a, b]).unwrap();
        let norm: f32 = c.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn fbank_produces_expected_frame_count() {
        // 1 s @ 16 kHz, snip_edges: 1 + (16000-400)/160 = 98 frames.
        let samples = synth_voice(1, 1.0);
        let feats = compute_fbank_cmn(&samples).unwrap();
        assert_eq!(feats.len(), 98 * 80);
        // CMN: per-bin means should now be ~0.
        let mean0: f32 = feats.chunks_exact(80).map(|f| f[0]).sum::<f32>() / 98.0;
        assert!(mean0.abs() < 1e-3, "CMN should zero bin means, got {mean0}");
    }

    // The tests below need the 26.5 MB model on disk; skip cleanly
    // when absent so CI without the download still passes.
    fn embedder_or_skip() -> Option<SpeakerEmbedder> {
        if !model_present() {
            return None;
        }
        Some(SpeakerEmbedder::new().expect("model loads"))
    }

    #[test]
    fn same_clip_scores_near_one() {
        let Some(e) = embedder_or_skip() else { return };
        let clip = synth_voice(1, 3.0);
        let a = e.embed(&clip).unwrap();
        let b = e.embed(&clip).unwrap();
        assert!(
            cosine(&a, &b) > 0.999,
            "identical audio must embed identically, got {}",
            cosine(&a, &b)
        );
    }

    #[test]
    fn different_synthetic_speakers_score_below_same_speaker() {
        let Some(e) = embedder_or_skip() else { return };
        // Same "speaker", two different clips (different duration).
        let same_a = e.embed(&synth_voice(1, 3.0)).unwrap();
        let same_b = e.embed(&synth_voice(1, 4.0)).unwrap();
        // Different "speaker".
        let other = e.embed(&synth_voice(4, 3.0)).unwrap();
        let same_score = cosine(&same_a, &same_b);
        let cross_score = cosine(&same_a, &other);
        assert!(
            same_score > cross_score,
            "same-source clips ({same_score}) must outscore cross-source ({cross_score})"
        );
    }

    #[test]
    fn too_short_utterance_errors() {
        let Some(e) = embedder_or_skip() else { return };
        let short = synth_voice(1, 0.3);
        assert!(e.embed(&short).is_err());
    }

    /// Real-speech sanity check with macOS TTS voices. Needs `say` +
    /// `sox` + the downloaded model, so it's ignored by default — run
    /// with `cargo test speaker_embed -- --ignored`.
    #[test]
    #[ignore]
    fn real_tts_voices_separate_cleanly() {
        let Some(e) = embedder_or_skip() else { return };
        let tts = |voice: &str, text: &str| -> Vec<i16> {
            let dir = std::env::temp_dir();
            let aiff = dir.join(format!("always-spk-test-{voice}-{}.aiff", text.len()));
            let raw = aiff.with_extension("raw");
            let ok = std::process::Command::new("say")
                .args(["-v", voice, "-o"])
                .arg(&aiff)
                .arg(text)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "say -v {voice} failed");
            let ok = std::process::Command::new("/opt/homebrew/bin/sox")
                .arg(&aiff)
                .args([
                    "-r",
                    "16000",
                    "-c",
                    "1",
                    "-b",
                    "16",
                    "-e",
                    "signed-integer",
                    "-t",
                    "raw",
                ])
                .arg(&raw)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "sox convert failed");
            let bytes = std::fs::read(&raw).unwrap();
            let _ = std::fs::remove_file(&aiff);
            let _ = std::fs::remove_file(&raw);
            bytes
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect()
        };

        let sam_a = e
            .embed(&tts(
                "Samantha",
                "The quick brown fox jumps over the lazy dog near the riverbank.",
            ))
            .unwrap();
        let sam_b = e
            .embed(&tts(
                "Samantha",
                "Yesterday I walked to the market and bought fresh bread and coffee.",
            ))
            .unwrap();
        let dan = e
            .embed(&tts(
                "Daniel",
                "The quick brown fox jumps over the lazy dog near the riverbank.",
            ))
            .unwrap();

        let same = cosine(&sam_a, &sam_b);
        let cross = cosine(&sam_a, &dan);
        assert!(
            same > 0.5,
            "same TTS voice, different sentences should exceed gate threshold, got {same}"
        );
        assert!(
            cross < same - 0.15,
            "different TTS voices should score clearly lower (same={same}, cross={cross})"
        );
    }
}
