# Always — Product Specification

The behaviour Always is expected to have. Written from the code as it stands, so
it describes what the product *does*, and — where marked — what it *must* do.

**This file is normative.** If the code and this spec disagree, one of them is a
bug. Say which before changing either.

Items marked ❓ are ones the author of this document could not verify and the
product owner should correct.

---

## 1. What Always is

An always-on dictation tool for macOS. The user speaks; their words are
transcribed and typed into whatever application has focus. There is no
push-to-talk and no window to click into: the microphone is live, and speech
becomes text.

Two things follow from "always-on", and they shape every rule below:

- **It must never type words the user did not intend to dictate.** A false paste
  is worse than a missed one — it lands in someone else's document, chat, or
  terminal.
- **The user must always know whether it is listening.** An always-on microphone
  with no visible state is not acceptable.

---

## 2. Invariants

Load-bearing rules. Breaking any of these is a regression regardless of what
else improved.

**I1. The listening indicator must be visible when it is shown.**
Not "ordered front", not "alpha 1.0", not "on a screen" — *visible to the user*,
including over full-screen applications. A HUD that renders behind other content
has failed, and every internal signal saying otherwise is irrelevant.

**I2. The listening indicator must stay on screen long enough to be read.**
Minimum 600 ms once shown. Bursts of internal state changes must not tear it
down before a human can see it.

**I3. Nothing is typed into the focused application unless the daemon is
listening, unpaused, and — when My Voice is on — the speaker was verified.**

**I4. Only one recorder.** One `rec` process, driven from a single thread. Two
readers of the microphone deadlock or starve each other.

**I5. Only one daemon and one GUI.** Duplicates fight over the microphone and
the socket.

**I6. Audio captured while listening was suppressed must never be transcribed
or pasted.** Applies to every pause source.

**I7. When another application takes the microphone, Always stops immediately
and does not paste.** Two dictation tools must never transcribe the same speech.

**I8. A model may only be offered if it can actually run.** No catalogue entry
may reference an engine with no implementation.

**I9. The user's own voice profile never leaves the machine, and transcripts are
not logged by default in release builds.**

**I10. The app the user is running must be the build that was last built.**
See §12.

---

## 3. Architecture

Two processes, plus a recorder:

| Process | Role |
|---|---|
| `Always` (Swift, menu bar) | UI: menu bar item, overlay HUD, settings, onboarding. Owns nothing about audio. |
| `always-daemon` (Rust) | Audio capture, VAD, speaker verification, transcription, pasting. |
| `rec` (SoX) | Raw microphone capture, 16 kHz mono 16-bit, one instance, spawned by the daemon. |

The GUI spawns the daemon and talks to it over a Unix domain socket at
`~/Library/Caches/always/always.sock`. Messages are newline-delimited JSON; the
daemon broadcasts events, the GUI sends commands.

**Socket rules:**
- A client falling behind must be told it missed events, never disconnected.
  Dropping the connection makes the GUI reconnect and re-read the entire initial
  state, during which the user sees nothing.
- The GUI must reconnect on its own, with backoff, and respawn the daemon if it
  is genuinely gone.
- The daemon sends its full current state on connect, so a reconnecting GUI is
  never out of sync.

---

## 4. The dictation lifecycle

1. **Idle** — recorder running, VAD scoring every 30 ms frame.
2. **Speech onset** — frames pass the energy floor and Silero's threshold for
   `onset_ms` (60 ms). The utterance begins; a short pre-buffer (200 ms) is
   prepended so the first syllable is not clipped.
3. **Listening shown** — see §5.
4. **Speaking** — audio accumulates. Long utterances are committed in chunks and
   transcribed as they go, so a two-minute dictation does not wait until the end.
5. **Pause detected** — a tentative silence starts a *speculative* transcription
   in the background. If speech resumes, it is discarded.
6. **End of utterance** — silence exceeds the configured window
   (`stt_silence_secs`, default 1.4 s). With adaptive silence on, the window
   extends when the text looks mid-sentence.
7. **Transcribe** — the speculative result is used if still valid, otherwise a
   fresh transcription runs.
8. **Post-process** — filters, glossary corrections, grammar cleanup (§9).
9. **Paste** — text is typed into the focused application. If auto-Enter is on,
   Return follows after `auto_enter_delay_ms`.

