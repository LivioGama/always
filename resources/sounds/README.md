# Always UI sounds

Original, synthesised status cues for Always (`~/proj/always`), plus open-source sets for comparison.

- **Lead family:** `halo/`. Alternates: `glass/`, `wood/`, `airy/`. Comparison sets: `sourced/` (see `sourced/INDEX.md`).
- **Format:** AIFF, 48 kHz, 24-bit PCM, stereo. This matches the `/System/Library/Sounds/*.aiff` files Always plays today.
- **Listen:** `./preview.sh [volume] [set]`. It plays the lead family, then the alternates, then the sourced sets, each labelled with its licence. The default volume is 0.75, which matches Always' "medium" setting.
- **Regenerate:** `python3 gen.py` writes all families next to the script. `python3 gen.py <outdir> halo` writes one family. It needs python3 with numpy, and ffmpeg. Output is deterministic (seeded RNG).

## 1. What Always does today

The code is in `src/always/status_sound.rs`, and the call sites are in `src/always/event.rs`. Playback runs `/usr/bin/afplay -v <vol> <path>` on a spawned thread, at most one cue at a time (`PLAYBACK_IN_FLIGHT` drops any cue that overlaps). Cues are off by default. The `audible_status_sound` setting takes off, low (0.45), medium (0.75) or high (2.0).

| `StatusSound` | Fires in (`event.rs`) | Current file | Dur | Peak | Max momentary |
|---|---|---|---|---|---|
| Listening | `voice_activity_detected()` (first VAD edge) | `/System/Library/Sounds/Pop.aiff` | 1627 ms (208 ms audible) | -11.1 dBFS | -29.0 LUFS |
| Transcribing | `transcribing_started()` (first edge) | `/System/Library/Sounds/Tink.aiff` | 564 ms (36 ms audible) | -8.8 dBFS | -24.7 LUFS |
| Success | `transcript_final()` | `/System/Library/Sounds/Ping.aiff` | 1502 ms (283 ms audible) | -10.6 dBFS | -26.2 LUFS |
| Failure | `transcription_failed()` | `/System/Library/Sounds/Basso.aiff` | 768 ms (161 ms audible) | -11.2 dBFS | -26.5 LUFS |

All four are pcm_s24be, 48 kHz, stereo. `notification.rs` also uses the system "Ping" through osascript for pause and auto-enter notifications.

There is no cue for cancel, filtered/empty result, or "Iris claimed this as a command". Those events (`transcription_filtered`, etc.) emit no sound today.

### Mapping for the wiring agent

| New file | Use for |
|---|---|
| `start.aiff` | `StatusSound::Listening` |
| `stop.aiff` | `StatusSound::Transcribing` |
| `done.aiff` | `StatusSound::Success` |
| `error.aiff` | `StatusSound::Failure` |
| `cancel.aiff` | New: user cancel / dictation discarded. It also fits `transcription_filtered`. |
| `command.aiff` | New: Iris claimed the utterance as a command (not pasted) |

## 2. Reference: what makes the Superwhisper sounds feel good

These were analysed only, never copied. The source was the official Homebrew cask zip, v2.19.2, with sha256 matching the cask. Only the `.m4a` files were extracted, into `../sw-ref-DO-NOT-SHIP/`, which sits outside this folder. Measurements came from `tools/analyze.py` (envelope, STFT peaks and stereo correlation as a reading aid) and `ffmpeg ebur128`.

| SW file | Content | Audible (-40 dB) | Peak | Max M |
|---|---|---|---|---|
| Start | single near-pure sine G5 (785 Hz); 2nd harmonic -29 dB; ~10 ms attack, ~60 ms plateau, exponential decay ≈0.12 dB/ms; mono | 354 ms | -27.5 dBFS | -35.5 LUFS |
| Stop | single sine B5 (987 Hz), same envelope, mono | 315 ms | -29.2 | -38.0 |
| Notification | G5 then B5 at +240 ms (rising major third) | 457 ms | -27.8 | -36.5 |
| NotificationError | G3 harmonic tone (1,2,3,5 partials), then B3 at +120 ms; low and soft, not buzzy | 465 ms | -24.5 | -35.7 |
| noResult1 | ~100 Hz thud plus noise | 560 ms | -15.8 | -31.6 |

