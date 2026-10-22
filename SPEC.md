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
| `rec` (SoX) | Raw microphone capture, 16 kHz mono 16-bit, one instance, spawned by the daemon. Its output is drained continuously by one daemon reader thread (see §4, "Audio delivery"). |

On Linux and Windows, the daemon uses CPAL for microphone capture. It converts
the selected device's supported sample rate and channel layout to the pipeline's
16 kHz mono frames; Linux exposes the capture device through ALSA, including
PipeWire/PulseAudio ALSA compatibility layers. Linux uses `wl-copy`/`wl-paste`
for clipboard access on Wayland when installed, falling back to `xclip`, and
uses `xclip` on X11. On Wayland it sends the paste chord with `wtype` (the
virtual-keyboard protocol of wlroots compositors such as Hyprland and Sway; no
daemon or `/dev/uinput` access needed), then `ydotool` (needs `ydotoold`), then
`xdotool` (XWayland windows only); an injector that exits non-zero hands over to
the next. `ydotool` is driven with raw evdev keycodes
(`ydotool key -d 3 29:1 47:1 47:0 29:0`): ydotool 1.0 rejects key names and
exits 0 while doing so, which until 2026-10-20 meant nothing was pasted on
Wayland. Terminals receive Ctrl+Shift+V instead of Ctrl+V — on X11 from the
active window's class, on Hyprland from `hyprctl activewindow` (class or process
name); other Wayland compositors offer no portable query and receive Ctrl+V.
X11 uses `xdotool`. Linux global
shortcuts are available in X11 sessions; Wayland compositors do not let the
daemon observe global key events, so users there control pause and auto-enter
through the CLI. The GUI shows this instead of pretending the keys work: on
Linux with the shortcut listener unavailable (`ShortcutListenerStatus` false =
Wayland), onboarding's tips list only the bindable actions — pause everywhere
(`always toggle-pause`) and auto-enter (`always toggle-auto-enter`) — with the
user's configured keys as suggestions for the desktop's own bindings, and
Settings → Shortcuts says keys there are recorded, not active. Everywhere,
both read the configured shortcuts (never a fixed list), and the per-app pause
(focused-app monitor, macOS only) is not offered on Linux.

The GUI talks to the daemon over a Unix domain socket using newline-delimited
JSON. On macOS it is `~/Library/Caches/Always/always{suffix}.sock`; on Linux it
is `$XDG_RUNTIME_DIR/always{suffix}.sock` (or `/tmp/always{suffix}.sock` when
`XDG_RUNTIME_DIR` is unset). Development instances use the `-dev` suffix so
they cannot replace the production socket. The daemon broadcasts events; the
GUI sends commands.