**Pause tolerance is the difference between dictating and being interrupted.**
Two settings govern it, and the shipped defaults were both too aggressive for
real speech: a 1.4 s silence window ended utterances during ordinary thinking
pauses, and auto-Enter with no grace period then sent the half-finished message.
This instance now runs 2.2 s and 800 ms.

❓ The code defaults are still 1.4 s / 0 ms. Product decision needed on whether
to move them for everyone.

---

## 5. The listening indicator (HUD)

A small floating badge showing what Always is doing.

**States:** Listening · Transcribing (with elapsed seconds after 5 s) ·
Transcribing-with-text · Paused · Resumed · Filtered · Auto-Enter countdown ·
Idle-paused · Low mic volume · correction confirmations.

**Rules:**

- **Activity-only.** It represents something happening — the user speaking, or a
  transcription running. It is not an "app is alive" light and must not sit on
  screen while idle.
- **Visible above everything** (I1). Window level must be above normal and
  full-screen application content. It is a status indicator, and it belongs at
  the same tier as the menu bar, below system alerts.
- **Minimum 600 ms on screen** (I2).
- Follows the active screen and the cursor; must be fully on a screen the user
  is looking at.
- Fixed-size panel: transcript text wraps within the fixed content width and
  never enlarges or moves the HUD as interim text changes. Normal mode shows
  at most four wrapped lines; compact mode remains a single truncated line.
- Hidden entirely when paused, disconnected, or when the user selects the hidden
  display mode.

**When "Listening" appears:**

| Situation | Timing |
|---|---|
| My Voice off | On voice onset — immediately. |
| My Voice on, speaker verified recently | On voice onset — immediately (trust window, §6). |
| My Voice on, no recent verification | Only once the speaker is confirmed. ~0.6 s minimum: the voiceprint model requires 0.5 s of voice. |

**Live transcript text** is only shown for models that genuinely stream partial
results. A model that returns one finished transcript per utterance must not
claim streaming — doing so produces a useless flash of text *after* the words
have already been pasted.

Live text requires a streaming engine to be **active**, not merely available. The
cloud backend returns one finished transcript per utterance and must not be
marked as streaming. `MoonshineStreaming` and `Nemotron` models do stream and
render partial text as the user speaks.

The daemon's live-preview loop (`vad.rs`) re-transcribes the growing buffer on
an interval while the user is still talking and arms whenever *either*: an
external stream consumer opted in via `SetConsumeMode` (regardless of engine),
**or** `Transcriber::supports_streaming()` reports the active engine genuinely
streams. The cloud backend always reports `false` — firing this loop every
~200ms would mean a network round trip that often — so normal dictation on
Groq still only shows one flash of speculative text at a pause, exactly as
before. A local streaming engine (Nemotron, MoonshineStreaming) reports `true`
and gets continuous live preview during normal dictation too, not just under
an external consumer.

---

## 6. My Voice (speaker verification)

Optional. When on, only the enrolled user's voice is transcribed — other people,
television, meeting audio and music are ignored.

**Enrollment** records three passes (normal, lower, louder), each needing 5 s of
voice. Each pass is stored as its own embedding, plus their combined average.

**Scoring:** an utterance's embedding is compared against the enrolled profile;
above the threshold, it is the user.

- Compare against **the best-matching enrolled style**, not only the combined
  average. The average is not equal to any style the user actually speaks in,
  and scoring against it alone costs real similarity for no benefit.
- The model requires **0.5 s of voice minimum** — this is the floor on how fast
  identity can be established, and no tuning removes it.
- Checks repeat as the user speaks, so a long utterance ends when *they* stop,
  not when the room goes quiet.
- Audio that fails is discarded before any transcription is paid for.

**Trust window:** after a confirmed match, the indicator appears on voice onset
for a period without re-proving identity. This trades a possible brief flash for
media or another voice against re-paying the 0.5 s floor on every sentence.

❓ Currently 5 minutes. This is the single most aggressive value in the product
and directly re-opens the "badge appears when I'm not talking" complaint.
Product decision needed.

**Threshold** is a preference (currently 0.40). Measured on the owner's profile:
their own voice scores 0.50–0.67, other audio scores below 0.30 in the vast
majority of cases. ❓ The value should be re-derived if the voiceprint is
re-recorded.

---

## 7. Pause model

Listening can be suppressed by several independent sources. They do not
overwrite each other — a manual pause survives a call ending.