Why they work:
- **Almost pure tone.** There is nothing harsh to fatigue on.
- **Upper-mid register (780–990 Hz).** This is clear over speech without being shrill.
- **Same timbre and envelope everywhere.** The set reads as one family.
- **No click.** There is a short soft attack and a smooth exponential tail, and nothing below roughly 350 ms of audible length.
- **Very quiet master.** The max momentary level is about -36 LUFS, roughly 8–10 dB below Always' current system sounds.
- **Error is a lower octave of the same idea.** It differs by register and richness, not by harshness.

Their weak spots, which this set aims to beat:
- 26.6 ms of leading silence in Start and Stop (latency).
- Mono and a single partial, so they sound a little "test-tone".
- Start and Stop are both single notes, so their direction is ambiguous. Stop is actually *higher* than Start.

Guidance consulted:
- Apple WWDC23 "Explore immersive sound design": "a good UI sound has to be subtle and give just enough feedback to be helpful"; sounds heard often should stay subtle. https://developer.apple.com/videos/play/wwdc2023-10271
- Apple HIG "Playing audio": system volume governs app sound effects; keep non-essential sounds respectful. https://developer.apple.com/design/human-interface-guidelines/playing-audio
- Material Design "Applying sound to UI" / "About sound": earcons for state changes, consistency, decorative sound used sparingly, avoid sound for very frequent actions. https://m2.material.io/design/sound/applying-sound-to-ui.html, https://m2.material.io/design/sound/about-sound.html

## 3. Lead family `halo`: design

- **Key of D major** (D5 = 587 Hz tonic). This is deliberately not Superwhisper's G/B.
- **start** asks a question: D5 then A5 at +45 ms, a rising perfect fifth.
- **stop** is the answer: A5 then D5, a falling fifth that resolves to the tonic.
- **done**: a D5 / F♯5 / A5 arpeggio, 38 ms per step, with a faint D6 halo.
- **error** is gentle: F4 then D4, a falling minor third, in a low register with warmer harmonics (2nd harmonic at -14 dB). It has no dissonance and no buzz.
- **cancel**: one A5 that droops about a semitone into place, then fades fast. It is the shortest cue (209 ms audible).
- **command** (Iris claimed it) has its own identity: FM timbre (ratio 2, decaying index), an A5+E6 dyad then an E6 re-strike. Its centroid is 1.3–1.9 kHz, against 0.6–0.9 kHz for the dictation cues, so it reads as "something else took this".

Each sound has three layers:
1. A 3 ms band-passed noise tick at -30 dB, for touch.
2. A tonal body: fundamental plus 2nd/3rd/4th partials at -24/-36/-40 dB. Each partial is an in-tune centre with a -9 dB voice detuned ±4 cents L/R, which gives warmth and gentle width.
3. An "air" tail: convolution with decorrelated, exponentially decaying noise (32 ms, wet 0.13).

There is no leading silence: the attack starts on sample 0 with a 3 ms raised cosine.

### Refinement rounds

Earlier rounds are kept in `rounds/r1..r5/halo/`, rendered with the code of each round. `gen.py` holds the final code, with R-comments marking each change.

| Round | Measured problem | Change |
|---|---|---|
| R1 | Baseline. L/R correlation 0.32–0.78: too diffuse. `cancel` envelope dipped to -49 then rose again (detuned L/R pair beat-nulling when summed to mono). Decay ≈0.18 dB/ms, faster than the reference's 0.12. | none |
| R2 | | Stereo topology changed to an in-tune centre in both channels plus a -9 dB detuned chorus voice. tau 70→90 ms, hold 8→15 ms. Cancel uses a downward glide. **Result:** correlation 0.93–0.96, decay slope matches the reference. |
| R3 | Command still at 0.56 correlation (the FM path used the old topology). Cancel still warbled. Hard truncation at the end of voice buffers (start.aiff tail jumped -64 → -81 dB). | FM uses the same topology, overtones follow the glide, 12 ms end fade per voice. **Result:** command correlation 0.92. Cancel still warbled. |
| R4 | Cancel warble isolated: the voice alone was smooth, so the cause was the air tail. Narrowband tone through a noise IR fluctuates once the tail decay (55 ms) rivals the note decay (55 ms). | Air 55→32 ms, wet 0.16→0.13. Rule for all families: air_ms ≲ 0.6·tau. Voice buffers auto-size to 7·tau. **Result:** every envelope monotonic. |
| R5 | Start/stop audible length ~470 ms against the reference's ~350. The start cue fires on VAD while the mic is recording, so a long tail risks bleeding into the take. | tau 90→72, hold 15→10, note gap 55→45 ms, file trimmed at -54 dB rel. Error tau 60/80→50/62. **Result:** start 354 ms, stop 343 ms (reference 354/315). |

