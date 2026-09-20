# Always — Product Specification

> The first speech-to-text software designed to be always on and free your hands
> from the affordance.

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

**I3. Nothing is typed into the focused application unless the words were
captured while the daemon was listening, unpaused, and — when My Voice is on —
the speaker was verified.**
The test binds on **capture time, not paste time** (reworded 2026-08-31 at the
product owner's request). What makes a paste legitimate is that the user
authorised the words by speaking them under those conditions; a pause that
arrives afterwards suppresses further *listening*, and does not retroactively
un-authorise speech already given. This is what lets master pause keep the
in-flight utterance (§7) without weakening the rule. It does not loosen I6:
audio captured *during* suppression is still never transcribed or pasted.

**I3a. Under My Voice, no audio reaches transcription unless the utterance
matches the enrolled voiceprint *as a whole*.**
A single matching window during capture is not sufficient authority — it is one
trial in a series, and background media wins it eventually. Every path to STT is
covered: the final transcript and each committed chunk. See §6.

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
   **Chunking is suppressed while a live decode session is carrying the
   utterance** (§4a): every flush resets that session, so chunking a streaming
   utterance replaced one flat ~130 ms finalization with a from-scratch
   one-shot decode of every 6 s of speech, and the end-of-utterance wait grew
   with how long the user talked (measured 487 ms streaming → 880 → 1086 →
   1296 ms once chunking engaged). Rolling chunks resume past 120 s of speech —
   the longest span over which streaming was measured flat — and immediately if
   the session dies or its audio is truncated.
4a. **Live decode (streaming engines — Nemotron, and Apple on macOS 26+)** — a single
   persistent decode session is fed the audio in 560 ms windows as it is
   captured, so the transcript is built *while the user speaks*. The overlay
   preview is read straight off that session; no extra decode runs.
5. **Pause detected** — at a tentative silence the grammar LLM is pre-warmed
   for the expected final text (§9), and the adaptive-silence verdict is taken.
   On a live session both use the transcript that already exists. Without one,
   a *speculative* background transcription starts instead and both wait on it;
   if speech resumes, it is discarded.
6. **End of utterance** — silence exceeds the configured window
   (`stt_silence_secs`, default 1.4 s). With adaptive silence on, the window
   moves in *both* directions:
   - **Extends** (×2, capped at +1.5 s) when the transcript looks mid-sentence.
     On the **live** path that means an explicitly unfinished trailing mark
     (`,` `;` `:` `—` `-`) or a trailing connector / hesitation filler. On the
     **speculative** path (cloud engines, which punctuate reliably) a missing
     sentence terminator also counts.
   - **Shortens to 300 ms** when the transcript reads as a finished sentence:
     its last word is not a connector or filler, it does not end on an
     unfinished mark, and it is at least three words long. A sentence
     terminator counts when present but is **not required** — local streaming
     engines do not punctuate dictation. The short window is clamped to the
     configured one, so it can only ever shorten the wait.

   The two verdicts are a ladder: extend wins, then shorten, then the window is
   left alone. They must never both hold for the same text. The live path
   deliberately does **not** treat missing punctuation as "unfinished": doing so
   made the extend branch true on every unpunctuated utterance, which is every
   utterance on a local streaming engine, so the shorten branch was unreachable
   (measured: it fired 0 times, ever) and ordinary dictation silently paid the
   *doubled* 1.8 s window instead of the configured 0.9 s.

   The ladder is evaluated from the tentative mark onward, every second frame,
   plus one guaranteed last look before the base cut. It requires only that the
   session was not invalidated, that voice was logged, and that the speaker gate
   has verified the user. It deliberately does **not** require "speech has landed
   since the last chunk flush": that flag exists to avoid *spending* a
   speculative decode on trailing silence, and the live verdict spends nothing.
   Requiring it cost every chunked utterance its verdict for the whole remaining
   silence run, because the flush clears the flag and only a voiced frame sets it
   again — and the flushing pause is usually the end of the utterance.

   Every evaluation is logged: `midsentence_decision`, `early_finalize_decision`,
   or `early_finalize_skipped` with the exact precondition that was false
   (`no_live_session`, `worker_behind`, `no_transcript`, `adaptive_disabled`,
   `short_utterance`, `tail_not_decoded`, `already_extended`,
   `silence_below_early_window`, `not_complete_utterance`). Because that log
   lived *inside* the gate, a false gate produced no reason at all; the gate is
   therefore reported from outside itself by `silence_verdict_state` — one
   unconditional line per silence run, at the tentative mark, naming every input
   including the live-session state. A gate that can be false must say so from
   outside itself, and a verdict that only logs when it succeeds cannot be shown
   to be dead.

   Both verdicts require that *every voiced sample has actually been decoded
   into the transcript*. The live session withholds a partial trailing window
   of up to 560 ms from the decoder, so "caught up" alone does not mean the
   user's last words are in the text; judging a truncated transcript reads as
   "unfinished" almost by construction and extended the window on utterances
   that were in fact complete. The verdict is deferred frame by frame until the
   trailing audio is covered — the silence itself flows into the decoder, so
   this resolves within one 560 ms window. If it never resolves before the
   configured window expires, no verdict is taken and the window applies
   unchanged. Shortening additionally requires a live decode session; the
   speculative path can still only extend.

   In practice the shortened cut lands 300-660 ms after the last voiced frame
   (never at exactly 300 ms unless speech happened to end on a 560 ms decoder
   boundary), plus ~130 ms of flat live finalization.
7. **Transcribe** — with a live session the transcript is already complete:
   finalization feeds the last partial window plus one silent flush window and
   takes the accumulated text (measured 124-139 ms, and **flat** — a 40 s
   utterance finalizes as fast as a 5 s one). Otherwise the speculative result
   is used if still valid, else a fresh whole-utterance transcription runs
   (~0.10x realtime: 0.5 s for 5 s of audio, 3.8 s for 40 s, 11.5 s for 2 min).
   A live session that failed, timed out, decoded nothing, or whose audio was
   truncated by the speaker gate falls back to that same fresh transcription.
8. **Post-process** — filters, glossary corrections, grammar cleanup (§9).
   A rule-filtered utterance (not a hallucination) is still copied to the
   clipboard — no paste, no auto-Enter — so a wrong filter verdict costs a
   manual ⌘V instead of the whole utterance.
9. **Paste** — text is typed into the focused application. If auto-Enter is on,
   Return follows after `auto_enter_delay_ms`.

**Pause tolerance is the difference between dictating and being interrupted.**
Two settings govern it, and the shipped defaults were both too aggressive for
real speech: a 1.4 s silence window ended utterances during ordinary thinking
pauses, and auto-Enter with no grace period then sent the half-finished message.
This instance now runs 1.1 s (lowered from 2.2 s on 2026-08-26 after log
analysis showed real pauses p95 ≈ 1.25 s and the adaptive mid-sentence
extension covering the tail) and 0 ms auto-Enter delay.

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
- Fixed width, growing height: transcript text wraps within the fixed content
  width — the panel never widens or shifts horizontally as interim text
  changes. In normal mode the panel's height grows to fit the wrapped
  transcript, from the classic 130 pt up to a 200 pt cap (~6 wrapped lines),
  and shrinks back when the text shortens or the state changes. The bottom
  edge stays anchored, so growth is upward and can never push the HUD
  off-screen. Text taller than the cap is head-truncated — a leading "…"
  replaces the oldest words so the words just spoken stay visible. Compact
  mode remains a fixed single truncated line.
- Hidden entirely when paused, disconnected, or when the user selects the hidden
  display mode.
- **Live feedback preempts transient confirmations.** Brief confirmation
  flashes (Pause/Resume, Auto-Enter toggles, low-mic volume, offline
  fallback — 1.5 s to 4 s) normally run their full duration, but if the user
  starts speaking (Listening) or a transcription state arrives while one is
  showing, the flash ends immediately and the live state shows at once.
  Lower-priority states still wait for the flash to finish, so a
  confirmation is never cut short by e.g. a stale state re-emission.

**When "Listening" appears:**

| Situation | Timing |
|---|---|
| My Voice off | On voice onset — immediately. |
| My Voice on | After the speaker is verified — the first check fires at ~0.3 s of voiced audio (the embedder's minimum window, padded by the 200 ms pre-buffer), rechecked every 0.25 s of new voice; typically under ~1 s live, up to ~2 s for marginal audio. Background media and other voices do NOT flash the badge; the indicator appears only for the enrolled user. |

The verify wait under My Voice is the price of the gate doing its job: the
badge must not register audio the user did not produce. An earlier "optimistic
badge on onset" experiment flashed the overlay on every non-user voice (videos,
meetings, sleep-time media) and was reverted at the owner's request — see §6.
The latency budget is attacked at the check schedule (earlier first attempt,
tighter retry cadence), never by weakening the threshold — a noisy short window
that cannot clear the full bar simply falls through to the next check.

**Live transcript text** is shown whenever the daemon produces a provisional
preview — the GUI renders any non-empty partial it receives, during the
transcribing wait *and* while the user is still talking. The daemon, not the
GUI, decides when previews flow. (Previously the GUI discarded previews unless
the active model claimed streaming; that gate is gone.) A model that returns
one finished transcript per utterance must still not claim streaming —
`Transcriber::supports_streaming()` stays `false` for the cloud backend.

**A live decode session supersedes the preview loop entirely.** When the active
engine offers one (`Transcriber::open_live_stream`, today Nemotron only), the
overlay preview is the session's own cumulative transcript, broadcast whenever
it changes and at most every `LIVE_STREAM_PREVIEW_MIN_GAP_MS` (250 ms). It
costs no extra decode, and the text is the *cumulative* transcript rather than
concatenated per-chunk returns — those split words at window boundaries
("whe ther", "finali zes") because the tokenizer emits sub-word pieces.
`preview_cadence` is not consulted at all in that case; the re-decode loop
below would otherwise contend with the session for the one shared ONNX model
mutex (measured: seven previews in a single 30 s utterance at 318-1966 ms each,
with the final decode queued behind all of them).

The re-decode preview loop (`vad.rs`, `preview_cadence`) re-transcribes the
growing buffer on an interval while the user is still talking, for engines with
no live session. Three ways it arms, in priority order:

1. **Consume mode** (`SetConsumeMode`, regardless of engine) — fast cadence:
   every `CONSUME_STREAM_INTERVAL_MS` (200ms), self-limited to one round trip
   at a time. Unchanged; external consumers (Iris) depend on it, and its
   preview payloads are never prefixed with chunk text.
2. **A genuinely-streaming local engine** (`supports_streaming()` true —
   Nemotron, MoonshineStreaming) — same fast cadence; decode is local and
   cheap. Unchanged.
3. **The `stt_live_preview` preference (default ON)** — non-streaming cloud
   backends (Groq) get a SLOW cadence: at most one preview every
   `LIVE_PREVIEW_INTERVAL_MS` (1.5s), and only after ~1s of NEW audio since
   the last preview (`LIVE_PREVIEW_MIN_NEW_SAMPLES`). Each tick is a full
   cloud round trip, so a minute of continuous dictation costs tens of extra
   API calls, not hundreds. The preview audio is capped to the last ~10s
   (`CONSUME_STREAM_PREVIEW_MAX_SAMPLES`); for chunked long utterances the
   already-settled chunk texts are prefixed (when cheaply available) so the
   overlay shows the whole sentence, not just the open chunk.

Starvation safety for the slow cloud path: a tick is skipped while a
tentative-silence speculation is pending, while a previous preview is still
in flight (`preview_pending` single-flight), and entirely until the speaker
gate has verified the utterance (no API calls for audio that may be dropped).
It never flips the overlay to "Transcribing" — the user is still talking, so
the partial text renders under the listening state. The final transcription
path (speculation → final cut → paste) is untouched by all three arms.

With `stt_live_preview` off, Groq dictation shows no mid-speech previews;
the free speculative preview at a pause (below) still renders once.

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

**A single matching moment never authorises an utterance.** Verification during
capture is scored on a 1.5 s trailing window, retried every 0.5 s, against the
best of four enrolled embeddings. That is a repeated trial, and against
continuous media it eventually succeeds by chance: measured on 2026-08-31 with a
Hindi video playing, three windows crossed a 0.35 threshold (0.362, 0.365,
0.375) and each one released a whole utterance to the clipboard. So the audio is
re-scored **as a whole** before it can be transcribed, at the same threshold, and
is discarded if the utterance does not match — no matter what an individual
window said. Over the same recording, 42 whole-utterance scores peaked at 0.341;
none reached the bar. Chunks of a long dictation are confirmed the same way
before they are committed, since a committed chunk leaves the buffer for good.

Rejecting requires positive evidence. A fragment too short to embed, or an
embedder error, defers to whatever the in-capture checks already decided — the
confirmation can turn an accept into a reject, never the reverse, and it never
discards chunks that were already confirmed. "Too short to embed" means
`samples.len() < MIN_EMBED_SAMPLES` — the embedder's own floor on input length.
A short burst of video/TTS audio that has enough total samples to embed (the
1.5s trailing window is 24000 samples, well above the 8000 floor) but little
voiced audio IS scored and CAN be refuted; the old check that gated on
`voiced_samples >= MIN_EMBED_SAMPLES` let these through as "insufficient" and
deferred to the ladder, which accepted them on the permissive 0.15 window bar.

**The gate gets stricter while the Mac is playing audio**, but only for the
single-window check: `AUDIO_PLAYING_GATE_BUMP` (0.15) is added to the window bar
and never to the whole-utterance bar. Dictating over music must keep working, and
the owner's Nepali scores 0.45–0.52 — a raised whole-utterance bar of 0.50 would
reject half of it. Missing the raised window bar costs the user latency, not
their words: the whole-utterance check at ~2 s still admits them at the
unraised threshold.

"Is audio playing" is tracked as a **fact**, separately from the audio-output
*pause source* below. The two were the same flag, and because the pause source is
deliberately suppressed whenever My Voice is on (§7), the fact was suppressed
with it — the strictness bump was unreachable in the only configuration that
needed it, for as long as it existed.

**No trust window.** An earlier build remembered a confirmed match for a period
(10 s, later 5 min) and let the badge appear on voice onset without re-verifying
inside that window. That made the overlay flash on every non-user voice —
videos, meetings, media played while sleeping — and the mic kept capturing audio
that would never be the user's, which blocked dictation until the media stopped.
Reverted at the owner's request: every sentence re-proves identity from scratch.
The cost is up to ~2 s of verify wait at the start of each utterance under the
gate (§5); the benefit is that the badge and the mic only ever engage for the
enrolled user.

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

The master pause shortcut is configurable via Settings → Shortcuts →
"Master Pause / Mute" or `always config set shortcut_master_pause <combo>`.
Default: `ctrl+alt+shift+p`. Supports the Fn/Globe key as a standalone
shortcut (`fn`) — the Fn key fires as a `flagsChanged` event on macOS,
not `keyDown`, so a dedicated `CGEventTap` catches it (rdev's `keyDown`
tap does not see it). All shortcut changes take effect immediately via
the `ReloadShortcuts` UDS command — the daemon re-reads the prefs DB
and swaps the live keyboard listener's combo set without a restart.
| Per-app | Focused app is on the paused list | Focus moves to an allowed app |
| Mic conflict | Another app holds the microphone | Mic free for ~3 s (§7.1) |
| Audio output | System audio is playing (suppressed entirely when My Voice is on — the gate already ignores non-user voices, so music must not stop dictation) | Playback stops |
| Idle | No voice for `idle_pause_secs` | Voice detected again |
| No GUI | Daemon lost its last client | A client connects |

**Rules:**
- Any active source suppresses capture. The explicit global resume clears all of
  them at once — including the idle auto-pause, so pressing the master chord
  after an idle timeout resumes immediately without needing a separate action.
- On resume, audio queued by the recorder during the suppressed period is
  **discarded** (I6). The recorder never stops, so its buffer holds several
  seconds of exactly the audio Always was meant to ignore.
- An utterance already being recorded when a pause source fires is **not**
  aborted mid-capture (only a mic conflict does that, §7.1). It finishes
  recording and is transcribed in full; the pause decides only whether the
  result is pasted.
- **A pause that arrives mid-utterance normally drops the paste**, so a
  transcript cannot leak into a window the user has since moved to. The text
  is not lost — it is held in the single-slot filtered buffer and the overlay
  offers "press ⌃⌥V to paste anyway".
- **Exception — master pause with unchanged focus (changed 2026-08-31, at the
  product owner's request):** when Master is the *only* active source and the
  focused app is still the one the utterance began in, the transcript **is
  pasted** rather than dropped. Muting means "stop listening", not "discard
  what I already said", and with focus unchanged there is no other window to
  leak into. The originating bundle id is captured before the first frame is
  recorded (`pause::set_dictation_origin_app`) and compared at paste time; an
  unknown origin fails closed to the drop. Every other source — per-app, idle,
  mic conflict, audio output, no-GUI — keeps the drop unchanged, because those
  genuinely mean "this window must not receive dictation".
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
- The conflicting app is logged and surfaced to the GUI by its **display
  name** (resolved via LaunchServices from the bundle id), not the raw bundle
  id — e.g. "superwhisper" instead of "com.superduper.superwhisper".

The following are **not** treated as conflicts even though they open an input
stream: always-on system listeners (Siri/CoreSpeech/assistantd), macOS
Settings extensions that use the mic only for level-metering
(`com.apple.Sound-Settings.extension`), and user-configured coexisting apps
(`mic_conflict_exclusion_bundles` preference, JSON array of bundle IDs).
The Sound extension can outlive the visible System Settings window and keep
the stream open; without the exclusion it would permanently pause Always.
Coexisting apps (default: ScreenFlow `net.telestream.screenflow10`, CleanShotX
`pl.maketheweb.cleanshotx`) hold the mic for the duration of a recording;
excluding them lets the user voice-type while recording. The list is
user-configurable via `always config set mic_conflict_exclusion_bundles
'["bundle.id"]'`.

### 7.2 Recorder health and respawn

The `rec` (SoX) process runs for the daemon's lifetime and is only replaced
when genuinely unhealthy. CoreAudio "buffer overrun" messages trigger a respawn
only as a **rate**: at least 64 overruns within one 60 s window. Overruns are
**not counted while capture is deliberately gated** (pause, mic conflict) —
during a gate nobody drains the pipe, SoX blocks, and CoreAudio discarding
callbacks is expected backpressure, not a device fault. (The previous
lifetime-cumulative counter condemned a healthy recorder after any single
benign backpressure episode — 291 respawns in 2 days, each paying a ~4.5 s
device cold start.)

When a respawn does happen, the old recorder is killed and reaped **before**
the replacement is spawned, so two `rec` processes never hold the input device
at the same time (I4).

The recorder is also respawned when the macOS **default input device
changes** while the daemon is running. The GUI installs a CoreAudio
property listener on the system object for
`kAudioHardwarePropertyDefaultInputDevice`; on change it sends a
`RespawnRecorder` UDS command, and the daemon kills and re-spawns `rec`
so it opens the newly selected mic. Without this, `rec` keeps capturing
from the device it opened at spawn time and the user would have to
relaunch Always to switch microphones. The respawn follows the same
I4 "drop old before spawn new" discipline as a health-driven respawn.

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

**Settings → Models** shows the catalogue's advertised `size_mb` (an estimate)
before download, and the model's *actual* on-disk byte count once downloaded —
summed across every file for a directory model like Nemotron. Read directly by
the Swift app from `~/Library/Application Support/always/models/` (the daemon
doesn't compute this itself); falls back to the advertised estimate if the
real bytes can't be read.

`url` and `files` are mutually exclusive; a test enforces this, along with real
checksums, real sizes, `is_directory`, filenames that cannot escape the model
directory, and a declared `size_mb` that agrees with the sum of the parts.

**Implementation details:**
- `LoadedEngine::Nemotron` holds a **shared, read-only `parakeet_rs::NemotronHandle`** (loaded via `NemotronHandle::load(path, None)`), never a bare `Nemotron` instance. Every call — the one-shot final path and each streaming session — spawns its own independent `Nemotron::from_shared(&handle)` with fresh decoder state, transcribes, and drops it. This is required, not cosmetic: `Nemotron::transcribe_audio` (the one-shot path) starts by resetting the same cache-aware decode state (`encoder_cache`, LSTM `state_1`/`state_2`, `last_token`) that a concurrent `transcribe_chunk` streaming sequence depends on, and the streaming path releases `self.engine`'s mutex between chunks (so a slow decode doesn't block unrelated daemon work) — sharing one `Nemotron` between the two paths let the daemon's independent "speculative transcription" (fires ~240ms after any pause, unrelated to streaming preview state) silently corrupt an in-progress streaming decode. Per-call isolation removes the shared mutable state entirely; no additional locking is needed.
- Non-streaming transcription uses `Nemotron::from_shared(&handle)` then `transcribe_audio(&samples)`
- `LocalTranscriber::open_live_stream` returns the **persistent** session used for both the live preview and the final transcript. It takes the handle from a `nemotron` field held *outside* `self.engine`'s mutex — `transcribe_from_bytes` holds that mutex for a whole decode, and the session is opened from the capture thread, which must never block behind an unrelated one-shot. The session lives on its own worker thread (`always::live_stream`), is fed exactly 8960-sample windows in capture order, and exposes only the **cumulative** transcript (`Nemotron::get_transcript()`), never per-call returns.
- Measured on the shipped int8 model (`examples/nemotron_stream_bench.rs`): per-window decode cost is **flat at ~54 ms** from a 0.5 s buffer to a 120 s buffer (0.10x realtime, ~10x headroom), and the flush costs 51-54 ms. The same clips cost 514 ms / 4200 ms / 11512 ms to decode one-shot at 4.7 s / 39.6 s / 119.7 s. Transcript equivalence versus one-shot: WER 0.000 at 4.7 s, 0.008 at 39.6 s.
- The session is opened at voice onset. On a cold daemon the engine may still be loading, and the open is a deliberately non-blocking `try_lock` that answers "no session" rather than stalling the capture thread; that answer is **retried every 300 ms** for as long as the active engine reports it can stream. Without the retry a single unlucky first frame disabled streaming for the whole utterance — no live transcript (so no early finalization) and the 6 s chunk target back in force, which is how `chunk_flush` still appeared on a streaming engine. Re-opening mid-utterance is complete, not partial: `feed` starts from sample 0 of the current buffer, and after a chunk flush that buffer is exactly the tail `finalize_chunked` appends.
- A **degraded** session is treated as an absent one and is replaced, up to three times per utterance. `degraded()` latches on a single worker decode error or a queue 16 windows behind, and a degraded session is present-but-dead: `feed` returns immediately, `transcript()` and `finish()` are `None`, and `live_session_carrying` goes false — which puts the 6 s chunk target back for the rest of the utterance. The replacement starts at `fed = 0` on a fresh generation and re-decodes the current buffer from its first sample. Past three attempts the utterance falls back to rolling chunking, which has its own retries and spill. `live_final_state` is logged once per utterance with the carrying reason, the re-open count and the chunk count, because `stt_wait_ms` scaling with utterance length (one-shot, ~0.10x realtime) and flat ~130 ms (live) are otherwise indistinguishable in the log.
- `supports_streaming() == true` does **not** imply `open_live_stream().is_some()` — the flag is a per-model registry constant while the session additionally requires the loaded engine to be Nemotron. That combination is warned once per utterance (`live_stream_unavailable_despite_supports_streaming`) rather than retried silently.
- Every chunk flush logs which of the three non-carrying states caused it (`no_session`, `invalidated`, `degraded`, or `carrying` for the 120 s safety valve), because `live_session_carrying` collapses three very different failures into one `false`.
- The session is re-based (`LiveStream::reset`) on every chunker flush, because the committed audio leaves the live buffer and `finalize_chunked` appends the tail to the separately-decoded chunks — a session still holding the committed words would duplicate them. It is invalidated outright on a speaker-gate truncation, because its decoded state then covers audio the rest of the pipeline has discarded.
- Because a flush destroys the session's accumulated state, the two are mutually exclusive by design: while a healthy session is carrying the utterance the chunk target rises from `CHUNK_TARGET_SECS` (6 s) / `CHUNK_HARD_MAX_SECS` (15 s) to `STREAM_CHUNK_TARGET_SECS` (120 s), so a normal dictation never chunks at all. 120 s rather than "never" because that is the longest span the flat per-window cost was actually measured over, and because past it the chunker still provides things the live path does not: per-chunk empty-result retries, per-chunk grammar for text beyond `GRAMMAR_MAX_CHARS`, the failed-chunk WAV spill, and a bound on the raw sample buffer (120 s ≈ 3.8 MB). parakeet-rs trims its own retained audio to ~1.8 s per session regardless of length, so a long session is memory-bounded on the engine side. The `ALWAYS_CHUNK_TARGET_SECS` test override still wins over the streaming ceiling.
- `LocalTranscriber::transcribe_streaming` (the older, non-persistent API) clones the handle out of `self.engine` once, then uses stateful `transcribe_chunk(&audio_chunk)` calls on its own `Nemotron` instance with 560ms chunks (8960 samples @ 16kHz). It decodes a COMPLETE clip from scratch each call, so it is a presentation helper only — it is no longer on the dictation path for Nemotron.
- Each preview snapshot spawns a fresh (already-reset) instance, pads its final chunk to 8960 samples, and sends three silent flush chunks
- VAD consume-mode previews call `transcribe_streaming`; preview events contain cumulative text because the Swift monitor replaces its stored partial transcript on each event
- The final paste path uses the live session when one is available, and falls back to non-streaming `transcribe_audio(&samples)` otherwise
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

### 8.2 Apple SpeechAnalyzer backend

macOS 26+ uses `SpeechAnalyzer` + `SpeechTranscriber` — Apple's newer on-device
speech engine, ~4x more accurate than the legacy `SFSpeechRecognizer` model.
On macOS < 26 the backend falls back to `SFSpeechRecognizer` with cloud
recognition. Requires no API key and never uploads audio on macOS 26+.

**Rules:**
- The `apple` backend is only available on macOS. On other platforms the daemon
  rejects the `apple` backend choice when building the transcriber.
- Audio is written to a temporary WAV file and analyzed with
  `SpeechAnalyzer(inputAudioFile:modules:analysisContext:finishAfterFile:)`
  running a `SpeechTranscriber` module. The daemon deletes the temp file
  after the call regardless of the outcome.
- Domain vocabulary (all canonical glossary terms plus a small default tech
  list, capped at 100 phrases) is passed as `AnalysisContext.contextualStrings`
  under the `.general` tag — the only tag the SDK defines. A compiled custom
  language model (`SFCustomLanguageModelData` + `DictationTranscriber`) was
  evaluated and rejected: it added 2-5 s of latency per utterance and was less
  accurate on technical vocabulary than plain `SpeechTranscriber`.
- Missing locale assets are installed via
  `AssetInventory.assetInstallationRequest(supporting:)` on first use.
- On macOS 26+ the backend is **incrementally streaming**: `supports_streaming()`
  reports true and `open_live_stream()` opens a `SpeechAnalyzer` session fed by
  an `AsyncStream<AnalyzerInput>` of 16 kHz mono Int16 buffers (~500 ms
  chunks; the recognizer worker only accepts Int16 — Float32 input traps it). `transcriber.results` is collected into an ordered segment table;
  `.volatileResults` emissions supersede overlapping spans, so every
  `push_chunk` returns the engine's latest cumulative transcript. `finish`
  closes input, runs `finalizeAndFinishThroughEndOfInput()`, and returns the
  joined segments — end-of-speech costs one flush, not a from-scratch decode.
  A session refuses to open when the locale's model is not yet installed or
  speech recognition is not authorized — the caller keeps its one-shot path,
  which installs the model on demand, and the next utterance streams.
  On macOS < 26 the backend stays one-shot (`supports_streaming()` false,
  `transcribe_streaming()` yields a single final result).
- The configured `lang` hint is resolved against
  `SpeechTranscriber.supportedLocales`. If `lang` is `auto` or empty, the
  current system locale is used.
- Recognition calls are serialized with a global mutex because concurrent
  recognition sessions have produced empty results for one of the callers.
- Before transcription, the WAV samples are normalized to ~90% of full scale
  with a 4x ceiling. Apple STT is less tolerant of quiet speech than Whisper.
- The async analyzer is bridged to the sync C FFI with a `Task` +
  `DispatchSemaphore` pair, bounded to a 12 second timeout so a wedged
  analyzer cannot stall the chunker finalize. The legacy recognizer path runs
  its result handler on a dedicated `OperationQueue` because the daemon's Rust
  main thread does not pump `NSRunLoop`.
- The re-decode live preview cadence is disabled for the Apple backend. On
  macOS 26+ interim text comes free from the incremental session's cumulative
  transcript; on older systems previews were non-streaming, competed for the
  serialized recognizer, and added latency without improving the final paste.
- Grammar correction on the Apple backend follows `postprocess_provider`:
  `groq` calls the Groq LLM with the glossary-aware prompt (~600 ms);
  `apple` calls Apple Intelligence through the same prompt (1.5-4 s, fully
  on-device). Both are eligible on the Apple backend — the provider pref
  decides, no forced disable. Independent of provider, the fast local
  cleanup always runs first: tier-1 glossary rewrites plus removal of
  adjacent repeated words and fillers.
- In the Settings → Models tab, the Apple on-device option appears at the top of
  the model list (above downloaded/available models and broken-model diagnostics).
  Selecting it sends `SetActiveTranscriber { "backend": "apple" }`, the active
  row is badged, and the same loading spinner is shown while the daemon confirms.
- `Always.app/Contents/Info.plist` declares `NSSpeechRecognitionUsageDescription`
  so the bundled `always-daemon` can request macOS speech-recognition permission.
  The first transcription attempt triggers a TCC prompt; without this key the
  process is killed by the system before the user can respond.

---

## 9. Text pipeline

Between transcription and the keyboard:

0. **Script normalisation (Devanagari → Roman Nepali).** The first thing that
   happens to a transcript, ahead of every filter and every judgment below.

   Nemotron supports 40 language-locales and **Nepali is not one of them**:
   `ne-NP` is prompt slot 46 and its embedding is untrained, returning empty
   for every input. So Nepali speech — and English/Nepali code-switching — is
   resolved onto the nearest locale the model does know, Hindi (`hi-IN`), and
   comes back as **Devanagari even when `lang = "en"`**. The language prompt
   biases decoding; it does not constrain the output alphabet, so configuring
   the language correctly is necessary but not sufficient.

   The user writes Nepali in Latin script and never wants Devanagari pasted.
   Every Devanagari run is therefore rewritten into **his own romanisation**
   (`छ → x`, `भ → v` — he writes `hunxa`, `xaina`, `vayo`, `maa`, not
   `hunchha`/`bhayo`). Everything else — English words, punctuation, spacing,
   emoji, code — passes through **byte-identical**, so a code-switched
   sentence comes out uniformly Latin rather than half-transliterated.

   **Every tier is the same learned model.** There are no hand-written
   transliteration rules; the tiers differ only in what the answer costs.

   0. Devanagari digits `०-९` are a 1:1 map and never reach the model — the
      same carve-out the training pipeline made, because a seq2seq guessing
      them turned `००७` into `dah`.
   1. An exact table of 49,064 entries, built offline by running the trained
      transliteration model (99.0% exact on held-out pairs of his own
      spellings) over the SLR54 Nepali corpus and word-aligning the result,
      with his 1,844 hand-mined pairs overriding it. `include_str!`-ed and
      binary-searched in place: no init cost, no allocation for Latin-only
      text.
   2. **The model itself**, as ONNX (`translit_encoder.onnx` +
      `translit_decoder.onnx`, 18.4 MB, lazily loaded on the first cache
      miss), for words the table lacks. Every miss in one utterance is
      submitted as a single batch.
   3. A per-process memo of everything tier 2 has already answered, so a
      novel word costs the model once per process and a hash lookup after.
   4. `char_roman.tsv` — the model's own answer for each Devanagari codepoint
      taken alone — reachable only when ORT cannot build a session at all.

   Tier 2 replaced a hand-written syllable transliterator that scored **39.0%
   exact** (19,148/49,064) against tier 1 used as gold, at 607 ns/word. The
   model reproduces tier 1 on **97.7%** of a 2,000-word sample and is
   bit-identical to the PyTorch checkpoint it was exported from.

   Measured cost, release build, against a ~870 ms decode:

   | case | time |
   |---|---|
   | English utterance (no Devanagari) | **28.9 ns** |
   | Devanagari, every word in the table | **4.1 µs** |
   | Devanagari, one novel word, memoized | **4.2 µs** |
   | Devanagari, one novel word, cold | **8.2 ms** |
   | the reported code-switched utterance, cold | **19.6 ms** |
   | four novel words in one utterance, cold | **42.3 ms** |

   The cold numbers are ORT per-kernel dispatch over a 317-node decoder graph
   run once per output character, not arithmetic: ORT thread count, graph
   fusion and prepacking all fail to move them. This is **over the
   single-digit-millisecond budget** for utterances containing words the table
   has never seen, which is ~31% of Devanagari utterances on a held-out split
   (mean 0.37 novel words per utterance). Peak RSS for a session that reaches
   tier 2 is **+71 MB**; a session that never leaves tier 1 pays **+0.5 MB**
   and an English-only session pays **0**.

   **Invariant: no codepoint in U+0900..U+097F ever reaches the clipboard.**
   This is now structural: the model's entire target vocabulary is the 26
   lowercase ASCII letters, so no decode can emit one. Unmapped Devanagari is
   dropped rather than passed through. The pass is idempotent, and it runs
   *before* snippet expansion, so user-authored snippet text is never
   transliterated.

   Ordering note: this deliberately precedes the hallucination filter, which
   rejects mixed Latin/Devanagari as "mixed-script gibberish" — precisely what
   a genuine code-switched sentence looks like. Filtering first would discard
   the utterances this step exists to rescue. One gap remains: the
   hallucination detector reads the raw transcription object rather than the
   romanised string, so on the **remote Groq backend** (the only backend where
   content filtering runs at all) a code-switched utterance is still dropped.
   Local backends are unaffected. Escape hatch: `ALWAYS_NO_ROMANIZE=1`.

   **English recovery.** Romanisation is faithful, and that is the problem for
   the English *inside* a Nepali utterance. Nemotron picks one language per
   utterance; when the utterance is mostly Nepali it picks Hindi, and the
   English words in it come back spelled phonetically in Devanagari. The
   romaniser then transliterates them rather than recognising them:

   | raw | romanised | said |
   |---|---|---|
   | `स्पीकिंग` | `spiking` | speaking |
   | `इंग्लिस` | `inglis` | English |
   | `बिटवीन` | `bitwin` | between |
   | `अगेन` | `agena` | again |
   | `एम` | `em` | am |

   Nothing is lost — `स्पीकिंग` *is* "speaking" — so a recovery pass runs on
   each romanised word, and only on words that came out of a Devanagari run.
   English dictation never reaches it and keeps its borrowed, zero-allocation
   path.

   Devanagari destroys English vowels (it cannot write the lax/tense contrast,
   so *and*/*end* and *speaking*/*spiking* collapse) but preserves consonants
   including voicing, so the match key is a **consonant skeleton**:
   `spiking → SPKNG ← speaking`. A skeleton alone is far too permissive, so a
   rewrite must also survive, in order:

   1. **The vocabulary veto** — anything he has ever typed himself (9,730
      tokens mined from 18,452 of his WhatsApp messages) is left alone. This
      covers all of his Nepali *and* his English that transcribed correctly.
   2. **Nepali orthographic shape** — `x` (his `छ`), the aspirate digraphs
      `dh`/`bh`/`jh`/`chh`, an `h` after `k g l r n m d b j v y`, and
      consonant+`y` clusters. Devanagari renderings of English words do not
      contain these.
   3. **Vowel correspondence** — each vowel must be a collapse Devanagari
      actually forces (`i`←`ea`, `i`←`ee`, `e`←`a`) and not one it does not
      (`o`←`e`, or a long `aa` standing for English /ʌ/). This alone is what
      separates `owar → over` from `owar → every`, and what stops his `बात`
      (`baata`) becoming `but`.
   4. **Evidence proportional to ambiguity** — a one-consonant skeleton may
      only rewrite a two-letter token one edit away; two consonants need four
      letters and two edits; longer skeletons carry themselves.

   Targets come only from his own vocabulary (`en_recover.tsv`, 2,121
   skeletons / 2,684 words, built by
   `training/nepali-nemotron/build_en_recover.py`), so a recovery can never
   produce a word he does not use. Ties break on fewest edits, then on how
   often he writes the word. **A wrong rewrite is worse than none** — he can
   read `spiking`; `spike` would mislead — so every filter exists to make "do
   nothing" the default answer.

   Measured, release build:

   | case | result |
   |---|---|
   | the reported utterance, English recovered | **5/5** (`speaking`, `english`, `between`, `again`, `am`) |
   | the reported utterance, Nepali damaged | **0** (`dekhi`, `nepali`, `boli`, `ki`, `khabar`, `rat`, `ita`, `aai` all survive) |
   | his own 9,730-token vocabulary rewritten | **0 (0.00%)**, asserted as a test |
   | unseen SLR54 romanisations rewritten | 662/45,923 (**1.44%**), mostly correct loanword recoveries |
   | English utterance (no Devanagari) | **30.5 ns**, unchanged — the pass is not reached |
   | added to a 6-word Devanagari sentence | **+4.7 µs** (8.8 → 13.5 µs) |
   | added to a 20-word code-switched sentence | **+9.3 µs** (15.0 → 24.3 µs) |

   The pass is idempotent and emits lowercase, matching how he writes. Escape
   hatch: `ALWAYS_NO_ENGLISH_RECOVERY=1`.

1. **Hallucination filter** — rejects the known failure modes of speech models:
   empty output, "thank you"/"bye" family, subtitle credits, repeated tokens,
   gibberish, low-confidence segments.
2. **Vocabulary bias** — the user's glossary is sent to the model as a hint so
   domain terms transcribe correctly. **The hint must never reach the user's
   text**: these models echo their prompt back, especially on short or quiet
   audio, and the echo is indistinguishable from speech to everything downstream.
3. **Glossary corrections** — known mistranscriptions mapped to canonical terms.
4. **Snippets / text expansion.** ❓ Not verified in detail.
5. **Local cleanup** — a deterministic pass that runs on every utterance at
   paste time and around the LLM call: removes adjacent repeated words and
   conservative fillers ("um", "uh", "ah", "eh") without inventing content.
   It is the sole cleanup when the LLM pass is disabled, timed out, or
   failed, and a post-pass on the LLM's own output.
6. **Grammar cleanup** — an LLM pass, on by default (`postprocess_enabled`).
   Its prompt enforces token preservation (never expand one word into a
   phrase or contract a phrase into one word) and a certainty bar for
   glossary substitutions. A bounded domain-vocabulary section lists the
   user's canonical terms so the model can resolve phonetic mishearings
   (`kit rug trees` → `git worktrees`) only when strong phonetic similarity
   AND technical context both hold; a meta-speech rule keeps quoted wrong
   words literal (`puts cloud instead of Claude` keeps `cloud`).
   The blocking call at paste time is pre-warmed in the background so it
   usually lands as a cache hit (or joins the identical in-flight request):
   - **Un-chunked utterance:** the tentative-silence speculation warms the
     grammar key for its transcript as soon as speculative STT returns.
   - **Chunked utterance:** the paste-time call is keyed on the *join* of the
     corrected chunks (+ tail), a key the per-chunk corrections never touch.
     Each chunk that finishes its per-chunk correction warms the joined
     transcript once every committed chunk is settled; a voiced tail's
     speculation warms join + tail. Warm and paste build the request through
     the same builder (`correction_request::build`), which is what keeps the
     cache keys byte-identical.
7. **Corrections** — the user can log a correction for a wrong transcription;
   passive capture of clipboard edits is available but off by default.

---

## 9b. Vocabulary and correction system

The vocabulary-import, correction-learning, and transcript-replacement system
combines Whisper biasing, deterministic learned corrections, context-aware
decisions, optional LLM assistance, and safe no-LLM operation.

### Data model

Glossary entries in `~/.always/glossary.json`:

```json
{
  "term": "Kubernetes",
  "mistranscriptions": ["kubernetics", "cuber netties"],
  "frequency": 200,
  "weight": 1,
  "provenance": "hotkey"
}
```

- `term` — canonical desired spelling.
- `mistranscriptions` — observed incorrect forms to be replaced.
- `frequency` — usage count, incremented when the term is successfully applied.
  Bumps are debounced (30s flush interval) to avoid disk churn.
- `weight` — user-curated priority (default 1).
- `provenance` — where the correction came from: `manual`, `hotkey`, `passive`,
  `llm`, `import`. Optional for backward compatibility — existing entries
  without this field are left unchanged.

### Hot reload

The glossary is loaded with modification-time-gated reloads. Writing to
`glossary.json` (via `apply_pairs_to_glossary`, `bump_frequency`, or external
edit) invalidates the in-memory cache, and the next access reloads from disk.
No daemon restart is needed for new corrections to take effect.

### Auto-learning toggle

The `auto_learn_corrections` preference (default `true`) controls whether
corrections captured via the ⌃⌥X hotkey and the passive clipboard watcher are
automatically written to the glossary. When `false`, pairs are still extracted
and surfaced (for the pending-review queue and event broadcast) but NOT
applied — the user curates the glossary manually.

Explicit user actions (approving from the review queue, typing in the
correction dialog) always apply regardless of this toggle.

### Frequency feedback loop

When a glossary substitution is successfully applied to a transcript, the
term's `frequency` is bumped. Bumps are accumulated in memory and flushed to
disk at most every 30 seconds to avoid write churn. Frequency influences
ordering in the Whisper bias prompt.

### Ambiguous vs unambiguous classification

Mistranscriptions that are also common English words (e.g. "cloud" as a
mistranscription of "Claude") are classified as **ambiguous** via an embedded
common-word list (`common_words.rs`). Ambiguous exact hits are NOT rewritten
unconditionally — they become `ExactAmbiguous` substitutions and defer to the
LLM for context arbitration.

Unambiguous learned forms (e.g. "kubernetics" → "Kubernetes") are deterministic
and apply immediately, even with an LLM available.

### Confidence-tiered fuzzy matching

Fuzzy matches (Soundex/Levenshtein) use three confidence bands:

- **High confidence** (score < 0.10): applied automatically, even without an
  LLM, unless the heard word is ambiguous.
- **Marginal confidence** (score 0.10–0.18): deferred to the LLM when
  available; skipped when no LLM is available.
- **Ambiguous/low confidence**: the heard word is a common English word —
  always deferred to the LLM. Without an LLM, context heuristics (Phase 4)
  may permit application.

### LLM-assisted diff extraction

After the deterministic `diff_words` extraction, a background LLM "second
opinion" runs asynchronously. It receives `(original, corrected)` and extracts
genuine STT mishearings — not style edits or grammar fixes. New pairs are
deduplicated against the deterministic output and written to the glossary with
`llm` provenance.

The LLM path is non-blocking: the user gets immediate feedback from
`diff_words`, and may get additional pairs from the LLM a few seconds later.
If no API key is configured, extraction is skipped silently.

### Dedicated correction model

The `correction_model` preference allows a separate Groq model for LLM-assisted
correction extraction (e.g. a cheaper/faster model). When unset, falls back to
the post-processing `groq_model`.

### Back-correction / negative learning

When a user correction reverses a previously learned mapping (e.g. the glossary
has `cloud → Claude` and the user corrects "Claude" back to "cloud"), the bad
mistranscription is removed from the glossary. The reverse mapping is NOT
learned — this prevents false-positive rules from accumulating.

### No-LLM context heuristics

When no LLM is configured, ambiguous-term resolution uses lightweight context
heuristics (`context_heuristics.rs`):

- **Product-context verbs** ("ask", "tell", "prompt", "call") preceding the
  ambiguous word strongly suggest the product/person meaning → apply.
- **Ordinary-context prepositions** ("to the", "in the", "on the") or
  **ordinary-context following nouns** ("storage", "computing") suggest the
  ordinary meaning → skip.
- **No strong cue** → fail safe, leave the text unchanged.

Example: "ask cloud to fix it" → "ask Claude to fix it" (product context).
Example: "deploy to the cloud" → "deploy to the cloud" (ordinary context).

### Correction effectiveness metrics

Atomic counters track correction pipeline decisions: exact applied/deferred,
fuzzy applied/deferred/rejected, back-corrections, pairs written, LLM
extraction found/empty/failed. Metrics are logged via `correction_metrics` and
do not record transcript content — only aggregate counts and glossary term
pairs.

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
| `stt_live_preview` | true | Live provisional transcript in the overlay while still talking (§5) |
| `stt_auto_enter` | true | Press Return after pasting |
| `auto_enter_delay_ms` | 0 (this instance: 800) | Grace period before Return |
| `speaker_gate_enabled` | true | My Voice |
| `speaker_gate_threshold` | 0.40 | Identity cut-off |
| `idle_pause_secs` | 0 (off) | Idle auto-pause |
| `postprocess_enabled` | true | Grammar cleanup |
| `transcript_stream` | true | Append accepted utterances to `~/.always/transcripts.jsonl` |
| `audible_status_sound` | off | Sound cues |
| `per_app_settings_json` | — | Per-application pause rules |

Shortcuts: pause `ctrl+alt+p` · master pause/mute `ctrl+alt+shift+p` ·
auto-enter `ctrl+alt+a` · force paste `ctrl+alt+v` · log correction
`ctrl+alt+x` · correction dialog `ctrl+alt+w`.

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

Two identities coexist on the development machine; each is one GUI + one
daemon (I4/I5 hold *per instance*).

| | Production | Development |
|---|---|---|
| App | `/Applications/Always.app` | `/Applications/Always Dev.app` |
| Bundle id | `com.always.v3` | `com.always.v3.dev` |
| State dir | `~/Library/Application Support/always` | `~/Library/Application Support/always-dev` |
| Socket | `~/Library/Caches/Always/always.sock` | `~/Library/Caches/Always/always-dev.sock` |
| URL scheme | `always://` | `always-dev://` |
| Sparkle | on | off (keys stripped at build; updater disabled in code) |
| Focus-state file | `~/Library/Caches/Always/focus-state.json` | `focus-state-dev.json` |
| Daemon logs | `~/Library/Logs/Always` | `~/Library/Logs/Always-dev` |

Shared deliberately: `~/Library/Application Support/always/models` and the
`~/Library/Caches/always` model caches — immutable downloads, reused by both.

**Instance detection.** The daemon resolves its instance once at startup:
`ALWAYS_INSTANCE=dev` env var, else `Always Dev.app` in its own executable
path, else prod. The GUI detects via its bundle id/path. No env plumbing is
needed for the bundled daemon — it inherits the right identity from its
location.

**Isolation.** Each instance's single-instance lock, pid file, process
sweep, and orphan-reconciliation only match processes from its own bundle
path, so neither instance can signal or kill the other. Dev's first launch
seeds its state dir from production's db/voiceprint/vocabulary (one-time
copy; afterwards they diverge).

**Peer handoff.** The microphone is still singular — two live daemons would
both transcribe and both paste. So whichever daemon starts last pauses its
peer over the peer's UDS socket (`SetPaused{reason:"peer-instance"}`): the
paused instance stops listening, no audio is transcribed or pasted. A
paused-by-peer daemon runs a watchdog that self-resumes the moment the
peer's socket dies (~1 s), covering SIGKILL where no cleanup runs;
graceful exits resume the peer directly. A user pause/unpause of either
instance always overrides the handoff — the watchdog stands down and the
user's choice wins.

**Commands.**

- `scripts/dev-rebuild.sh` — the dev loop. Kills/rebuilds/relaunches only
  `Always Dev.app`; never touches the production install. Exits non-zero
  unless the dev GUI + daemon are running the just-built binaries, and
  warns if production went missing.
- `scripts/promote.sh` — manual production promotion, user-invoked only.
  Kills production, rebuilds, deploys over `/Applications/Always.app`,
  relaunches, verifies. Agents must not run it.

**Nothing is "done" until the new build is running** (I10). "Built" and
"deployed" are different claims, and only "running" is testable.

> ⚠️ Live-mic testing of the dev instance still contends for the single
> hardware microphone — the mic-conflict monitor (§7) will pause one side.
> Test the dev daemon with file/STT paths while production holds the mic,
> or pause production manually first.

---

## 13. Open questions for the product owner

1. ~~Trust window at 5 minutes — how much badge-flashing is acceptable in
   exchange for an instant indicator?~~ **Resolved 2026-08-30:** reverted
   entirely. The optimistic badge flashed on every non-user voice (videos,
   meetings, sleep-time media) and blocked dictation while media played. Every
   sentence now re-verifies; the ~2 s verify wait is accepted as the cost of the
   gate.
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