**Always never changes what the Mac plays (2026-10-16, v0.1.0 user report:
micro-cuts / crackling in Apple Music and on a speaker while Always ran).**
The voice-processing (VPIO) echo canceller is OFF by default: the daemon
starts `always-capture --no-vp` (plain mic) unless `capture_aec` is set true
(or `ALWAYS_AEC=1`), and the helper itself runs VPIO only with `--vp` /
`ALWAYS_CAPTURE_AEC=vp`. Measured on a MacBook Air (macOS 27, built-in
speakers): starting a VPIO unit makes coreaudiod restart the OUTPUT device's
IO (and again when it stops) with Audio IO Overloads at both edges, and duck
every other app to 0.178 (−15 dB) for ~2 s, then 0.631 (−4 dB) while it runs
— at `duckingLevel = .min`. The plain helper caused no output restart and no
ducking. **Trade-off (owner decision, 2026-10-16):** with VPIO off, sound
playing from the Mac's own speakers reaches the microphone again (measured
below: playback put 100 % of frames above the speech floor, vs 0 % through
VPIO), so dictating over a playing video now relies on the speaker gate and
the audio-output pause instead of cancellation, and status cues are no
longer cancelled out of the mic. Users who want the 2026-10-14 behaviour opt
in with `always config set capture_aec true`. The paragraphs below describe
the VPIO path, now opt-in.
**Echo-cancelled capture (2026-10-14, user request: "When a YouTube video is
playing she does not hear me at all — I should never have to pause a video to
dictate").** When the bundled `always-capture` helper is present, it is the
recorder instead of `rec`: same contract (16 kHz mono s16le on stdout, one
process, spawned by the daemon), but captured through Apple's voice-processing
I/O, which subtracts the Mac's own audio output from the microphone before the
daemon's VAD, speaker gate and recognizers see it. Other apps' audio is not
ducked (`duckingLevel = .min`) and VPIO's AGC is off, so energy thresholds stay
calibrated to the raw mic. **Opt-in since 2026-10-16** (it was default ON
from 2026-10-14, when the user confirmed dictation over a playing video works
with it; see above for why it changed). Enable: `always config set
capture_aec true` (applied at the next daemon start); env `ALWAYS_AEC=0/1`
overrides.
**Echo canceller only while something plays (2026-10-14, user request:
"optimise the fuck out of it… voice is badly lagging").** The canceller only
has work when the Mac is producing sound, so while no other process runs audio
output the helper sets `isVoiceProcessingBypassed = true` and re-engages it the
instant any process starts output (CoreAudio process objects:
`kAudioHardwarePropertyProcessObjectList` + per-process
`kAudioProcessPropertyIsRunningOutput` listeners — event-driven, no polling;
macOS ≥ 14.2, otherwise always engaged). It stays engaged 2 s after the last
output stops (track gaps, room tail) and for each status cue's duration
+ 0.5 s (a cue rendered while bypassed would leak into the mic).
`corespeechd` is ignored: it reports running output around the clock without
playing anything. The stream is continuous across transitions (measured: 30 s
idle = 960 164 bytes = 16 kHz s16 mono, no gap). Measured on the user's
MacBook Air, 60 s windows, same room: VPIO engaged 15.1 CPU-s/min, bypassed
3.0 CPU-s/min (gated helper idle: 2.8 CPU-s/min), plain capture 0.5. `--aec-always`
restores the previous always-engaged behaviour. The parent-death watchdog is a
kqueue process-exit source instead of a 1 Hz poll; stderr lines use `fputs`
(an `NSFileHandle` write raises and aborts on EPIPE).
**Ducking.** A voice-processing unit ducks *other* audio on the Mac. The
helper requests the minimum (`enableAdvancedDucking: false`,
`duckingLevel: .min`), re-applied once the engine runs and read back in its
log (`ducking advanced=false level=10`). Even so, status cues played from
the daemon process were inaudible in production while the helper ran, so
**while the helper runs, status cues are rendered through the helper's own
voice-processing output** (`play <file>` on its stdin) — the unit never
ducks its own output, and it is the echo reference, so cues never reach the
mic. (In plain mode — the default since 2026-10-16 — the helper carries no
output chain at all: attaching the cue players would keep the OUTPUT device
running for the helper's whole lifetime and restarting output IO at launch,
an audible cut in whatever the user is playing — reported 2026-10-18 as a
"small jump" in output sound on every app start. Cues therefore play
in-process; nothing ducks them once VPIO is off.) With plain `rec` (or if the helper's cue channel fails) cues play
in-process as before. ❓ Residual ducking of other apps' audio at `.min`
could not be measured here: while any voice-processing unit runs, even
another process's raw `rec` capture of the speakers is strongly attenuated
(measured: an `afplay` cue at volume 45 raised the raw mic from 0.0009 to
0.0020 RMS), so the speaker→mic path cannot separate ducking from
cancellation. If the helper is missing,
exits, or delivers no audio within 4 s of spawn, it is killed and reaped and
`rec` is spawned instead (I4 holds); the daemon then stays on `rec` until the
default input device changes. Measured on a MacBook Air (built-in mic +
speakers, music + TTS speech played at `afplay -v 0.3`, system volume 100):
playback raised the raw-mic RMS from 0.0375 to 0.1354 (100 % of 30 ms frames
above the 0.012 speech-energy floor); through the helper it was 0.0012 (0 % of
frames above the floor). ❓ How much the user's own voice is attenuated by
VPIO's noise suppression was not measurable on a single-speaker machine.

**Socket rules:**
- A client falling behind must be told it missed events, never disconnected.
  Dropping the connection makes the GUI reconnect and re-read the entire initial
  state, during which the user sees nothing.
- The GUI must reconnect on its own, with backoff, and respawn the daemon if it
  is genuinely gone.
- The daemon sends its full current state on connect, so a reconnecting GUI is
  never out of sync.
- `MicUnavailable { unavailable }` (2026-10-17) is additive — no protocol
  bump. It is broadcast on change and included in the initial burst while the
  mic is stuck (§7.1, §7.2).

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
   in the background, which also pre-warms the grammar LLM for the expected
   final text (§9). If speech resumes, it is discarded.
6. **End of utterance** — silence exceeds the configured window
   (`stt_silence_secs`; the code default is 0.9 s). With adaptive silence
   on, the window extends when the text looks mid-sentence. The window
   applies to the **whole utterance**: committing a chunk of a long
   dictation (step 4) must never shorten it. Only an utterance that is
   itself under 400 ms in total gets the short 200 ms cut.
7. **Transcribe** — the speculative result is used if still valid, otherwise a
   fresh transcription runs.
8. **Post-process** — filters, glossary corrections, grammar cleanup (§9).
   A rule-filtered utterance (not a hallucination) is still copied to the
   clipboard — no paste, no auto-Enter — so a wrong filter verdict costs a
   manual ⌘V instead of the whole utterance.
9. **Paste** — text is typed into the focused application. If auto-Enter is on,
   Return follows after `auto_enter_delay_ms`. With a 0 ms delay, Return is
   posted 50 ms after Cmd+V (the Cmd-flag guard in `auto_enter_countdown`),
   or 200 ms after live typing's last keystroke: terminal UIs treat a Return
   right behind fast typing as part of a paste and insert a newline (Codex
   does for 120 ms), so 50 ms there made auto-Enter add a line instead of
   sending the prompt (user report 2026-10-22, Codex and Claude Code);
   the 100 ms post-paste settle that holds the paste-in-flight lock runs
   *after* Return, not before it (2026-10-13, user request: "make it insanely
   fast" — previously Return landed ~155 ms after the paste). The clipboard is
   written in-process through `NSPasteboard` (p50 0.84 ms) rather than a
   `pbcopy` subprocess (p50 17.4 ms), which remains the fallback. On macOS,
   simulated Cmd+V brackets the V key events with Command `FlagsChanged` events;
   optional Return is sent after Command is released.

**Long utterances (chunks).** A chunk committed by the hard flush
(`CHUNK_HARD_MAX_SECS`) ends at our cut, not at a pause the speaker made, so a
trailing "..." / "…" on any chunk followed by more text is dropped at join time
(`chunker::strip_cut_ellipsis`). For a chunked utterance the live streaming
session's final is never used as the tail — it overlaps already-committed
chunks and duplicated text (observed 2026-10-13); the tail is transcribed from
its own samples instead.

**A failed tail never loses the chunks (fixed 2026-10-15, user request: "his
words are lost").** When the recognizer fails on the tail of a chunked
utterance (2026-10-14T17:48:45Z: Apple STT timed out on a ~1 s tail and the
three committed chunks, 480 characters already transcribed, were discarded
with it), the committed chunks are still assembled and pasted. The tail's
audio is saved to `~/.always/failed-chunks/` (`chunk-<ns>-<N>-tail.wav`,
owner-only), and the tail's text is the Parakeet preview helper's
transcription of that same span when it heard words (Apple backend, English,
helper warm), otherwise the `[audio saved: chunk N]` placeholder a failed
chunk gets. The Parakeet pass starts only once the recognizer's tail pass has
run `TAIL_RESCUE_ARM_MS` (4 s; fresh passes 2026-10-13/14: n=271, p99 2.8 s,
max 3.5 s) or failed, so the normal path never shares the Neural Engine with
it; after a failure the paste waits at most `TAIL_RESCUE_MAX_EXTRA_MS`
(1.5 s) more for it. Measured on replay (Apple's 12 s timeout simulated):
the Parakeet text of a 4.9 s tail was ready 588 ms after the rescue started,
so the paste landed when the timeout fired — no added wait. The log line
`chunked_tail_stt_failed` marks the recovery (`recovered_by`, saved path;
no transcript text).

**Chunked dictation keeps its pauses (fixed 2026-10-14).** The 200 ms
short-utterance cut applies only while the *whole* utterance (committed
chunks + live buffer) is under `SHORT_SPEECH_MS`. It used to judge the live
buffer alone, which is nearly empty right after a chunk flush, so any
dictation past `CHUNK_TARGET_SECS` ended at its next pause and was pasted
in pieces (replay: three sentences with ~1 s pauses became three pastes;
after the fix, one).

**Consume mode hand-back.** While a consumer holds a consume lease
(`SetConsumeMode`), a final is not pasted. Its native paste job is parked
(`held.rs`) and announced as `TranscriptHeld { id, text, at_us }` immediately
*before* the usual `TranscriptFinal`. The consumer answers:
- `ReleaseHeld { id }` — it was dictation: the parked job runs the same paste
  path an unheld utterance takes (merge, dedupe, clipboard, Cmd+V,
  auto-Enter). The consumer must not type it itself.
- `DiscardHeld { id }` — claimed (a command / ambient speech): nothing is
  inserted.
An entry nobody answers is not pasted after 30 s (`HELD_TTL`) — a late
paste into whatever has focus by then is worse than none — but it is never
silently lost either: it moves to the recoverable ring (`rejected.rs`) and
is announced as `TranscriptRejected { score: "-1.000" }`, pasteable with
`PasteRejected` (2026-10-14: three held finals had vanished unanswered).
A held final is an internal handoff and is SILENT: no cue at its
`TranscriptFinal`, nor on `ReleaseHeld` / `DiscardHeld` (user request
2026-10-14).
`SetActStatus` (Iris action progress) is accepted and ignored; Iris renders
action progress in its own notch. `text` may address an entry
when `id` is missing.

**Audio delivery must be real time.** Every timing above is counted in
30 ms frames, so it is only as prompt as the frames' arrival. SoX's
`--buffer` is also the full-buffering size of its stdout, so it sets how
much audio SoX holds before the daemon sees any of it: it is 4096 bytes
(128 ms). It was 131 072 bytes (4.096 s) until 2026-09-25, which delivered
audio in 4.1 s bursts: the badge, the end-of-utterance cut and the paste
were each up to 4 s late, and none of it showed in the logs because they
stamped processing time. A dedicated reader thread drains `rec` into a
bounded queue (30 s, oldest dropped first), so SoX never blocks while the
event loop is busy transcribing, pasting or gated. Each frame carries its
estimated capture time; latency metrics are measured from it (§11).

**Pause tolerance is the difference between dictating and being interrupted.**
Two settings govern it, and the shipped defaults were both too aggressive for
real speech: a 1.4 s silence window ended utterances during ordinary thinking
pauses, and auto-Enter with no grace period then sent the half-finished message.
This instance runs 0.9 s and a 0 ms auto-Enter delay (read from
`always-daemon config show` on 2026-09-25; it was lowered from 2.2 s to
1.1 s on 2026-08-26 after log analysis showed real pauses p95 ≈ 1.25 s and
the adaptive mid-sentence extension covering the tail). The code defaults
are 0.9 s (`DEFAULT_SILENCE_SECS`) and 4000 ms (`DEFAULT_AUTO_ENTER_DELAY_MS`).

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
- **Minimum 600 ms on screen** (I2), counted from when the HUD appears.
  State changes on a HUD that is already visible (listening → text →
  transcribing, each preview update) do not restart the count.
- **Leaves as soon as the text is final.** On the final transcript the HUD
  fades out over 0.15 s (once the 600 ms minimum is met). "Transcribing"
  stays up from the speculative kickoff until the final text is ready to
  paste or the utterance is dropped — speech-to-text finishing is not the
  end, grammar follows — and it returns to "Listening" only if the user
  resumes speaking.
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
- **Display mode applies live.** Settings → Status Overlay offers Normal,
  Compact and Hidden; the default is Normal. A change takes effect at once, no
  restart: the indicator already on screen is re-rendered in the new size, and
  choosing Hidden removes it through the normal hide path, so invariant I2's
  600 ms minimum on-screen time still holds. The indicator is still hidden
  when paused or disconnected. It sits centred near the bottom of the screen
  under the pointer, just above the Dock.
- **The idle pill** (shown when idle auto-pause fires) hides on any resume,
  whichever control caused it. Its Play button sends `Resume`, which lifts the
  pause and never pauses (§7).
- **Live feedback preempts transient confirmations.** Brief confirmation
  flashes (Pause/Resume, Auto-Enter toggles, low-mic volume, offline
  fallback — 1.5 s to 4 s) normally run their full duration, but if the user
  starts speaking (Listening) or a transcription state arrives while one is
  showing, the flash ends immediately and the live state shows at once.
  Lower-priority states still wait for the flash to finish, so a
  confirmation is never cut short by e.g. a stale state re-emission.
  When a flash ends on its own while the user is still speaking or a
  transcription is running, the live badge returns immediately.

**When "Listening" appears:**

| Situation | Timing |
|---|---|
| My Voice off | On voice onset — 60 ms of speech, plus at most the 128 ms audio-delivery granularity. |
| My Voice on | After the speaker is verified — the first check fires at ~0.3 s of voiced audio (the embedder's minimum window, padded by the 200 ms pre-buffer), rechecked every 0.25 s of new voice. Measured 2026-09-25 on the owner's voice (built-in mic, 10 utterances): median ≈ 1.0 s after onset, 0.4–2.4 s — the spread is how many 0.25 s re-checks it takes to clear the threshold. Background media and other voices do NOT flash the badge; the indicator appears only for the enrolled user. |

Both rows are speech time: `listening_overlay_shown.latency_ms` is measured
from the capture time of the first speech frame. (Before 2026-09-25 it was
measured from when that frame was *processed*, which hid up to 4 s of
audio-delivery delay — see §4.)

The verify wait under My Voice is the price of the gate doing its job: the
badge must not register audio the user did not produce. An earlier "optimistic
badge on onset" experiment flashed the overlay on every non-user voice (videos,
meetings, sleep-time media) and was reverted at the owner's request — see §6.
The latency budget is attacked at the check schedule (earlier first attempt,
tighter retry cadence), never by weakening the threshold — a noisy short window
that cannot clear the full bar simply falls through to the next check.

**Unverified preview, shown dimmed (product-owner decision 2026-10-14).**
Under My Voice the *badge* still waits for verification (table above), but
the live preview *text* no longer does: it appears immediately, rendered
dimmed (`TranscriptionInterimPending`, GUI-only; the overlay shows it with
the text at 40 % opacity and no Listening badge). The words are still never
pasted and never routed to any consumer (Iris) until the speaker is
verified — `TranscriptionInterimPending` is a distinct event type that no
consumer subscribes to. On verification the current preview is re-sent at
once as `TranscriptionInterim` (normal rendering, routable). If the gate
rejects the utterance the daemon ends voice activity and the GUI drops the
dimmed text; a dimmed preview with no update for 2.5 s also expires.
Measured (offline replay, `src/always/replay_tests.rs`): stranger/TTS
voices produced only pending previews and zero `TranscriptionInterim`;
the user's recordings went pending → verified in p50 254 ms (n=3, max
1761 ms; utterances verified before any text existed skip the dimmed
stage).

**Instant preview engine (2026-10-14).** With the Apple backend, two
recognizers run per utterance and both PUSH partials from their own
callbacks (no per-frame polling) into one merger (`live_preview.rs`):
- **Parakeet EOU 120M, 160 ms chunks** (FluidAudio 0.17.7, Core ML on
  `.cpuAndNeuralEngine`) in a resident helper process
  `always-preview-asr` (source `preview-asr/`, protocol in
  `preview_asr.rs`). Spawned and warmed at daemon start, fed the 200 ms
  pre-buffer at onset plus every in-utterance frame. **English only**
  (`lang` `en*`, or `auto` with an English system locale); other languages
  keep the Apple partials alone. **Its text is never pasted** and never
  used as a final — with one exception: the tail of a chunked utterance
  whose Apple pass failed (§4, 2026-10-15).
- **The Apple streaming recognizer** — the source of the PASTED final, as
  before. Wherever Apple already has words, Apple's words replace
  Parakeet's; Parakeet supplies only the words past Apple's last one
  (anchored on Apple's last word, or by spoken length when Parakeet
  substituted it).
Measured on replayed audio through the real pipeline (machine load
average 60–80 during the runs): first preview text p50 430 ms / p95 605
ms after the first loud sample (n=5 TTS sentences; includes the 60 ms
onset debounce and the first word's own duration); every word was on
screen before the speech ended in 5/5. Helper: model load 0.35–0.9 s warm
(83 s the very first time Core ML compiles for the ANE), RSS ~55 MB
steady, CPU ≈8 % of one core while decoding, ≈0 idle. If the helper is
missing, not yet warm, or dies, the utterance simply has Apple partials
only (respawn is retried at most every 30 s); audio is handed to it
through a bounded queue, so a stalled helper can never block capture.

**Live transcript text** is shown whenever the daemon produces a provisional
preview — the GUI renders any non-empty partial it receives, during the
transcribing wait *and* while the user is still talking. The daemon, not the
GUI, decides when previews flow. (Previously the GUI discarded previews unless
the active model claimed streaming; that gate is gone.) A model that returns
one finished transcript per utterance must still not claim streaming —
`Transcriber::supports_streaming()` stays `false` for the cloud backend.

The daemon's live-preview loop (`vad.rs`, `preview_cadence`) re-transcribes the
growing buffer on an interval while the user is still talking. Three ways it
arms, in priority order:

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

With `stt_live_preview` off, the overlay shows no live transcript text at
all: no mid-speech previews (a streaming engine's preview still runs, for
live typing, but shows nothing), no per-chunk rolling text on long utterances,
and the speculative transcription at a pause is not displayed either (it
still runs, for the final). Consume mode is exempt —
its consumers depend on the speculative chunk (product-owner request
2026-10-22: "if they don't activate it, it should not be present in the
overlay either").

`stt_live_preview` stays ON by default. While Groq is the active backend,
Settings → Behavior shows a warning under the toggle: live transcript
re-sends speech about every 1.5 s (≈5× more Groq requests), which on Groq's
free tier (2,000 requests/day) can use up the daily quota after about 45
minutes of speaking. On macOS it adds that Always says when the day's quota
runs out (§ Models, Groq daily quota) (product-owner request 2026-10-22).

### Live typing (opt-in: `live_typing`)

Words appear in the focused application while the user is still speaking.
Off by default; the paste-at-the-end behaviour above is unchanged when off.
Settable from Settings → Behavior, “Type words while you speak”.

- **Source:** the live preview of this section — committed chunks (final
  quality) followed by the preview of the speech after them. While a chunk is
  in flight its text is unknown and nothing is typed. With the Apple backend
  the source is the instant-preview merger (Parakeet + Apple partials) — its
  verified previews only; Parakeet's words are typed provisionally and, like
  every typed word, corrected to the Apple final.
- **What is typed:** the committed text, then the words every preview of the
  last 600 ms agrees on (a word must read the same for 0.6 s before it is
  typed, or before a typed word is changed). The newest word of a preview is
  never typed early. Differences in case or punctuation alone never rewrite
  typed text, while speaking or at the final: the typed form stays and only
  new words are added. Local
  engines preview at least every 350 ms while live typing is on, and sooner
  when a preview pass is cheap: the next one starts twice the last pass's
  duration later, never under 150 ms, so previews use at most half the time
  (cloud cadences are unchanged).
- **Corrections:** when the committed text, or an agreed word, differs from
  what was typed, the difference is corrected at once (Backspace, then type),
  so typing never stalls behind one changed word. The final payload — merge,
  dedupe and post-processing exactly as `deliver_paste` computes it — is the
  last correction; it replaces the paste keystroke. The payload still goes to
  the clipboard (history), and auto-enter, the dictation buffer and voice edit
  behave as after a paste.
- **Outcomes that are not a paste** (cooldown, rejected, hallucinated, foreign
  speech, too quiet, noise, not the user's voice, duplicate) remove what was
  typed for the utterance.
- **Invariants:** previews exist only for speech captured while listening and,
  with My Voice on, verified — I3 and I6 hold for typed words as for pasted
  ones. Any pause stops typing for the rest of the utterance. When another
  application takes the microphone the session ends without one more
  keystroke (I7); words already typed stay, as speech authorised at capture
  time (I3).
- **Focus:** no keystroke is ever sent anywhere but where typing started,
  checked before every keystroke batch — macOS: the frontmost app and its
  focused Accessibility element; Linux: the focused window (Hyprland). If
  focus moved, the typed words stay where they were spoken and the final is
  kept recoverable (⌃⌥V), as for a paused utterance whose focus changed.
  Linux compositors without a focus query: focus is assumed unchanged ❓.
- **Typing:** voice editing's keystroke path (`ax_edit::post_strokes`):
  macOS posts Unicode text and Backspace events, the text as events of at most
  20 UTF-16 units spaced 25 ms apart: sent back to back, a busy receiver
  (Claude Code in a long session) re-inserted the tail of the burst once per
  queued event, so the end of a dictation appeared two or three times
  (owner report 2026-10-22); Linux uses `wtype` (virtual
  keyboard: layout independent, works in terminals without a paste chord),
  else `ydotool`, else `xdotool`. The keystrokes are marked as Always's own,
  so they never count as user input (voice-edit gate, §9c).
- **Consume mode** (Iris holds the line) never types.

---

## 6. My Voice (speaker verification)

Optional. When on, only the enrolled user's voice is transcribed — other people,
television, meeting audio and music are ignored.

**Enrollment** records three passes (normal, lower, louder), each needing 5 s of
voice. Each pass is stored as its own embedding, plus their combined average.
In Settings, the My Voice switch cannot be turned on until all three samples
are recorded, but it remains possible to turn it off if the saved setting is
on. When off without a complete profile, the UI explains that enrollment is
required before the gate can be enabled.

**Enrollment readiness.** The speaker model (26.5 MB, SHA-256 checked) is
prefetched in a background thread at every daemon start, whatever the gate or
enrolled state, so the first enrollment normally starts at once. If it is still
missing when a recording is requested, the daemon announces `VoiceEnrollmentPreparing`
and the UI shows "Preparing voice model…"; "Listening — speak now" appears only
after `VoiceEnrollmentStarted`, i.e. when the mic is really being read. The 45 s
recording window starts then, not at the click. The onboarding Record button is
disabled while the daemon is disconnected, as in Settings. Unknown events are
ignored by both ends, so an older GUI or daemon simply skips `VoiceEnrollmentPreparing`.

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

**No trust window.** An earlier build remembered a confirmed match for a period
(10 s, later 5 min) and let the badge appear on voice onset without re-verifying
inside that window. That made the overlay flash on every non-user voice —
videos, meetings, media played while sleeping — and the mic kept capturing audio
that would never be the user's, which blocked dictation until the media stopped.
Reverted at the owner's request: every sentence re-proves identity from scratch.
The cost is up to ~2 s of verify wait at the start of each utterance under the
gate (§5); the benefit is that the badge and the mic only ever engage for the
enrolled user.

**Verification runs off the capture thread (2026-10-14).** A 1.5 s
embedding costs 146–330 ms (measured), not the 30–50 ms once assumed; run
inline every 0.25 s of voice it stalled the capture loop behind real time,
which is what produced the 2–6 s "Listening" cluster (prod: 163 of 618
utterances, `listening_overlay_shown` p95 4219 ms). Scores now run on a
worker (one job in flight); the loop never waits on a model call, and the
final check reuses the verdict already in flight. A failing streak of
windows only triggers a whole-utterance re-score (capped at 6 s of
audio); only a failing re-score drops the utterance.

**Own voice over media.** While system audio plays
(`NotifySystemAudioState`), the early-reject streak starts at 4.25 s of
voice instead of 2 s; the threshold itself never moves. Scores are also
taken against an in-memory pool of the last 8 accepted own-voice
embeddings (never persisted; an embedding is only admitted if it passed
against the enrolled profile alone, so the pool cannot drift toward a
stranger; cleared on re-enrollment). Offline replay (25 own clips, 16
strangers incl. 6 TTS voices, 12 music-only): own voice over music at 5 dB
0.80 → 0.88 pass, over speech at 5 dB 0.36 → 0.76, verify p95 4891 → 4021
ms overall; false accepts stayed 0.

**Rejected words are recoverable (2026-10-14, user lost a dictation to a
false reject under music).** A rejected utterance is still never pasted,
but its audio is transcribed in the background and kept in memory
(`rejected.rs`, last 8, 10 min, never written to disk) and announced as
`TranscriptRejected { id, text, score }`. A consumer may offer "paste
anyway?"; `PasteRejected { id }` (empty id = most recent) runs the normal
paste path. Nothing is pasted without that explicit command.
❓ This explicit "paste anyway" is a user override of I3 (requested
2026-10-14 via the coordinator); I3's wording has not been changed — the
product owner should confirm or amend it.

**Threshold** is a preference (code default 0.50; this file previously
said 0.40). Measured on the owner's profile:
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
| Audio output | System audio is playing | Playback stops |
| Idle | No voice for `idle_pause_secs` while Always is otherwise listening (never set while another pause is active — its clock restarts instead) | Voice detected again, or an explicit user resume |
| No GUI | Daemon lost its last client | A client connects |

**Rules:**
- Any active source suppresses capture.
- **An explicit user resume** (global chord, idle pill Play, `Resume` or
  `TogglePause` over UDS, CLI, tray) clears master, idle, audio-output and
  mic-conflict at once, and never pauses. A global toggle resumes whenever any
  of those is active — or when Always is still in its paused-by-default start
  state with no source active (platforms without a focus monitor, e.g. Linux:
  otherwise the first press set master and the user had to press twice) —
  and only otherwise sets master. The watchdogs re-arm on
  their next transition, so the user can force dictation while music plays or
  another app holds the mic. This is a deliberate user override of the
  watchdogs; I7 still holds: when another app takes the mic Always stops
  immediately, and it is the user who resumes. After each control press the
  daemon re-broadcasts the authoritative `Paused` / `Resumed` state so a GUI
  that predicted wrongly is corrected.
- **Per-app chord** (for the focused app): if the app is effectively paused
  (not on the allowlist, or idle / audio-output / mic-conflict active), it is
  added to the allowlist if absent and idle / watchdog pauses are lifted; if it
  is listening, it is removed from the allowlist; while master is on, the chord
  edits the allowlist only. Allowlist edits from Settings / Onboarding
  (`SetAppPaused`) are plain edits and never lift idle or watchdog pauses.
- On resume, audio queued by the recorder during the suppressed period is
  **discarded** (I6). The recorder never stops, so its buffer holds several
  seconds of exactly the audio Always was meant to ignore.
- An utterance already being recorded when a pause source fires is **not**
  aborted mid-capture (only a mic conflict does that, §7.1). It finishes
  recording and is transcribed in full; the pause decides only whether the
  result is pasted.
- **A pause that arrives mid-utterance normally withholds the paste**, so a
  transcript cannot leak into a window the user has since moved to. The text
  is **never lost** (changed 2026-10-14 after a dictation vanished as
  "Skipped paste: app paused mid-utterance" when focus moved to an excluded
  app): it is kept in the recoverable ring (`rejected.rs`) and announced as
  `TranscriptRejected { score: "-1.000" }`, pasteable with `PasteRejected`;
  it is also held in the single-slot filtered buffer and the overlay offers
  "press ⌃⌥V to paste anyway". Logged at info as
  `paused_mid_utterance_withheld_recoverable` with the active pause sources
  (`master`, `per_app`, `mic_conflict`, `audio_output`, `idle`, `no_gui`).
  ❓ A per-app pause that begins while a capture cycle is still waiting for
  voice does not end that cycle, so speech spoken after the pause can still
  be captured and transcribed (seen in the 2026-10-13 incident); it is now
  recoverable rather than dropped, but whether such a cycle should be
  abandoned instead (I6) needs a product decision.
- **Exception — master pause with unchanged focus (changed 2026-08-31, at the
  product owner's request):** when Master is the *only* active source and the
  focused app is still the one the utterance began in, the transcript **is
  pasted** rather than dropped. Muting means "stop listening", not "discard
  what I already said", and with focus unchanged there is no other window to
  leak into. The originating bundle id is captured before the first frame is
  recorded (`pause::set_dictation_origin_app`) and compared at paste time; an
  unknown origin fails closed to the withhold (recoverable, not pasted).
  Logged at info as `paused_mid_utterance_pasted`. Every other source — per-app, idle,
  mic conflict, audio output, no-GUI — keeps the withhold (recoverable, never
  typed automatically), because those genuinely mean "this window must not
  receive dictation".
- Focus-driven pause/resume must be silent — switching windows must not flash
  badges.

### 7.1 Microphone conflicts

Another application taking the microphone is detected within about a second.

- Capture stops **immediately**, mid-utterance, not at the next loop boundary.
- Words captured before the cut are still transcribed and recorded in the log,
  but **never pasted** — the other app is about to paste its own version.
  They are kept recoverable (ring + `TranscriptRejected`, logged
  `mic_conflict_preempted_kept_recoverable`); only an explicit
  `PasteRejected` types them (2026-10-14, "dictation must never be lost").
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
'["bundle.id"]'`. Each entry matches a bundle id, an executable basename, or
(when it contains `/`) an executable path prefix.

**Process taps are not microphone use** (2026-10-14, after Iris's
`music-tap` paused Always for 25 min and pre-empted a dictation). A Core
Audio process tap of system *output* is consumed through a private aggregate
device, so coreaudiod reports the tapping process as "running input". A
process only counts as a mic user if at least one device it runs input on
has input streams and is not an aggregate made solely of taps (transport
`grup`, non-empty tap list, no active input sub-device). When the device
facts cannot be read, the process still counts. Iris's helpers are also
ignored by name/path regardless: `music-tap`, `caption-tap`, `hud-tap`,
`translate-tap`, `sense-probe`, `focused-editable`, `pcm-play`,
`life-helper`, and any binary under `iris-mama/packages/*/native/` or
`~/.iris/cache/`.

**Always never conflicts with itself** (2026-10-14 regression: the new
`always-capture` recorder, labelled "Always" by LaunchServices, paused the
daemon for a conflict that could never clear). A capturing process is self
when it is the daemon, a direct child of the daemon, any executable inside
the daemon's own `.app` bundle, or named `always`, `always-daemon`,
`always-capture`, `always-preview-asr`, `rec`, `sox` or `coreaudiod`. As a
last line of defence, if every app the watcher would blame is labelled as
Always itself, it refuses to pause — or, if already paused, resumes on the
next poll — and logs `mic_conflict_self_only_ignored` as an error.

**Every mic-conflict auto-pause auto-resumes** once the mic has been free
for 3 consecutive 1 s polls. A failing probe while paused counts as a free
poll, so a broken probe can never strand Always paused.

**Mic unavailable** (2026-10-17, requested by Livio: "Always loses the mic and
it's impossible to retrieve it"). After 3 consecutive recorder failures (§7.2)
the menu bar shows **"Mic stuck — Fix"**, which reaps the daemon's audio
children and restarts the daemon. The item hides when audio resumes or the
daemon disconnects. This is distinct from a mic conflict: nothing else holds
the mic, the device is simply delivering nothing.

### 7.2 Recorder health and respawn

The `rec` (SoX) process runs for the daemon's lifetime and is only replaced
when genuinely unhealthy. CoreAudio "buffer overrun" messages trigger a respawn
only as a **rate**: at least 64 overruns within one 60 s window. Overruns are
**not counted while capture is deliberately gated** (pause, mic conflict).
(The previous lifetime-cumulative counter condemned a healthy recorder after
any single benign backpressure episode — 291 respawns in 2 days, each
costing ~4.5 s before the first audio arrived. Most of those 4.5 s was the
4.096 s SoX output buffer, not the device; the daemon logs
`rec_first_audio` with the real figure since 2026-09-25.)

The recorder's reader thread drains `rec` whether or not capture is gated,
so gates no longer create backpressure; audio queued during a gate is
discarded on resume (I6), including frames still inside SoX when the gate
lifted, by capture time.

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

The same rules apply when the recorder is the echo-cancelling `always-capture`
helper (§3): one process, killed and reaped before any replacement. The helper
also exits on its own when AVAudioEngine reports a configuration change (route
or device change) or when the daemon goes away; the daemon sees EOF and
respawns it through the normal path. It emits no CoreAudio overrun lines, so
the overrun-rate respawn never fires for it. A `RespawnRecorder` (default
input change) re-enables the helper after an earlier start-up failure.

**Escalation when the mic stays dead** (2026-10-17, requested by Livio). The
18:50–18:59 outage that day: a default-input change wedged the device, the
helper failed to start (VPIO -10875, then plain `'what'`), the daemon fell
back to SoX, and SoX delivered nothing — `rec_timeout_recorder_reset` fired
~40 times at 15 s intervals forever, and manual restarts hit the same path.
- A failed helper start (VPIO or plain) is remembered for **60 s**, not for
  the daemon's lifetime; after that the next spawn retries the native helper
  before falling back to SoX.
- Read timeouts, EOFs and failed spawns count as **consecutive failures**.
  Respawns back off **1, 2, 5, 15, 30 s** (the last repeats); the wait runs
  outside the recorder lock.
- **3** failures log `mic_stuck` and broadcast `MicUnavailable { true }` (§7.1).
- **6** failures make a GUI-spawned daemon (`ALWAYS_SPAWNED_BY_GUI=1`) drop
  its recorder, remove its pid/socket files and exit with code **75**, so the
  GUI respawns it. A terminal-launched daemon keeps retrying.
- The first delivered frame resets the count and broadcasts
  `MicUnavailable { false }`.
- SoX runs in its own process group and the group is killed on drop. At the
  first spawn of a daemon, and when the GUI kills a stale daemon, orphaned
  (ppid 1) `always-capture` binaries from the app bundle and `rec`/`sox`
  processes with the daemon's exact argument signature are killed (I4).
- The GUI coalesces default-input changes: `RespawnRecorder` is sent only once
  the default input has been quiet for **1.5 s**.
- ❓ A device wedged at the coreaudiod level may only recover after a
  coreaudiod restart or a replug; Always does not restart coreaudiod.
### 7.3 Daemon memory ceiling and self-restart

The daemon checks its own **physical footprint** every 30 s (same metric
`vmmap --summary` reports). If it is past **3 GiB** AND the daemon is not
actively listening — effective-paused, or no voice for at least 60 s — it
logs `rss_ceiling_self_restart`, cleans up its pid/socket like the orphan
watchdog does, and exits; the GUI respawns it within seconds and the
dev/prod mic handoff covers the transition. It **never** restarts
mid-utterance, and a failed footprint reading never triggers a restart.

Why: ONNX Runtime 2.0.0-rc.12 on current macOS builds retains a slice of
every inference's working buffers at the process level for the lifetime
of the process (measured 2026-09-29: 0.7 GB at launch → 4.2 GB within an
hour of dictation; session recycling and the arena/pattern options do
not bound it). A heavy dictation day otherwise grows the daemon into the
tens-of-GB regime the user reported. The ceiling converts that into a
bounded sawtooth — a quiet-moment blip of a few seconds at most a few
times a day instead of unbounded growth.

---

## 8. Models and transcription backends

A cloud backend and a set of downloadable local models. Local models run
offline; the cloud backend is faster to start and needs an API key.

**Rules:**
- **Never list a model that cannot run** (I8). Every catalogue entry must name an
  engine with a real implementation, a correct download size, and a checksum.
- A local model may be armed as fallback for the cloud backend; failures fall
  back silently but are logged.
- **Groq daily quota used up (product-owner request 2026-10-22).** When Groq
  answers 429 with `x-ratelimit-remaining-requests: 0` (Groq's request
  headers always describe the per-day quota), the daemon sends
  `GroqQuotaExhausted` — at most once per UTC day, and only while an app is
  connected to receive it (otherwise the next such 429 tries again). A per-minute 429 (daily
  requests left) sends nothing. If Groq is still the active backend, the
  settings app asks whether to switch to an on-device engine: the best
  downloaded local model (recommended first, then speed + accuracy — the
  fallback pick). With none downloaded: macOS 26+ offers Apple's recognizer;
  older macOS offers to download the best catalogue model by the same ranking
  and switches when it is installed; Linux offers the same download, shows its
  progress, and switches when it is installed (closing the dialog
  mid-download leaves the download running but does not switch). "Keep Groq"
  changes nothing; the switch takes effect like any model switch, and
  switching back is manual (Settings → Models). macOS returns focus to the
  previously frontmost app after the prompt (the native app asks; the Tauri
  Settings window, which also runs on macOS, stays out of it so the prompt
  never appears twice); Linux shows the Settings window
  to present it, except during first-run setup (it then waits for Settings). Installs without the settings app (Linux tarball, headless)
  get no prompt. Live preview counts against the same quota (each preview is
  a Groq request).
- Switching models takes effect on the next utterance.
- Linux development and release builds include the local STT engines. Selecting a local
  model never invokes Groq or requires its API key; missing files or missing
  local-engine support produce a local-model error and keep the previous
  backend selected.
- On Linux startup, Groq is eligible only when a non-empty key is configured.
  Without a key, a downloaded local model is chosen (recommended first, then
  combined accuracy and speed); with none downloaded, the backend is `none`
  and Settings shows “No model selected” with setup guidance. A saved local
  selection takes precedence. A failed startup load reports its error and
  returns to `none`, rather than showing an unavailable backend as active.
- Installs without the Settings app (the Linux tarball, headless machines) use
  `always model list | download <id> | use <id>`, which send the panel's own
  daemon commands (`ListModels`, `DownloadModel`, `SetActiveTranscriber`).
  `download` returns once the catalogue shows the model installed and
  verified; `use` once the daemon names that backend as running. Both exit
  non-zero on the daemon's failure events, and `use` rejects a malformed id
  before sending it (the daemon would only log it and never answer).
- A switch loads the new engine before the config names it, and never under
  the config lock the main loop reads, so a slow load does not block dictation.
  Engine rebuilds (a switch, a language change) run one at a time.
- Every `SetActiveTranscriber` request ends with `ActiveTranscriberChanged`
  naming the backend now running — no-ops and failures included.
- A failed load first sends `ActiveTranscriberFailed { backend, error }` and
  leaves the previous backend active, so a retry really retries. The Models tab
  shows the error on that row.
- If the daemon never answers, "Loading…" gives up after 120 s with a message
  on the row; a late success clears that message.

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
- The assistant's name — `Iris` and `Hey Iris` — is in the default list
  (requested 2026-10-14: the recognizer hears it as "Irish"/"IRS"). It is a
  default, not a glossary term: the glossary's phonetic matcher would also
  rewrite "Irish" mid-sentence. The file path's alternative-hypothesis
  rescore matches phrases as case-insensitive substrings, so an alternative
  containing "Irish" also counts as an "Iris" hit there. The Parakeet live
  preview (`StreamingEouAsrManager`, FluidAudio 0.17.7) has no vocabulary
  biasing API; its text is preview-only.
- Missing locale assets are installed via
  `AssetInventory.assetInstallationRequest(supporting:)` on first use.
- It is a non-streaming backend: `Transcriber::supports_streaming()` returns
  `false` and `transcribe_streaming()` yields a single final result.
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
- The live preview cadence is disabled for the Apple backend. Previews are
  non-streaming, compete for the serialized recognizer, and add latency without
  improving the final paste.
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
  row is badged, and the same loading spinner is shown until the daemon
  answers (success, failure or no-op; 120 s give-up, see above).
- `Always.app/Contents/Info.plist` declares `NSSpeechRecognitionUsageDescription`
  so the bundled `always-daemon` can request macOS speech-recognition permission.
  The first transcription attempt triggers a TCC prompt; without this key the
  process is killed by the system before the user can respond.

---

## 9. Text pipeline

Between transcription and the keyboard:

1. **Hallucination filter** — rejects the known failure modes of speech models:
   empty output, "thank you"/"bye" family, subtitle credits, repeated tokens,
   gibberish, low-confidence segments.
   Groq backend only. Script mixing is judged by Unicode script, not ASCII: Latin
   with diacritics is ONE script ("déjà", "Ça", "naïve" are ordinary words). The
   user's scripts are Latin plus the script of `lang` (plus the engine-reported
   language's script only when `lang` is `auto`). A single garbled token (two
   scripts in one word, or a mid-word case flip carrying non-ASCII, e.g.
   "ünkLöß") is removed and the rest is pasted. The utterance is rejected when it
   holds only such tokens, when they are ≥ 25% of the words of a short utterance
   (< 80 chars), or when it is mostly in a script that is not the user's.
   **Trailing canned closer** (2026-10-17, requested by Livio: "thank you"
   injected at the end of sentences). When a Groq result ends with a separate
   final sentence that normalizes to "thank you", "thanks", "thank you very
   much", "(thank you|thanks) for watching", "bye" or "bye bye", preceded by at
   least 4 words, that sentence is removed and the rest is pasted. Mid-sentence
   uses are never touched. A final segment with its own metrics (avg_logprob
   ≥ -0.15, no_speech_prob ≤ 0.05, differing from the previous segment) is
   treated as genuine and kept. Known limit: Groq repeats one window's metrics
   on every segment, so on Groq a genuine closing "Thank you." after ≥ 4 words
   is also removed. ❓ Thresholds not tuned on real tail audio.
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
   failed, and a post-pass on the LLM's own output. It also runs on the live
   typing preview before it is typed, and on a ⌃⌥V paste of a filtered
   transcript: the engine can turn a spoken stutter ("the the") into ten
   copies, and both paths typed them verbatim (owner report 2026-10-22).
6. **Postprocess mode** — ONE setting (`postprocess_mode`, Settings → Models →
   Postprocessing), changed 2026-10-14 at the user's request ("it must be
   instant"; a "wait 250 ms then paste uncorrected" compromise was rejected).
   **The paste never waits on a language model.** Before, the blocking LLM
   pass (8 s timeout) was the 3–10 s insert delay: prod 2026-10-13, n=86,
   `latency_breakdown` total p50 266 ms / p95 9228 ms, grammar p95 8026 ms,
   0/86 speculative-cache hits; Apple Intelligence per call p50 5.5 s / p95
   8.6 s (n=80, offline harness).
   - **Always Fast** (`fast`) — the deterministic corrector
     (`det_correct.rs`) inline before every paste: fillers and adjacent
     repeats, glossary + learned corrections (exact wrong forms always;
     phonetic/edit-distance only at high confidence, never for ambiguous
     common words), recognizer artifacts (leading `-..`/`you..`, interior
     hesitation `..`, `, ,`, `it?'s`, clause-initial `and,`, false-start
     fragments like `your yourself`), `i` → `I`, capital after an internal
     sentence end. Never touches the first word's case, never deletes
     content words, never expands abbreviations. Budget ≤ 2 ms (measured
     debug build, thread CPU: p50 ~0.9 ms, p99 ~1.8 ms).
   - **Apple Intelligence** (`apple`) / **Groq** (`groq`) — Always Fast
     first, so the glossary is always honoured; the LLM runs only as the
     speculative warm at a pause, and its result is used if it is ALREADY
     cached at paste time, otherwise skipped silently (no timeout wait).
     With today's latencies this almost never lands in time — it is kept
     as an option, not a dependency.
   - **Off** (`off`) — raw transcript + `local_cleanup` only.
   - **Leading wake normalisation** (all modes, before the mode branch and
     the short-utterance/snippet paths, so the consume-mode handoff sees it
     too): a misheard assistant name as the FIRST token (or right after one
     greeting: hey/hi/hello/ok/okay/yo/you) — irish, irs, i.r.s., ires,
     iras, ira's, iris's, iris', eris — becomes `Iris`, but only when the
     next word opens a request (question word or imperative verb, or an
     auxiliary followed by a subject / in a sentence ending `?`). "Irish
     coffee is great", "IRS forms are due", "I told Irish…" are untouched
     (`det_correct::normalize_leading_wake`, requested 2026-10-14).
   - **Migration** when `postprocess_mode` is unset (no silent change of
     kind): Smart Postprocess on + provider Apple → `apple`; on + Groq
     (or unset provider, the old default) → `groq`; Smart Postprocess off
     → `fast` (the corrector supersedes the old bare local cleanup). The
     stored legacy prefs are not rewritten. A mode change restarts the
     daemon, like the provider switch it replaces.
   Its prompt enforces token preservation (never expand one word into a
   phrase or contract a phrase into one word) and a certainty bar for
   glossary substitutions. A bounded domain-vocabulary section lists the
   user's canonical terms so the model can resolve phonetic mishearings
   (`kit rug trees` → `git worktrees`) only when strong phonetic similarity
   AND technical context both hold; a meta-speech rule keeps quoted wrong
   words literal (`puts cloud instead of Claude` keeps `cloud`).
   The speculative warm builds the request through the same builder as the
   paste path (`correction_request::build`), which keeps the cache keys
   byte-identical:
   - **Un-chunked utterance:** the tentative-silence speculation warms the
     grammar key for its transcript as soon as speculative STT returns.
   - **Chunked utterance:** the paste-time call is keyed on the *join* of the
     chunks (+ tail), a key the per-chunk corrections never touch. Each
     chunk takes part in the join as `finalize` would take it at that
     moment — corrected once its per-chunk correction landed, raw before —
     so the join is warmed at each point it can change: when a chunk's raw
     text arrives, and again when its correction lands. A voiced tail's
     speculation warms join + tail. Warm and paste build the request through
     the same builder (`correction_request::build`), which also applies the
     local cleanup (step 5) itself, so raw warm text and cleaned paste text
     produce byte-identical cache keys.
   - The Groq request for gpt-oss models asks for `reasoning_effort: low`
     and no reasoning text in the response: the call is on the paste path,
     and hidden reasoning tokens are generated before the answer.
   Transcript before/after text of a correction is only logged when
   transcript logging is on (I9); the old unconditional "grammar
   correction applied" log line leaked it in release builds.
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
  without this field are left unchanged. Every entry created by the
  vocabulary import (first-launch auto-import or `always vocab import`) is
  written with `"provenance": "import"`, `"mistranscriptions": []` and
  `"frequency": 100`.

### Hot reload

The glossary is loaded with modification-time-gated reloads. Writing to
`glossary.json` (via `apply_pairs_to_glossary`, `bump_frequency`, or external
edit) invalidates the in-memory cache, and the next access reloads from disk.
No daemon restart is needed for new corrections to take effect.

### Vocabulary auto-import (first launch)

Always seeds the glossary from words the user already owns, once, so it spells
their vocabulary correctly from day one.

- **When it runs.** On daemon start, if the one-time marker file
  `vocab_auto_import.json` does not exist in the per-instance config dir
  (`~/Library/Application Support/always/`; the dev app uses `always-dev/`, so
  each instance imports once). It runs in its own background thread, started
  after the UDS server is accepting connections — it never delays audio or the
  GUI connection. macOS only: on Linux and Windows it is a no-op (those
  platforms have no matching sources, and `always vocab import` remains
  available there on request).
- **Once.** The marker is written only after the import succeeded, holding
  counts and a timestamp (no term text). Later starts find it and do nothing.
  A failed import (for example a corrupt `glossary.json`) writes no marker and
  is retried on the next start; a repeated import only adds terms that are
  still missing, so retrying is harmless.
- **Sources** (all read locally and read-only; nothing is sent anywhere):
  1. installed application names — the `.app` bundles directly inside
     `/Applications` and `~/Applications`, name without the `.app` suffix
     (non-bundles such as `.DS_Store`, `.localized`, `Utilities` and hidden
     names are skipped);
  2. macOS Text Replacements — the `with` side of every pair;
  3. Wispr Flow — the `Dictionary` table of
     `~/Library/Application Support/Wispr Flow/flow.sqlite`: the `replacement`
     when set, otherwise the `phrase`; soft-deleted rows and text-expansion
     snippets are skipped. Wispr Flow counts as present when its database or
     its app bundle (either Applications folder) exists;
  4. SuperWhisper — its Vocabulary list and Replacements, plus mined
     transcript terms. The explicit part comes from
     `<SuperWhisper folder>/superwhisper/settings/settings.json` (the folder is
     the `appFolderDirectory` of the `com.superduper.superwhisper` defaults
     domain when set, otherwise `~/Documents`): every `vocabulary` string and
     the `with` value of each `replacements` entry, uncapped. The mined part
     is statistical outlier words from the transcripts of its 5000 most
     recent recordings (ordered by `recording.datetime`), capped at 200.
     SuperWhisper counts as present when its app bundle or its
     `settings.json` exists. All of it is counted under `superwhisper`;
  5. MacWhisper Replacements, and the legacy Dragon seed list when Dragon is
     installed.
- **Merge rules.** Only terms missing from the glossary are added (compared
  case-insensitively); existing entries — including hand-tuned
  `mistranscriptions`, `frequency` and `weight` — are never modified. New
  entries carry `provenance: "import"`. When nothing is new the file is not
  rewritten. An unreadable or non-array `glossary.json` is left untouched and
  the import fails rather than replacing the user's entries.
- **Atomic write.** The glossary is written to a temporary file in the same
  directory and renamed over `glossary.json`, so a concurrent reader (a
  correction being applied) or a crash never sees a half-written file.
- **Takes effect without a restart.** After the write the in-process glossary
  cache is invalidated and the next access reloads from disk (see Hot reload);
  an import run from the CLI is picked up by the daemon through the
  modification-time check.
- **Logging.** One structured event, `vocab_auto_import`, with per-source
  counts only: `app_names`, `wispr_flow`, `superwhisper`, `text_replacements`,
  `macwhisper`, `other`, `unique_terms`, `added`. Term text is never logged —
  Wispr Flow and SuperWhisper words are the user's own data.
- **Manual re-run.** `always vocab import` runs the same code path on demand,
  ignores the marker, and works on every platform; use it after installing
  Wispr Flow or SuperWhisper later.

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

## 9c. Voice editing

Requested by the product owner 2026-10-14 (gap #11). Code: `voice_edit.rs`,
`voice_edit_grammar.rs`, `ax_edit.rs`.

**Commands** (the WHOLE utterance, after punctuation/case normalisation and an
optional leading "please"/"okay" or trailing "please"):

| Command | Phrasings (examples) | Effect |
|---|---|---|
| scratch that | scratch / delete / remove / erase / strike that | removes the last paste exactly (repeatable: walks back up to 8 pastes in the same field) |
| replace | change X to Y, replace X with Y, correct/fix X to Y, swap X with Y (each side ≤ 5 words) | replaces the last word-bounded, case-insensitive X **inside the last paste**; Y takes X's capitalisation |
| case | capitalise that, uppercase that, lowercase that | title / upper / lower case of the last paste |
| list | make that a list | last paste → "- item" lines (sentences; else commas/"and") |
| new line / new paragraph | new line, new paragraph | inserts "\n" / "\n\n" at the caret (never a bare Return: no accidental send) |
| press enter (opt-in `spoken_enter`) | the user's `spoken_enter_phrases`; default: press enter, hit enter, press return, hit return, send it, send that, submit it/that | presses Return — submits in chat apps and agent prompts. Off (default): the words are dictated. No bare "enter" by default |
| undo / redo | undo (that), redo (that) | ⌘Z / ⇧⌘Z |
| select | select that, select the last word, select the last sentence, select all | selection |
| delete | delete the last word, delete the last sentence | removes it (plus trailing space) |
| go to | go to the end / start | caret |
| rewrite | make that shorter / more formal / friendlier, fix the grammar, translate that to <language> | handed to Iris (on-device model) — see below |

**Press enter** is the one command that sends: it is off unless the user turns
on `spoken_enter`. Its phrases are the user's (`spoken_enter_phrases`,
comma-separated, matched after the same normalisation and polite affixes as
every command; blank = the defaults above); they are checked before the fixed
grammar, so a phrase the user picks wins over a built-in command. It passes
the same gate as every command — said on its
own, right after a paste into the same field, nothing committed since. Once it
has pressed Return the paste counts as committed, so a second "send it" is
dictated rather than pressing Return on an empty field. With live typing on,
the command's own words are removed before Return is pressed.

**At the end of a dictation** (opt-in `spoken_enter_at_end`, needs
`spoken_enter`): "…and run the tests, go go" — the phrase ending the
utterance (matched word by word, punctuation ignored, before the
repeat-removing cleanup) is cut with the punctuation joining it, the rest is
pasted as usual, and Return follows the paste through auto-Enter's 0 ms path
(regardless of the auto-Enter setting and its minimum word count; never for an
incomplete dictation, a withheld or paused paste, or after focus moved). A
phrase must be chosen that is never dictated: with the defaults, any sentence
ending in "send it" is sent — hence off by default.

**Linux:** there is no Accessibility API to verify the field or the caret, so
only `press enter` runs there (same window, ≤ 30 s after the paste); every
other command is dictated as before.

**Gate — dictation is never swallowed.** An utterance is claimed only when
all hold, otherwise it pastes as normal dictation:

1. the whole utterance parses as a command (≤ 12 words, ≤ 120 bytes);
2. a paste happened ≤ 30 s ago (or ≤ 30 s since the last voice edit), in the
   same app and the same AX field (`AXFocusedUIElement` snapshotted 60 ms after
   the paste, off the paste thread);
3. nothing committed it (auto-enter Return) and the user has not typed or
   clicked since;
4. the field is not secure (`AXSecureTextField`);
5. the command is executable right now: the pasted text still ends at the
   caret (AX fields), the replace target X exists in the paste, etc.

Saying "Iris, …" first ("Iris, scratch that") addresses the command explicitly:
step 2–3 are skipped; 4–5 still apply, and "change X to Y" may then search the
whole field before the caret.

**Execution.** Primary path is native AX: set `AXSelectedTextRange`, set
`AXSelectedText` (one undoable edit in Cocoa text views), restore the caret,
then read `AXValue` back and verify. If the write is unsupported, the range
stays selected and the text is typed over it (`ax_select+keys`). Fields with no
writable AX text (terminals, some Electron views) use synthesised keystrokes —
only when the field is provably untouched since the paste (no user input, no
untracked edit since); Backspace × the paste length for "scratch that". A
failed edit restores the prior selection and the utterance is pasted instead.
Edits serialise with the paste path (`try_begin_paste`), clear the
dictation-session merge buffer, and keep `last_pasted` (⌃⌥X baseline) equal to
what is on screen.

**Correction learning.** A "change X to Y" inside the last paste where X and Y
look like a recognition error (`looks_like_correction` or equal Soundex) is
learned like ⌃⌥X: applied to the glossary (provenance `manual`) when
auto-learn is on, else queued.

**Rewrite handoff to Iris.** Iris pings `VoiceEditRewriterAlive` (TTL 15 s).
A gated rewrite command is claimed only while Iris is alive and a target
exists (non-empty selection, or the last paste still in place); Always emits
`VoiceEdit { op: "rewrite_request", detail: style }`. Iris reads the target
(`VoiceEditGetTarget`), rewrites on-device, and commits with
`VoiceEditReplaceTarget { expect, text }` — one AX replacement, refused if the
target changed or the field is secure.

**Shadow dataset.** Every grammar hit (claimed or declined, with reason,
method and ms) is appended to `<config dir>/voice_edit_shadow.jsonl`; the
utterance text only when transcript logging is on (I9).

**Safety replay.** `cargo test --lib voice_edit_replay` replays every
utterance in `~/Library/Logs/Always/always.2026-10-1*` and
`~/.always/transcripts.jsonl` through grammar + gate with the previous paste as
anchor and real timing; it fails on any claim not whitelisted after a manual
read. 2026-10-14: 3 048 utterances, 0 grammar hits, 0 claims.

**UDS** (Iris): `VoiceEdit { req, command, force }`, `VoiceEditGetTarget
{ req }`, `VoiceEditReplaceTarget { req, expect, text }`,
`VoiceEditRewriterAlive`; answered with `DaemonEvent::VoiceEdit { req, op, ok,
method, ms, detail, text }` (strings only).

## 9a. First-run setup

Shown **once**, to someone who has never been through it, and never again.

Three steps:

1. **Engine (product-owner request 2026-10-22).** First, before Permissions,
   so a model download runs while the user grants them. "Choose Your Engine":
   option cards, in order —
   - **Groq (cloud):** best accuracy and speed, needs a free API key (link to
     console.groq.com/keys). "Save & Use" saves `groq_api_key` and switches to
     Groq once the daemon, restarted to load the key, reconnects; on macOS
     an invalid key is rejected inline first (Linux does not check it). Caption: free tier is about
     2,000 requests a day and live transcript uses more of it.
   - **Local model:** private and offline. Offers the best catalogue model
     (recommended first, then speed + accuracy); "Download & Use" downloads it
     with progress ("Keeps downloading while you continue") and switches when
     installed, even if the user has moved on to later steps or closed setup.
     A daemon restart mid-download (the Groq key save, or the mic supervisor's
     exit before the mic is granted) is survived: the app re-requests the
     download on reconnect and it resumes from the partial file. The daemon
     ignores a second `DownloadModel` for a model already downloading.
     Picking another engine cancels the pending switch. If one is already downloaded,
     "Use <name>".
   - **Apple on-device (macOS 26+ only):** no key, no download; the card says
     it works but results are not as good as Groq or a local model.
   The card for the active backend shows as selected. The step never blocks
   Continue — "Set up later in Settings → Models" continues and keeps the
   active backend (Apple on macOS 26+). Same step, same text on Linux, minus
   Apple.
2. **Permissions.** Microphone and Accessibility are required and gate the
   Continue button — without them nothing works at all. Input Monitoring is
   offered but never blocks: it enables the global shortcuts, not dictation.
   The window polls permission state, so granting in System Settings updates it
   without a restart.
3. **My Voice.** The three enrollment recordings, inline. Always skippable —
   "Skip for now" and "Finish without it" both complete setup.

**Last page: apps to turn voice typing on in.** The final "You're all set"
page lists apps to turn voice typing on in, because Always pastes only into apps
on the allowlist and every other app stays paused. The list is chosen by how
likely you are to dictate into the app, not alphabetically:

1. likely dictation targets that are **running** (Slack, Messages, ChatGPT,
   Claude, Cursor, VS Code, Zed, Chrome, Safari, Arc, Dia, Firefox, Mail,
   Notes, Notion, WhatsApp, Teams, Discord, Telegram), in that fixed order;
2. likely targets that are **installed but not running**, in the same order;
3. any other running regular (Dock) app, A→Z.

The list is capped at **10**, and the chosen apps are then *shown* A→Z. An app
appears once even if it is both running and installed. Never offered: Always
itself (every known bundle id), Finder, System Settings, Activity Monitor, and
password managers and keychain tools (Passwords, Keychain Access, 1Password,
Bitwarden). A browser stands in for web apps — Slack in a browser is covered by
turning that browser on, since the allowlist is per app. Every checkbox starts
**unchecked** unless that app is already resumed; ticking one resumes Always in
it, unticking pauses it again. With nothing to list the page says to open the
app and use "Resume Always for …" in the menu bar. ❓ The "installed" check
only looks up the likely-target bundle ids, so an installed app outside that
list never appears unless it is running; this is read from the code, not
exercised on a machine with every target installed.

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
The Groq API key was once removed from this flow for that reason; it is back
only as an optional choice in the Engine step, which never blocks (local models
need no key, and Settings → Models still holds it).

**Linux matches macOS.** The Linux onboarding uses the macOS layout, step order,
titles, text and buttons. Agreed differences: no Apple engine, Linux's own
permission rows, and no spoken narration (`say` is macOS-only)
(product-owner request 2026-10-22).

**Linux and Windows (Tauri app).** Both windows start hidden. At launch the
settings webview shows the onboarding window until completion is recorded, and
the settings window afterwards. Launched with `--hidden` (launch at login)
the Tauri app starts in the tray with no window once completion is recorded;
Settings stay in the tray menu. Completion lives in the webview's
`always_onboarding_done` flag. A saved Groq key or a recorded voice profile
(`always config show`, `always voice status`) retro-marks it. Finishing,
skipping, or closing the onboarding window records completion and shows
Settings. The tray's Settings → Onboarding item re-opens the flow from its first
step.
The Tauri app starts the sibling `always` daemon with `always start` before
creating its reconnecting IPC client. The daemon's single-instance guard makes
this safe when it is already running; a start failure is logged and leaves the
settings UI available.

---

## 10. Preferences

Stored in the daemon's database, editable from Settings and the CLI
(`always-daemon config set <key> <value>`).

The macOS Transcript History panel fills the available settings content height,
including while loading or empty. With no entries, its message is centered in
the space below the header; populated history uses that space for its list.
The Linux and Windows Tauri History panel reads daemon history rows from
`history.jsonl`, using the daemon's `ts` millisecond timestamp and `raw_text`.

**Settings controls.** Settings labels and explanatory text are not selectable
as ordinary web text. Inputs, editable text, and transcript-history entries
remain selectable. Models cards use subdued dark surfaces against the dark
window.

The Linux settings UI follows the macOS panel conventions for selection and
status: selected segmented options use the blue accent; the Models catalog
filters by All, English, or Multilingual and offers Translation, Fast, and
Streaming filters. A downloaded model leaves its in-progress state after the
daemon confirms it in the catalog, so the row settles into its downloaded
actions instead of spinning indefinitely.
The Fast filter includes catalog entries with `speed_score >= 0.75`, matching
the 0–1 score scale. Model rows use accuracy and speed bars, plus globe,
translation, streaming, and storage metadata icons. Target language selection
appears only for an active model that supports it; Linux omits the Auto-detect
choice from that picker.

The shared Windows/Linux settings layout follows the macOS reference in these
areas: Models keeps language, capability, and sort filters in one row and uses
an icon-only trash action for downloaded models; My Voice groups the gate label,
current status, and switch in one tinted row without changing enrollment rules;
Permissions puts a shield beside its heading, Re-check at the top right, and
one inline status indicator per permission. Library centers its tabs without
an extra content heading and puts the glossary file action and path above the
entries. About places update status beside Check for Updates. Platform-specific
permission names and actions remain intact.

On Linux, the Behavior switches for adaptive silence and live transcript apply
their changes to the running daemon immediately and save them for the next
launch. Models dropdown controls and their option menus use dark surfaces with
readable light text.

**Linux permissions.** Linux does not show the microphone-permission banner in
General: microphone access comes from the active audio session, and the
Permissions page reports the daemon/audio-session status. Global shortcuts
report their X11 requirement separately. Platform-specific banners wait until
the app has identified the platform, avoiding a Windows warning flash on Linux.

| Key | Default | Meaning |
|---|---|---|
| `lang` | `auto` | Dictation language. Groq sends it as Whisper's `language` (bare ISO code, `fr-FR` → `fr`); `auto` resolves to the system locale's language (as the Apple recognizer does), `en` if unknown. Before 2026-10-16 Groq was always sent `en`. |
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
| `postprocess_mode` | unset → migrated (§9 item 6) | `fast` · `apple` · `groq` · `off` |
| `postprocess_enabled` | true | Legacy; only read to migrate `postprocess_mode` when unset |
| `transcript_stream` | true | Append accepted utterances to `~/.always/transcripts.jsonl` |
| `capture_aec` | false (unset = off) | Echo-cancelled (VPIO) capture via `always-capture` (§3); false = plain helper (`--no-vp`) |
| `audible_status_sound` | off | Sound cues (off/low/medium/high; see §10.1) |
| `per_app_settings_json` | — | Per-application pause rules |
| `update_check` | true | Background GitHub release check (§12a); `ALWAYS_UPDATE_CHECK=0` also disables it |
| `live_typing` | false | Type words while the user speaks and correct them to the final transcript (§5, Live typing) |
| `spoken_enter` | false | "press enter" / "send it" on its own right after dictating presses Return (§9c) |
| `spoken_enter_at_end` | false | The Return phrase also works at the end of a dictation, without a pause (§9c) |
| `spoken_enter_phrases` | (built-in list) | What presses Return, comma-separated — e.g. `go ahead, ship it`; blank = defaults (§9c) |

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
- Audio frames carry their estimated capture time (§4). Measured from it:
  `listening_overlay_shown.latency_ms` (first speech frame → badge) and
  `latency_breakdown` (`speech_end_at` = capture time of the frame on which
  the silence window elapsed). `voice_detected.audio_delivery_lag_ms` is how
  far behind real time the onset frame was processed — near zero when
  capture flows; a regression to burst delivery shows there first.
  `rec_first_audio.ms_since_spawn` is the recorder's real cold start.
  `speaker_gate_scored` (info) records each voice check's cost.
  `grammar_cache_hit` counts a paste that joined a warm call still in flight
  as a hit.

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
| Updates (§12a) | background check + notify; apply on request | no background check; apply refused |
| Focus-state file | `~/Library/Caches/Always/focus-state.json` | `focus-state-dev.json` |
| Daemon build identity | `always.build-id` in the state dir | `always.build-id` in the dev state dir |
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
both transcribe and both paste. Starting Dev while production is live keeps
Dev passive: Dev pauses itself and watches production, without changing the
production pause state. When a newer daemon starts alongside an older peer,
it applies the same safe standby policy at socket readiness; it does not
assume that a PID check alone completed the handoff.

Cross-instance mutations use the optional `peer-pause-lease-v1` capability
negotiated in the initial `Hello`. A pause/release request carries a unique
request ID and requires a correlated `PeerPauseLeaseResult`; EOF, timeout,
legacy peers, and mismatched replies fail closed. A peer already manually
paused is acknowledged without acquiring a release lease. A matching lease
may release only the pause it acquired; any later manual pause/resume revokes
that lease and wins over a delayed release. A paused-by-peer daemon resumes
when its peer endpoint dies (~1 s), covering SIGKILL where no exit cleanup
runs; graceful exits release only a still-owned lease. An explicit Dev resume
is a deliberate takeover: it negotiates a conditional pause first and stays
paused if the peer cannot safely confirm it. Similarly, if a new production
daemon cannot safely pause a live older Dev peer, production enters standby
and watches that peer instead of allowing both to capture.

**Commands.**

- `scripts/dev-rebuild-macos.sh` — the macOS dev loop. Kills/rebuilds/relaunches only
  `Always Dev.app`; never touches the production install. Exits non-zero
  unless the dev GUI + daemon are running the just-built binaries, and
  warns if production went missing.
- `always start` compares a running daemon's recorded build identity with the
  current executable. A mismatch or missing identity is reported as stale or
  unknown and returns an error without stopping the live daemon; `always
  status` reports the same state. The identity distinguishes compiled daemon
  artifacts. Source edits that have not been built are outside this check.
  Process ownership is established from the OS-reported executable path and
  the daemon's `run` command, never from arbitrary argument text. A live IPC
  endpoint without a verifiable PID is reported as running with unknown build
  identity; `start` does not start a duplicate and `stop` never signals an
  unverified PID.
- `scripts/dev-rebuild-linux.sh` — builds the Linux daemon, settings frontend,
  and Tauri app, then launches and verifies the checkout's settings app.
- `scripts/promote.sh` — manual production promotion, user-invoked only.
  Kills production, rebuilds, deploys over `/Applications/Always.app`,
  relaunches, verifies. Agents must not run it.

- `scripts/launch-smoke/run.sh <Always.app> [secs]` — pre-promote launch
  smoke test (added 2026-10-14 after a promoted build paused itself on its
  own capture helper). Runs the built bundle's daemon + helpers for 30 s as
  an isolated sandbox instance (`ALWAYS_INSTANCE=smoke`: own socket, state
  dir, logs; no peer handoff; `ALWAYS_SKIP_ORPHAN_KILL=1`; `ALWAYS_NO_PASTE=1`
  so it can never type or touch the clipboard), with production
  master-paused for the window and always resumed. Passes only if the UDS
  client connects, capture runs at the real frame rate, there is no
  self mic-conflict pause, pre-emption, or reaping, and every status cue
  logs `status_sound_played` (with echo-cancelled capture: routed through
  the helper and rendered with a non-zero peak). It never changes system
  volume and never plays audibly: the sandbox sets `ALWAYS_CUE_DRY_RUN=1`,
  which renders cues offline to a buffer. A sandbox instance
  never pauses, resumes or reaps prod/dev daemons.

**Windows daemon lifecycle.** On Windows the daemon is not CLI-only: the
versioned IPC protocol is carried over a named pipe instead of a unix socket,
including the negotiated peer pause lease. `always start` spawns
`always run` detached (no console), single-instance is a named kernel
mutex (`Local\always-daemon[-dev]`, released by the kernel on process
death), liveness is the pid file + an `OpenProcess` image-name check
(recycled pids can't impersonate the daemon), and the process table
comes from a Toolhelp32 snapshot with command lines read out of each
candidate's PEB. `stop`/orphan-reaping use `TerminateProcess` — there
is no SIGTERM equivalent for a detached process — and stale pid files
are reclaimed on the next start. `always run` in a console shuts down
gracefully on Ctrl+C (pid file cleanup).

**Windows IPC — named pipe.** The JSON-lines `DaemonCommand`/`DaemonEvent`
protocol (same versioned schema as the UDS socket) listens on
`\\.\pipe\always[-dev]`. Its protected DACL permits only the creating
user and SYSTEM; remote clients are rejected. The pipe carries transcript
events, so it must not inherit the default Everyone/Anonymous read access.
The server creates the endpoint with `first_pipe_instance`, so bind failure is the pipe equivalent of the
live-socket check — a second daemon can't steal it (the kernel mutex
already prevents that anyway). Opening the pipe IS the liveness probe:
`ERROR_FILE_NOT_FOUND` = no server, success/`ERROR_PIPE_BUSY` = live.
`\\.\pipe\` paths never stat as files, so "socket exists" checks on
Windows mean "endpoint answers", not `Path::exists`. CLI commands that
need live IPC (`toggle-pause`, `toggle-auto-enter`, `get-state`,
`models.*`, voice enrollment) go through the pipe like on unix and fall
back to local state only when no daemon is running. The Tauri settings
app (`always-tauri` + `always-gui`) connects through the same pipe —
same protocol, same transport — and holds a persistent client
connection where CLI calls are transient. The orphan watchdog
(no client for 30 s → exit) now runs only when `ALWAYS_SPAWNED_BY_GUI=1`:
a terminal-spawned daemon has no client to wait for and would otherwise
suicide 30 s after every `always start` — on Windows, where clients are
transient CLI calls, that is the only mode there is.

**Nothing is "done" until the new build is running** (I10). "Built" and
"deployed" are different claims, and only "running" is testable.

> ⚠️ Live-mic testing of the dev instance still contends for the single
> hardware microphone — the mic-conflict monitor (§7) will pause one side.
> Test the dev daemon with file/STT paths while production holds the mic,
> or pause production manually first.

---

## 12a. Updates

One updater, inside the `always` daemon binary, on macOS, Linux and Windows.
GitHub Releases of `LivioGama/always` is the only channel. It **notifies; it never installs on its own.**

**U1 — Signed bytes only.** Every byte that gets installed is covered by a
minisign signature that verifies against the public key compiled into the
binary (`resources/update-signing.pub`, `include_str!`). That means the
platform archive/package **and** the release's `install.sh` / `install.ps1`.
Verification is Ed25519 over the BLAKE2b-512 pre-hash (the minisign default);
legacy (non-pre-hashed) signatures are refused. The signature's trusted
comment must bind the file: it carries `file:<asset name>` and `tag:<release
tag>` fields, and both must match what was asked for, so a validly signed
asset of another release or another name cannot be substituted. A missing,
malformed or non-verifying `.minisig` aborts the update: the download is
deleted and nothing is installed. There is no "install anyway" path. A build
whose embedded key does not parse refuses every update (a unit test fails the
build first).

**Check.**
- `GET https://api.github.com/repos/LivioGama/always/releases/latest`. Sends a
  `User-Agent: always/<version>` and `Accept` header, no token, no cookie, no
  identifier of the user or machine. Response capped at 1 MiB.
- When: 15 s after daemon start (so audio/UDS start-up never waits on it;
  skipped, and the stored result replayed, when the last check was under
  1 h ago — a flapping daemon must not burn GitHub's 60 requests/h/IP), then
  every 24 h by wall clock. The last result is kept in
  `<config dir>/update-state.json` (checked-at, latest tag; nothing else).
- Compare: the release tag (`vX.Y.Z`, `v` stripped) against `CARGO_PKG_VERSION`
  with semver ordering. Newer = update available. Equal or older = up to date
  (a downgrade is never offered). Drafts, prereleases and tags that are not
  semver are ignored.
- Failures are logged once at info/debug and are never user-visible errors:
  HTTP 404 (no published release — true until the first release) =
  `no_release`; DNS/connect/timeout = `offline`; HTTP 403/429 with an
  exhausted rate limit = `rate_limited`; anything else = `error`. A failed
  attempt is retried at the next hourly wake-up; a definitive answer
  (`up_to_date`, `update_available`, `no_release`) is not asked again for 24 h.
- Logs (JSON lines, no user data): `update_check_done {result}`,
  `update_available {current, latest}`.
- A **dev instance** (`ALWAYS_INSTANCE` ≠ prod, or running from `Always Dev.app`)
  never checks in the background and refuses to apply.

**Opt-out.** Preference `update_check` (default `true`;
`always config set update_check false`) and env `ALWAYS_UPDATE_CHECK=0`
(`0`/`false`/`no`/`off`) each disable the *background* check; either one is
enough. The preference is re-read on every tick (no restart). Explicit
requests — `always update`, `always update --check`, the `CheckForUpdates` UDS
command — always run: the user asked.

**Notify.** On finding an update the daemon logs `update_available` and
broadcasts `UpdateAvailable` (once per newly seen version per process, and in
the initial state burst of every client that connects while one is known).
`always status` prints one line when a stored result says an update exists.
The GUI shows it; it does not install it.

**Apply** — `always update`, or the `InstallUpdate` UDS command.
`always update --check` only reports; exit code 0 = up to date (including "no
published release"), 10 = update available, 1 = error.
1. Fetch the latest release again (never trust a cached answer to install).
   Not newer than the running build → stop, nothing downloaded.
2. Work out how this binary was installed (below). Unsupported → refuse with a
   message saying what to do instead.
3. Download into a fresh `0700` directory under the user cache dir: the asset
   the installer would use on this platform (`always-<tag>-<target>.tar.gz`,
   `.zip`, `.deb` or `.rpm`) + `.minisig` + `.sha256`, and `install.sh`
   (`install.ps1` on Windows) + `.minisig`. `https` only; the URLs are built
   from the tag, not taken from the API response. The archive streams to disk
   while it is hashed, so the bytes verified are the bytes written.
4. Verify every signature (U1). Any failure deletes the directory.
5. Run the **verified installer** against the **verified local archive**
   (`ALWAYS_VERSION=<tag> ALWAYS_ARCHIVE=<path>`; the installer does not touch
   the network, and still checks the archive against the `.sha256` sibling).
   The installer is the one in the same release, so it can change per release.

Per OS:
- **macOS** — the daemon lives inside the bundle being replaced, so the
  installer is started **detached** (own process group, output to
  `~/Library/Logs/Always/update-install.log`), and the daemon returns. The
  installer quits the running app (AppleScript quit, then terminates what is
  left of that bundle; from `always update` it spares that process, so the
  user keeps the output), swaps
  `Always.app` in place — the directory the running bundle lives in, normally
  `/Applications` — with `ditto`, and relaunches it if it was running. Needs
  the bundle named `Always.app` and its parent directory writable; otherwise
  refused. Ad-hoc signed, not notarized (unchanged).
- **Windows** — `always update` runs `install.ps1` as a child and waits.
  A running `.exe` cannot be overwritten but can be renamed: the installer
  stops the other Always processes in the install dir (never the updater's own
  pid), renames the current `always.exe` to `always.exe.old`, copies the new
  files, and the leftover `.old` is deleted by the next `always update` / daemon
  start. Installs into the directory the running `always.exe` lives in
  (normally `%LOCALAPPDATA%\Programs\Always`). Windows talks to the daemon
  over the `\\.\pipe\always` named pipe (§12); the CLI falls back to
  spawning it when the pipe is absent.
- **Linux, tarball install** (binary at `<prefix>/bin/always`, not owned by a
  package) — replaced in place, then the user service is restarted if it was
  active. Allowed from the CLI and from `InstallUpdate`.
- **Linux, deb or rpm** (binary owned by dpkg/rpm) — the package manager owns
  the files. Only `always update` from a terminal, where the `sudo` prompt
  passes through. `InstallUpdate` is **refused** (`can_apply: "false"`,
  `UpdateFailed { stage: "refused" }`). A binary that is not at
  `<prefix>/bin/always`, or whose `bin` directory is not writable by the user,
  is refused for both (reinstall instead). After a Linux update the running
  daemon is still the old binary: the installer restarts the user service when
  it is active; otherwise `always update` says to restart it.

**Windows global shortcuts.** Registered with `RegisterHotKey` on a dedicated
message-loop thread; the OS matches the full chord, so no raw key stream or OS
permission is needed (the Permissions row reports "active" once registration
succeeds). `ReloadShortcuts` posts a `WM_APP` message to that thread to
unregister and re-register — hotkey changes apply live. Modifier-less keys and
`fn` are not registrable on Windows and are skipped. Because focus tracking
isn't wired on Windows yet, the per-app pause chord falls back to the master
toggle when no focused app is known.

The Settings window learns the shortcut status from the daemon's startup burst,
but its listeners can attach after that burst is sent. It therefore also asks
the daemon (`GetShortcutListenerStatus`) on every connect, and the daemon
re-sends the status it already knows. On Windows, the General banner shows a
"Global Shortcuts" row only while the daemon is connected and hotkeys are not
registered. The Microphone row shows only while the daemon is unreachable; a
connected daemon with a working microphone never produces a warning.

Downloaded files live only in the update directory; stale ones (older than 1 h,
other than a running update's) are removed at daemon start and on
`always update`.

**UDS contract** (protocol version stays 12: additive; every `data` value is a
string). Events: `UpdateAvailable {current, latest, url, can_apply, apply_hint}`,
`UpdateCheckDone {result, current, latest}`, `UpdateProgress {phase, percent,
detail}`, `UpdateFailed {stage, message}`. Commands: `CheckForUpdates`,
`InstallUpdate` (unit variants, no `data`). Field meanings are documented on the
variants in `src/always/event.rs`; wire shape pinned by
`tests/uds_protocol_test.rs`.

**Release signing** (`.github/workflows/release-build.yml`): the `attest` job
signs every archive, package, `install.sh` and `install.ps1` with `minisign`
using repository secret `ALWAYS_MINISIGN_SECRET_KEY` (an unencrypted key; an
optional `ALWAYS_MINISIGN_PASSWORD` unlocks an encrypted one) and uploads each
`<asset>.minisig` next to it; the release publishes them. A branch dry run has
no secret: signing is skipped with a visible notice. The smoke jobs verify
every `.minisig` that exists with the public key from the repo. The signing
step sets the trusted comment to `timestamp:<unix>`, `file:<asset name>` and
`tag:<release tag>`, tab-separated (what U1 checks). Losing the private key
means installed apps can no longer verify future updates; those users would
have to reinstall from the release page.

---

### 10.1 Sound cues

Original "halo" set (`resources/sounds/halo/*.aiff`, generated by
`resources/sounds/gen.py`, provenance in `resources/sounds/README.md`),
embedded in the daemon binary. Played in-process through preloaded
`AVAudioPlayer`s (pool of 3 per cue) — no `afplay` subprocess. A cue is
never dropped because another is playing: different cues overlap, a
retriggered cue takes another pooled player. Gain is baked into the PCM
(low 0.45, medium 0.75, high 2.0). Measured: `cue()` costs ~0 µs on the
caller, play start 11–21 ms. Every cue logs `status_sound_played {cue, level, latency_ms}` or
`status_sound_failed {cue, error}` at info/error level; `PlayStatusCue
{cue}` plays one through the normal path (launch smoke test).

| Cue | File | When |
|---|---|---|
| Listening | start | listening badge raised (first VoiceActivityDetected) |
| Transcribing | stop | first TranscribingStarted |
| Success | done | final delivered (not for consume-mode held finals — silent handoff) |
| Failure | error | transcription failed |
| Cancel | cancel | transcription filtered |
| Command | command | reserved; not played on `DiscardHeld` (handoffs are silent, user request 2026-10-14) |

### 10.2 Voice level

`VoiceLevel { energy, energy_db, voiced, bands[5] }` at ~20 Hz, only for
voiced frames of a verified speaker (or with My Voice off) — never for
room noise, pauses, or media the gate has not accepted — plus one
`voiced: false` frame (energy 0) when the utterance ends. `energy` is the
RMS of the captured (echo-cancelled, when §3 applies) mic since the last
tick as a fraction of full scale; `energy_db` the same in dBFS;
`bands` the RMS share of ≈80-250/250-500/500-1k/1k-2k/2k-4k Hz.

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
   mid-thought. As of 2026-09-25 this instance runs 0.9 s and 0 ms (§4).
   Open question is whether the code defaults should follow.
4. ~~Should a streaming local model become the default so live text works?~~
   **Resolved 2026-08-04:** `moonshine-small-streaming-en` downloaded and made
   active, so the overlay's live-text path has something to render. English
   only — revisit if multilingual dictation matters more than live text.
   As of 2026-09-25 the active backend is Groq again (daemon logs), and live
   text comes from the `stt_live_preview` cloud cadence (§5).
5. Mic conflict discards the interrupted sentence rather than pasting it — right
   call?
6. ~~Nemotron — fix and restore, or leave out?~~ **Resolved 2026-08-15:**
   Nemotron is restored with state-reset, padded 560 ms chunk processing,
   trailing-frame flushes, and intact UTF-8 partial transcript updates.
