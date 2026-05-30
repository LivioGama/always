# Audit: stt-local (`src/always/stt_local.rs`)

Scope file read in full (282 lines). Callers traced: `src/stt_dispatch.rs` (build_local_transcriber, lines 136-221) handles the *feature-off* (`#[cfg(not(feature = "local-stt"))]` → falls back to Groq) and *model-missing* (`path.exists()` bail) cases **outside** this file, so those two specific lenses are handled gracefully at the dispatch layer and are NOT defects in the assigned file. `EngineType` has exactly the 8 variants matched in `build_engine`, so that `match` is exhaustive (no fallthrough panic). The `Transcriber` trait is `Send + Sync` and the engine is held behind a `Mutex`, matching the trait's `&self`.

The findings below are concrete defects within the assigned file.

### [HIGH] Unbounded memory: WAV decode + engine never cap audio length, OOM on a stuck-open utterance
- file: src/always/stt_local.rs:245-250 (and the whole `transcribe_from_bytes` path, 119-142)
- category: resource-leak
- confidence: medium
- impact: A pathologically long utterance (VAD never closing, e.g. continuous noise/feedback while the daemon runs unattended) produces a multi-hundred-MB WAV. `decode_wav_to_f32` allocates a second full `Vec<f32>` (4 bytes/sample = 2x the i16 data), and the ONNX/Whisper engines then materialize internal feature/encoder tensors over the *entire* clip with no chunking. On a background always-on daemon this is a realistic path to multi-GB spikes / OOM kill.
- problem: There is no length guard anywhere in this file. `decode_wav_to_f32` does `let mut samples = Vec::with_capacity(data.len() / 2);` and pushes every sample; `run_engine` hands the full `&[f32]` straight to each engine (`w.transcribe_with(samples, ...)`, etc.). The module doc even brags "No temp files — we decode the bytes to f32 in memory," confirming the whole clip is resident. Handy/whisper-style engines have quadratic-ish attention memory on very long inputs.
- fix: Cap the input duration before decoding/inference. E.g. at the top of `transcribe_from_bytes`:
```rust
const MAX_SECS: usize = 600; // 10 min hard ceiling for an offline utterance
const MAX_WAV_BYTES: usize = 16_000 * 2 * MAX_SECS; // 16 kHz mono i16
if audio.len() > MAX_WAV_BYTES + 1024 {
    return Err(SttError::Other(anyhow::anyhow!(
        "utterance too long for local STT ({} bytes > {} cap)", audio.len(), MAX_WAV_BYTES
    )));
}
```
and/or truncate `samples` to `MAX_SECS * 16_000` after decode. This bounds the memory the always-on daemon can be driven to allocate.

### [MEDIUM] WAV decoder silently drops a trailing odd byte instead of rejecting malformed data
- file: src/always/stt_local.rs:246-249
- category: correctness
- confidence: high
- impact: A `data` chunk with an odd byte length (corruption, or a non-canonical producer) is decoded as if the last byte didn't exist, silently transcribing slightly-shifted/garbage audio rather than failing loudly — directly contradicting the function's own doc promise ("a bad input fails loudly rather than silently transcribing garbage").
- problem:
```rust
let mut samples = Vec::with_capacity(data.len() / 2);
for pair in data.chunks_exact(2) {
    let s = i16::from_le_bytes([pair[0], pair[1]]);
    samples.push(s as f32 / 32768.0);
}
```
`chunks_exact(2)` discards any odd trailing byte with no error. The doc comment at lines 204-209 explicitly claims malformed input fails loudly; an odd `data` length violates that.
- fix: Reject odd-length sample data:
```rust
if data.len() % 2 != 0 {
    anyhow::bail!("WAV data chunk has odd byte length {}", data.len());
}
```

### [MEDIUM] `language` hint is cloned but discarded for monolingual engines, and `TranscriptionResult.language` lies
- file: src/always/stt_local.rs:139, 159-183
- category: correctness
- confidence: medium
- impact: For monolingual engines (Parakeet/Moonshine/GigaAM) the configured `language` is ignored during inference (correct — they don't accept it) but is still echoed back verbatim in `TranscriptionResult.language` (`self.language.clone().unwrap_or_default()`). Downstream post-processing / per-app routing that keys off `result.language` gets a label the engine never actually used or detected, which can mis-drive language-dependent post-processing.
- problem: line 139 `language: self.language.clone().unwrap_or_default(),` reports the *requested* hint, not what the engine produced. The Parakeet/Moonshine/GigaAM arms (159-183) don't pass language at all, so the reported value is unverified. (Auto-detecting engines like Whisper/Canary discard their detected language too, but that's a smaller loss.)
- fix: Either return an empty `language` for engines that don't consume/return it, or thread through the engine's detected language where the upstream `result` exposes it. At minimum document that `.language` is the *requested* hint, not the detected one, so callers don't treat it as authoritative.

### [LOW] No upper bound / sanity check that decoded sample count is non-trivial before inference
- file: src/always/stt_local.rs:120-123
- category: error-handling
- confidence: medium
- impact: Only the exactly-empty case is short-circuited (`if samples.is_empty()`). A 1–2 sample clip (e.g. a truncated/garbage WAV with a 2-byte data chunk) is still pushed into the engine, which for some ONNX models can panic or error on inputs shorter than one frame/window — turning a malformed-input case into an engine-level failure rather than a clean empty result.
- problem:
```rust
if samples.is_empty() {
    return Ok(TranscriptionResult::default());
}
```
Anything ≥1 sample proceeds to `run_engine`. There is no minimum-length guard before feeding e.g. `MoonshineModel`/`SenseVoiceModel`.
- fix: Treat sub-frame clips as empty:
```rust
const MIN_SAMPLES: usize = 16_000 / 10; // 100 ms floor
if samples.len() < MIN_SAMPLES {
    return Ok(TranscriptionResult::default());
}
```

### [LOW] Mutex poisoning is terminal: a single engine panic permanently bricks local STT for the daemon's lifetime
- file: src/always/stt_local.rs:125-128
- category: concurrency
- confidence: high
- impact: If any `transcribe_*` call inside `run_engine` panics while the lock is held (FFI/ONNX/whisper-cpp can panic on malformed tensors), the `Mutex` becomes poisoned. Every subsequent utterance then fails with "local engine mutex poisoned" forever — the always-on daemon silently loses local transcription until restarted, with no recovery and no reload.
- problem:
```rust
let mut engine = self
    .engine
    .lock()
    .map_err(|_| SttError::Other(anyhow::anyhow!("local engine mutex poisoned")))?;
```
There is no `into_inner()` recovery and no rebuild-on-poison path. Given these engines wrap C/ONNX code that can abort on bad input, a single bad utterance can be permanently fatal to the feature.
- fix: Recover the guard from poison so a one-off panic doesn't brick the engine for the process lifetime:
```rust
let mut engine = match self.engine.lock() {
    Ok(g) => g,
    Err(poisoned) => {
        tracing::warn!("local engine mutex was poisoned; recovering");
        poisoned.into_inner()
    }
};
```
(If the poisoning panic left engine state corrupt, prefer signalling the dispatch layer to rebuild the `LocalTranscriber`; at minimum recover rather than fail every call forever.)