| Source | Trigger | Clears when |
|---|---|---|
| Master | User toggles pause (shortcut or menu) | User resumes |
| Per-app | Focused app is on the paused list | Focus moves to an allowed app |
| Mic conflict | Another app holds the microphone | Mic free for ~3 s (§7.1) |
| Audio output | System audio is playing | Playback stops |
| Idle | No voice for `idle_pause_secs` | Voice detected again |
| No GUI | Daemon lost its last client | A client connects |

**Rules:**
- Any active source suppresses capture. The explicit global resume clears all of
  them at once.
- On resume, audio queued by the recorder during the suppressed period is
  **discarded** (I6). The recorder never stops, so its buffer holds several
  seconds of exactly the audio Always was meant to ignore.
- Focus-driven pause/resume must be silent — switching windows must not flash
  badges.

### 7.1 Microphone conflicts

Another application taking the microphone is detected within about a second.

- Capture stops **immediately**, mid-utterance, not at the next loop boundary.
- Words captured before the cut are still transcribed and recorded in the log,
  but **never pasted** — the other app is about to paste its own version.
- Listening resumes only after the microphone has been continuously free for
  about 3 s. Dictation apps release the mic between phrases; resuming on the
  first free moment means Always starts listening to speech still aimed at the
  other app.

---

## 8. Models and transcription backends

A cloud backend and a set of downloadable local models. Local models run
offline; the cloud backend is faster to start and needs an API key.

**Rules:**
- **Never list a model that cannot run** (I8). Every catalogue entry must name an
  engine with a real implementation, a correct download size, and a checksum.
- A local model may be armed as fallback for the cloud backend; failures fall
  back silently but are logged.
- Switching models takes effect on the next utterance.

**Streaming models available:** `moonshine-tiny-streaming-en`,
`moonshine-small-streaming-en`, `moonshine-medium-streaming-en`, `nemotron-3.5-asr-streaming-0.6b`.
These engines expose partial text events (§5). Moonshine models are English only;
Nemotron 3.5 supports 40 language-locales with auto language detection.

### 8.1 NVIDIA Nemotron 3.5 ASR — implementation notes

Nemotron 3.5 ASR Streaming 0.6B is implemented via the `parakeet-rs` crate (0.3.6,
pinned for ort 2.0.0-rc.12 compatibility). It provides multilingual streaming ASR
with 40 language-locales, auto language detection, and punctuation.

**Model files:** The ONNX export from HuggingFace
(`pantinor/nemotron-3.5-asr-streaming-0.6b-onnx`) publishes four loose files, whose
names `parakeet-rs` looks for exactly:

| File | Bytes |
|---|---|
| `encoder.onnx` | 42,164,972 |
| `encoder.onnx.data` | 2,454,405,120 |
| `decoder_joint.onnx` | 97,590,054 |
| `tokenizer.model` | 406,554 |

Total 2,594,566,700 bytes (2474 MB). It is a directory model and needs the
`local-stt` feature.

**Distribution — this model has no archive.** HuggingFace serves each file
separately and offers no repo-tarball endpoint, so the usual "one `.tar.gz` URL"
catalogue shape cannot express it. Pointing `url` at a constructed `.tar.gz` path
returns 404 and produces a model that downloads nothing and spins forever — the
entry shipped that way once and had to be removed.

Catalogue entries may therefore declare a **file list** (`ModelInfo::files`)
instead of `url`. Each file carries its own URL, SHA256 and size; the daemon
fetches them into a `.downloading` staging directory, verifies each against its
checksum as it lands, and only then renames the directory into place and writes
the `.verified` marker. An interrupted download can never leave a half-populated
directory that looks installed. Progress is aggregated from the declared sizes,
since there is no single content-length to report.

`url` and `files` are mutually exclusive; a test enforces this, along with real
checksums, real sizes, `is_directory`, filenames that cannot escape the model
directory, and a declared `size_mb` that agrees with the sum of the parts.