## 4. Measured stats (final files)

Command: `python3 tools/analyze.py --brief <file>`. Regenerating with `gen.py` gives identical PCM: all 24 files matched by md5. Lead silence is the time to the first sample above -60 dBFS. Audible length is measured down to -40 dB relative to peak. Max M is the EBU R128 max momentary loudness, padded.

| family/event | file ms | lead silence ms | audible ms | peak dBFS | RMS dBFS | L/R corr | Max M LUFS |
|---|---|---|---|---|---|---|---|
| halo/start | 521 | 0.23 | 354 | -19.6 | -34.0 | 0.966 | -30.0 |
| halo/stop | 474 | 0.29 | 343 | -18.2 | -33.3 | 0.964 | -30.0 |
| halo/done | 545 | 0.25 | 382 | -18.9 | -34.1 | 0.958 | -30.0 |
| halo/error | 496 | 0.17 | 397 | -20.6 | -33.2 | 0.992 | -30.0 |
| halo/cancel | 319 | 0.10 | 209 | -18.4 | -32.0 | 0.951 | -30.0 |
| halo/command | 490 | 0.17 | 365 | -19.0 | -34.4 | 0.948 | -30.0 |
| glass/start | 556 | 0.10 | 431 | -18.6 | -35.3 | 0.958 | -30.0 |
| glass/stop | 595 | 0.19 | 430 | -20.5 | -35.3 | 0.958 | -30.0 |
| glass/done | 590 | 0.12 | 478 | -18.9 | -35.6 | 0.957 | -30.0 |
| glass/error | 594 | 0.08 | 460 | -20.6 | -34.2 | 0.971 | -30.0 |
| glass/cancel | 273 | 0.08 | 199 | -18.1 | -31.0 | 0.975 | -30.0 |
| glass/command | 546 | 0.06 | 387 | -19.5 | -35.9 | 0.923 | -30.0 |
| wood/start | 406 | 0.15 | 312 | -17.9 | -33.2 | 0.987 | -30.0 |
| wood/stop | 407 | 0.10 | 314 | -17.8 | -32.8 | 0.992 | -30.0 |
| wood/done | 428 | 0.15 | 340 | -18.2 | -33.3 | 0.996 | -30.0 |
| wood/error | 491 | 0.10 | 387 | -19.4 | -33.2 | 0.995 | -30.0 |
| wood/cancel | 221 | 0.08 | 159 | -15.4 | -29.8 | 0.988 | -30.0 |
| wood/command | 318 | 0.08 | 242 | -17.3 | -32.8 | 0.984 | -30.0 |
| airy/start | 639 | 0.83 | 436 | -18.4 | -35.5 | 0.716 | -30.0 |
| airy/stop | 649 | 1.00 | 521 | -19.6 | -34.9 | 0.908 | -30.0 |
| airy/done | 733 | 0.92 | 498 | -19.8 | -36.0 | 0.766 | -30.0 |
| airy/error | 684 | 1.44 | 513 | -21.9 | -34.9 | 0.895 | -30.0 |
| airy/cancel | 374 | 0.60 | 282 | -20.3 | -32.9 | 0.936 | -30.0 |
| airy/command | 669 | 0.73 | 477 | -21.3 | -36.6 | 0.732 | -30.0 |

Checks on all 24 files:
- Leading silence is at most 1.44 ms (the bar was under 2 ms).
- No clipping: the maximum peak is -15.4 dBFS.
- Every file decodes with `afplay` (24/24).

### Loudness

