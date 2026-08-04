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

❓ Step 9 currently presses Return with **no delay by default**, which has caused
messages to send mid-thought. Product decision needed on the default.

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
marked as streaming. `MoonshineStreaming` models do stream and render partial
text as the user speaks.

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
These engines produce live partial text (§5). Moonshine models are English only;
Nemotron 3.5 supports 40 language-locales with auto language detection.

### 8.1 NVIDIA Nemotron 3.5 ASR — implementation notes

Nemotron 3.5 ASR Streaming 0.6B is implemented via the `parakeet-rs` crate (0.3.6,
pinned for ort 2.0.0-rc.12 compatibility). It provides multilingual streaming ASR
with 40 language-locales, auto language detection, and punctuation.

**Model files:** The ONNX export from HuggingFace (pantinor/nemotron-3.5-asr-streaming-0.6b-onnx)
includes:
- `encoder.onnx` + `encoder.onnx.data` (~2.4 GB)
- `decoder_joint.onnx` (93 MB)
- `tokenizer.model` (0.4 MB)

Total size: ~2500 MB. The model is a directory (not a single file) and requires
the `local-stt` feature to be enabled.

**Implementation details:**
- Loaded via `parakeet_rs::Nemotron::from_pretrained(path, None)`
- Non-streaming transcription uses `transcribe_audio(&samples)`
- Streaming transcription uses `transcribe_chunk(&audio_chunk)` with 560ms chunks (8960 samples @ 16kHz)
- The loader auto-detects English-only vs multilingual variants from the encoder ONNX graph
- Multilingual variant optionally accepts a target language code (e.g., "es-ES", "ja-JP") via `set_target_lang()`

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

## 10. Preferences

Stored in the daemon's database, editable from Settings and the CLI
(`always-daemon config set <key> <value>`).

| Key | Default | Meaning |
|---|---|---|
| `lang` | `auto` | Dictation language |
| `stt_energy_threshold` | 0.012 | Speech energy floor |
| `hear_energy_threshold` | 0.001 | Wake-on-voice floor |
| `silero_threshold` | 0.4 | VAD speech probability |
| `stt_silence_secs` | 1.4 | Silence that ends an utterance |
| `stt_adaptive_silence` | true | Extend the window mid-sentence |
| `stt_auto_enter` | true | Press Return after pasting |
| `auto_enter_delay_ms` | 0 | Grace period before Return ❓ |
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
2. Auto-Enter with no delay — should a grace period be the default?
3. Silence window at 1.4 s — long enough for natural thinking pauses?
4. ~~Should a streaming local model become the default so live text works?~~
   **Resolved 2026-08-04:** `moonshine-small-streaming-en` downloaded and made
   active, so the overlay's live-text path has something to render. English
   only — revisit if multilingual dictation matters more than live text.
5. Mic conflict discards the interrupted sentence rather than pasting it — right
   call?
6. Nemotron — fix and restore, or leave out?