**Implementation details:**
- `LoadedEngine::Nemotron` holds a **shared, read-only `parakeet_rs::NemotronHandle`** (loaded via `NemotronHandle::load(path, None)`), never a bare `Nemotron` instance. Every call — the one-shot final path and each streaming session — spawns its own independent `Nemotron::from_shared(&handle)` with fresh decoder state, transcribes, and drops it. This is required, not cosmetic: `Nemotron::transcribe_audio` (the one-shot path) starts by resetting the same cache-aware decode state (`encoder_cache`, LSTM `state_1`/`state_2`, `last_token`) that a concurrent `transcribe_chunk` streaming sequence depends on, and the streaming path releases `self.engine`'s mutex between chunks (so a slow decode doesn't block unrelated daemon work) — sharing one `Nemotron` between the two paths let the daemon's independent "speculative transcription" (fires ~240ms after any pause, unrelated to streaming preview state) silently corrupt an in-progress streaming decode. Per-call isolation removes the shared mutable state entirely; no additional locking is needed.
- Non-streaming transcription uses `Nemotron::from_shared(&handle)` then `transcribe_audio(&samples)`
- `LocalTranscriber::transcribe_streaming` clones the handle out of `self.engine` once, then uses stateful `transcribe_chunk(&audio_chunk)` calls on its own `Nemotron` instance with 560ms chunks (8960 samples @ 16kHz)
- Each preview snapshot spawns a fresh (already-reset) instance, pads its final chunk to 8960 samples, and sends three silent flush chunks
- VAD consume-mode previews call `transcribe_streaming`; preview events contain cumulative text because the Swift monitor replaces its stored partial transcript on each event
- The final paste path remains non-streaming and uses `transcribe_audio(&samples)`
- Rust test coverage covers the chunk-splitting/padding math (560ms sizing, ragged-tail zero-padding, flush-tail count) without a loaded model; real Nemotron model inference and long-running memory behavior remain unverified by automated tests since that needs ~2.5GB of real weights this repo doesn't ship
- The loader auto-detects English-only vs multilingual variants from the encoder ONNX graph
- Multilingual variant accepts a target language code via `apply_nemotron_language()` → `set_target_lang()`, driven by the Settings language picker (Swift) and the daemon's existing `cfg.lang` plumbing (`uds_server.rs::set_language`). **Known format mismatch**: `cfg.lang` and this model's own catalogue entry use bare ISO 639-1 codes ("es", "ja", "zh", ...), but parakeet-rs's `PROMPT_DICTIONARY` only has bare-code entries for most languages — "ja"/"zh" require locale-tagged form ("ja-JP", "zh-CN"). A bare "ja"/"zh" selection is rejected by `set_target_lang` and falls back to auto-detect with a logged warning rather than failing the transcription. Mapping "ja"→"ja-JP"/"zh"→"zh-CN" before the call is a known follow-up, not yet done.

**Why it was previously absent:**
The original implementation attempt failed because:
- `transcribe-rs` did not include a Nemotron engine
- The published ONNX exports did not fit transcribe-rs's Parakeet loader
- The catalogue entry had incorrect metadata (600 MB vs 2258 MB download, no checksum, pointed at .nemo archive)

The solution was to use `parakeet-rs` instead, which provides a dedicated Nemotron
implementation compatible with the ONNX Runtime this project already depends on.

---

## 9. Text pipeline

Between transcription and the keyboard:

1. **Hallucination filter** — rejects the known failure modes of speech models:
   empty output, "thank you"/"bye" family, subtitle credits, repeated tokens,
   gibberish, low-confidence segments.
2. **Vocabulary bias** — the user's glossary is sent to the model as a hint so
   domain terms transcribe correctly. **The hint must never reach the user's
   text**: these models echo their prompt back, especially on short or quiet
   audio, and the echo is indistinguishable from speech to everything downstream.
3. **Glossary corrections** — known mistranscriptions mapped to canonical terms.
4. **Snippets / text expansion.** ❓ Not verified in detail.
5. **Grammar cleanup** — an LLM pass, on by default (`postprocess_enabled`).
6. **Corrections** — the user can log a correction for a wrong transcription;
   passive capture of clipboard edits is available but off by default.

---

## 9a. First-run setup

Shown **once**, to someone who has never been through it, and never again.

Two steps:

1. **Permissions.** Microphone and Accessibility are required and gate the
   Continue button — without them nothing works at all. Input Monitoring is
   offered but never blocks: it enables the global shortcuts, not dictation.
   The window polls permission state, so granting in System Settings updates it
   without a restart.
2. **My Voice.** The three enrollment recordings, inline. Always skippable —
   "Skip for now" and "Finish without it" both complete setup.