Every file is mastered to **-30 LUFS max momentary**, with peaks capped at -12 dBFS. The original brief suggested -24 to -18 LUFS, but the measured evidence points lower:
- The Superwhisper reference sits around -36 LUFS.
- Always' current system sounds sit at -25 to -29 LUFS.

-30 LUFS is between the two. At Always' "high" setting (`afplay -v 2.0`, +6 dB), peaks reach about -9 dBFS with no clipping. To change it, edit `TARGET_MAX_M` in `gen.py` and re-run.

## 5. Sourced sets (comparison)

Details are in `sourced/INDEX.md`, and each folder has an `ATTRIBUTION.txt`. Most sourced files have 5–50 ms of leading silence and would need trimming. Many are mastered far hotter than Superwhisper.

| Set | Licence | Use |
|---|---|---|
| material-design-google | CC-BY 4.0 (credit Google) | ✅ usable. Quiet, dark. `state-change_confirm-up/down` have a slow 136 ms swell, 8–15 ms lead silence and ~500 ms audible length. |
| handy (cjpais/Handy) | MIT repo-level; audio authorship undocumented | ⚠️ marimba start/stop are warm but very hot (max M -7.8 / -12.9 LUFS) with 560–776 ms tails |
| crisp-ui-sounds-free | CC-BY 4.0 (new repo, AI-assisted procedural) | ✅ usable. pop_up/down are OK; "done" sounds are too long |
| kenney-interface-sounds / kenney-ui-audio | CC0 | ✅ usable but bright (2–5 kHz centroid), instant attack, peaks at 0 dBFS |
| openwhispr-rendered | MIT recipe (code-generated cues, rendered locally) | ✅ but plain sine |
| ping-island / codeisland | Apache-2.0 / MIT repo-level | ⚠️ chiptune style mismatch |
| VoiceInk, FluidVoice | GPL-3.0 | ❌ reference only |
| Whispering | AGPL repo; ZapSplat / Sound Ex Machina assets | ❌ reference only |

## 6. Ranking (all sets)

This ranking comes from measurements against the Superwhisper reference profile, not from listening (see the caveat below).

1. **halo** (original). Matches the reference envelope and length. Has an explicit question/answer pitch contour, gentle width, 0.1–0.3 ms lead silence, a distinct command cue and a soft error.
2. **glass** (original). Same qualities, brighter and bell-like; tails are ~80 ms longer.
3. **wood** (original). Shortest and most "tactile". The modal marimba is drier, almost mono.
4. **material-design-google** (CC-BY). The best licence-clean third-party set. It needs lead trimming, and its swells are slower than SW.
5. **airy** (original). Lush but long (436–521 ms) and wide (correlation 0.72), so it collapses less cleanly to mono.
6. handy marimba (needs -15 dB and has unclear provenance), then crisp-ui, kenney, openwhispr, ping-island/codeisland.
7. Reference only: VoiceInk, FluidVoice, Whispering.

**Caveat:** no one has listened to any of these sounds yet. They were judged by measurement only, so audition with `./preview.sh` before wiring them in.

## Revision 2026-10-14: softer, shorter `done`

The user disliked the original `halo/done` (D5/F#5/A5 arpeggio + D6 halo,
382 ms audible, -30 LUFS max momentary). `gen_done.py` renders three
variants with `gen.py`'s own renderer, mastered 3 dB softer (-33 LUFS):

| variant | design | audible ms | attack→peak ms | max M LUFS |
|---|---|---|---|---|
| done_a | F#5 → A5 rising third | 287 | 44 | -33.0 |
| done_b | single A5 + faint D6 shimmer | 238 | 10 | -33.0 |
| **done_c** (shipped) | D5/F#5/A5 arpeggio at 25 ms steps, no D6, tau 50-58 | 267 | 54 | -33.0 |
| original | D5/F#5/A5 at 38 ms + D6 halo | 382 | 92 | -30.0 |

Chosen: **done_c** — 30 % shorter and 3 dB softer than the original, keeps
the three-note "resolved" identity, so it stays distinct from start/stop
(two-note fifths, which done_a resembles) and from cancel (a single A5,
which done_b resembles). Regenerate: `python3 gen_done.py <always-sounds dir> <outdir>`.