**Completion is a recorded fact, not an inference.** It was previously derived
from "is a Groq API key saved?", which was wrong in both directions: a user
running a local model has no key and was re-shown the welcome window on every
launch forever with no way to stop it, while a user who already had a key never
saw onboarding at all and was never walked through permissions. Completion is
now stored under `onboardingCompletedV1` and written the moment the user
finishes or skips.

**Existing installs are never shown it.** On first run of a build carrying this
flag, a saved API key or a recorded voice profile is taken as proof the user has
used the app before, and the flag is retro-marked. Upgrading must not greet a
working setup with a welcome window.

**The window is always dismissable.** No step may hold a first-run user hostage.
The Groq API key was deliberately removed from this flow for that reason — it is
not required (local models need none) and it now lives in Settings → Models.

---

## 10. Preferences

Stored in the daemon's database, editable from Settings and the CLI
(`always-daemon config set <key> <value>`).

| Key | Default | Meaning |
|---|---|---|
| `lang` | `auto` | Dictation language |
| `stt_energy_threshold` | 0.012 | Speech energy floor |
| `hear_energy_threshold` | 0.001 | Wake-on-voice floor |
| `silero_threshold` | 0.4 | VAD speech probability |
| `stt_silence_secs` | 1.4 (this instance: 2.2) | Silence that ends an utterance. CLI key is `stt_silence`. |
| `stt_adaptive_silence` | true | Extend the window mid-sentence |
| `stt_auto_enter` | true | Press Return after pasting |
| `auto_enter_delay_ms` | 0 (this instance: 800) | Grace period before Return |
| `speaker_gate_enabled` | true | My Voice |
| `speaker_gate_threshold` | 0.40 | Identity cut-off |
| `idle_pause_secs` | 0 (off) | Idle auto-pause |
| `postprocess_enabled` | true | Grammar cleanup |
| `transcript_stream` | true | Append accepted utterances to `~/.always/transcripts.jsonl` |
| `audible_status_sound` | off | Sound cues |
| `per_app_settings_json` | — | Per-application pause rules |

Shortcuts: pause `ctrl+alt+p` · auto-enter `ctrl+alt+a` · force paste
`ctrl+alt+v` · log correction `ctrl+alt+x` · correction dialog `ctrl+alt+w`.

---

## 11. Logging and diagnostics

- Daemon log: `~/Library/Logs/Always/always.YYYY-MM-DD`, JSON lines.
  `always logs --pretty` renders it.
- **Transcripts are only logged in debug builds**, or with
  `ALWAYS_LOG_TRANSCRIPTS=1` (I9).
- Overlay timing instrument: `ALWAYS_OVERLAY_TIMING=1` writes each hop —
  event received, reached the main thread, ordered on screen, with window level
  and screen geometry — to `~/Library/Logs/Always/overlay-timing.log`. Off by
  default.
- Any user-visible latency claim must be measured from **first speech**, not
  from an internal marker that fires later.

---

## 12. Build, install, run

`scripts/dev-rebuild.sh` is the only supported path. It kills the app and
daemon, builds Rust and Swift, deploys to `/Applications/Always.app`, relaunches,
and **verifies the running processes are newer than the binaries** — exiting
non-zero if not.

**Nothing is "done" until the new build is running** (I10). "Built" and
"deployed" are different claims, and only "running" is testable.

---

## 13. Open questions for the product owner

1. Trust window at 5 minutes — how much badge-flashing is acceptable in
   exchange for an instant indicator?
2. ~~Auto-Enter with no delay~~ / ~~silence window at 1.4 s~~ — **resolved for this
   instance 2026-08-05**: 2.2 s and 800 ms, after both cut the user off
   mid-thought. Open question is whether the code defaults should follow.
4. ~~Should a streaming local model become the default so live text works?~~
   **Resolved 2026-08-04:** `moonshine-small-streaming-en` downloaded and made
   active, so the overlay's live-text path has something to render. English
   only — revisit if multilingual dictation matters more than live text.
5. Mic conflict discards the interrupted sentence rather than pasting it — right
   call?
6. ~~Nemotron — fix and restore, or leave out?~~ **Resolved 2026-08-15:**
   Nemotron is restored with state-reset, padded 560 ms chunk processing,
   trailing-frame flushes, and intact UTF-8 partial transcript updates.
