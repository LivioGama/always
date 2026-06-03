# Advanced Settings

The Settings window exposes one high-level **Mic Sensitivity** picker
(Low / Normal / High) and an **Advanced** disclosure containing the
underlying numeric thresholds. Every advanced knob is also reachable
from the CLI for power users, scripts, and `~/.dotfiles` setups.

> **TL;DR.** If you only ever change one thing, change **Mic Sensitivity**.
> Everything below tunes how that maps to the daemon internals.

---

## Mic Sensitivity presets

| Preset   | `stt_energy_threshold` | `hear_energy_threshold` | When to use                                              |
|----------|:---:|:---:|----------------------------------------------------------|
| **High** | 0.005 | 0.0005 | Quiet rooms, soft speakers. Picks up faint speech but more likely to false-trigger on noise. |
| **Normal** *(default)* | 0.012 | 0.001 | Typical office / home desk audio. Recommended starting point. |
| **Low**  | 0.025 | 0.002 | Open-plan offices, cafés, mechanical keyboards, fans. Only loud, clear speech triggers transcription. |

Apply from the CLI:

```bash
always config preset high     # quiet room
always config preset normal   # default
always config preset low      # noisy environment
```

The CLI command writes the same two preferences (`stt_energy_threshold`
and `hear_energy_threshold`) that the GUI picker writes — they are fully
interchangeable.

---

## Advanced thresholds

Each entry includes the **Default**, the **valid range**, and **what
moving it does** in plain language.

### `stt_energy_threshold`

| | |
|---|---|
| Default | `0.012` |
| Range   | `0.0001 – 0.5` |
| What    | Normalized RMS energy that an audio frame must exceed for the daemon to consider it speech *and* invoke Whisper. |
| Lower → | More sensitive; quiet speech transcribes; AC hum may also transcribe. |
| Higher → | Less sensitive; only deliberate speech transcribes; whispered commands get ignored. |

```bash
always config set stt_energy_threshold 0.015
```

### `hear_energy_threshold`

| | |
|---|---|
| Default | `0.001` |
| Range   | `0.0001 – 0.5` |
| What    | Pre-VAD "did I hear *something*?" hint used to flash the overlay early, before Silero confirms speech. Lower than `stt_energy_threshold` by design. |
| Lower → | Faster overlay reaction to faint sound. |
| Higher → | Overlay only flashes when the room is genuinely loud. |

```bash
always config set hear_energy_threshold 0.0008
```

### `stt_silence`

| | |
|---|---|
| Default | `2.0` (seconds) |
| Range   | `0.1 – 10.0` |
| What    | How many seconds of silence end an utterance. Whisper is invoked once silence persists this long. |
| Lower → | Snappier; transcription fires sooner; may cut off natural pauses. |
| Higher → | Tolerates long mid-sentence pauses; user feels the "lag" before paste. |

`2.0` is deliberate: humans pause mid-sentence. Going below ~`0.7` will
chop trains of thought into fragments.

```bash
always config set stt_silence 2.0
```

### `stt_cooldown_ms`

| | |
|---|---|
| Default | `150` (milliseconds) |
| Range   | `0 – 60_000` |
| What    | Minimum time between two consecutive paste actions. Prevents the daemon from double-firing on a single VAD edge. |
| Lower → | Faster back-to-back commands; risk of duplicate paste on noisy edges. |
| Higher → | Safer debounce; first-pass-of-many feels sluggish. |

```bash
always config set stt_cooldown_ms 150
```

### `silero_threshold`

| | |
|---|---|
| Default | `0.5` |
| Range   | `0.0 – 1.0` |
| What    | Silero VAD probability cut-off for "this frame contains speech." |
| Lower → | More frames classified as speech (false positives ↑, missed speech ↓). |
| Higher → | Stricter; less noise leaks through; quiet/short utterances may be missed. |

`0.5` is the canonical Silero default. Don't touch unless you know what
Silero is.

```bash
always config set silero_threshold 0.5
```

---

## CLI cheat sheet

```bash
# Inspect current values
always config show

# Apply a preset (sets the two energy thresholds atomically)
always config preset normal

# Tweak one knob
always config set stt_silence 1.5

# Roll everything back to recommended defaults
always config reset
```

Other CLI commands worth knowing:

| Command | Purpose |
|---------|---------|
| `always logs --pretty` | Live-tail the daemon log with emoji decoration |
| `always status` | Is the daemon running? PID, log path |
| `always toggle pause` | Mute / unmute mic without killing the daemon |
| `always toggle auto-enter` | Toggle the "press Return after paste" feature |

---

## When to lower vs. raise

A quick troubleshooting tree:

| Symptom | Likely fix |
|---------|------------|
| Daemon transcribes the AC, my keyboard, my dog | Preset → **Low**, or raise `stt_energy_threshold` |
| Quiet voice / whispered commands ignored | Preset → **High**, or lower `stt_energy_threshold` |
| Sentences get cut off mid-thought | Raise `stt_silence` |
| Lag between speaking and paste feels too long | Lower `stt_silence` |
| One utterance pasted twice | Raise `stt_cooldown_ms` |
| Random YouTube-style "thanks for watching" hallucinations | Already filtered by [`hallucination`](../src/always/hallucination.rs); if persistent, raise `silero_threshold` to `0.6` |

---

## Vocabulary auto-import — what actually works

`always vocab import` scans installed sources and writes the union of
detected terms into `~/.always/glossary.json`. The hardcoded "starter
pack" word lists that polluted earlier versions have been removed —
every term that lands in your glossary now comes from real user data.

| Source | Detection | Reads user vocab? | What gets imported |
|--------|:---:|:---:|---|
| **macOS Text Replacements** | ✅ always on macOS | ✅ **real** | Reads `NSGlobalDomain → NSUserDictionaryReplacementItems` via `defaults export` + `plutil -extract … json`. Imports the **`with`** value of every entry (the canonical phrase the user wants typed). UTF-8 / curly quotes / emojis / accents preserved. |
| **SuperWhisper** | ✅ checks `/Applications/superwhisper.app` | ✅ **real** | Opens `~/Library/Application Support/superwhisper/database/superwhisper.sqlite` read-only and runs Z-score statistical outlier extraction on the `recording_fts.result` corpus (capped at 5000 most-recent transcripts). Captures the technical terms you've actually been dictating. |
| **MacWhisper** | ✅ checks `/Applications/MacWhisper.app` | ✅ **real** (when present) | Reads the user's "Replacements" list from `~/Library/Containers/com.dipped.MacWhisper/Data/…/replacements.json` (or the matching `.plist`). Imports the `replace`/`to`/`with` value of each entry. |
| **Wispr Flow** | ✅ checks `/Applications/Wispr Flow.app` | ❌ cloud-hosted | Detection only. Wispr Flow stores the user's custom dictionary on their servers; we won't authenticate against their cloud to scrape it. |
| **Dragon NaturallySpeaking** | ✅ checks `/Applications/Dragon` | ⚠️ legacy | Returns a small list of common IT terms (Dragon's `.dvc` vocabulary files are not yet parsed). Kept for back-compat. |
| **Installed apps** | ✅ macOS / Linux | ✅ **real** | Every installed `.app` / binary name in `/Applications` and `~/Applications` becomes a glossary entry — so you can paste app names verbatim ("Linear", "Notion", "Cursor"). |

Sources that **used to be listed** and have been removed because their
"extraction" was a hardcoded generic-vocabulary list with no real user
data: Google Speech, Windows Speech Recognition, OpenAI Whisper CLI,
generic macOS Dictation (replaced by the Text Replacements plugin
above).

```bash
$ always vocab import
Scanning for installed speech-to-text software...
Active plugins: macOS Text Replacements, Superwhisper
Importing vocabulary...
Imported 93 unique vocabulary terms into ~/.always/glossary.json
```

The glossary file is plain JSON — see the README's **Custom Vocabulary**
section for the schema. Re-running `always vocab import` **merges**
imported terms with the existing file: new terms are added, existing
entries are kept untouched (so hand-tuned `mistranscriptions` /
`frequency` values survive).

If a plugin you'd find useful is missing, `src/always/vocab/plugins.rs`
is the right place to add one — the trait surface is documented at the
top of that file.

---

## Config storage

All preferences live in a SQLite file at:

| File | Path |
|------|------|
| Preferences (macOS)   | `~/Library/Application Support/always/config.sqlite` |
| Preferences (Linux)   | `$XDG_CONFIG_HOME/always/config.sqlite` (default `~/.config/always/`) |
| **Glossary** (every platform) | **`~/.always/glossary.json`** |
| Logs (macOS)          | `~/Library/Logs/Always/always.YYYY-MM-DD` |
| Logs (Linux)          | `$XDG_STATE_HOME/always/always.YYYY-MM-DD` |
| UDS socket (macOS)    | `~/Library/Caches/Always/always.sock` |
| UDS socket (Linux)    | `$XDG_RUNTIME_DIR/always.sock` (default `/tmp/always.sock`) |

The glossary path can be overridden with `ALWAYS_GLOSSARY_PATH=...`
for tests/CI. The Settings panel's "Open glossary.json" + "Show in
Finder" buttons resolve to the same canonical location, and
`always vocab import` writes there too.

API keys are stored in the OS Keychain (`security find-generic-password
-s com.always.daemon`), never in SQLite or env vars in plain text.

The daemon hot-reloads `stt_silence`, `stt_cooldown_ms`, and the energy
thresholds at the start of each utterance. `silero_threshold` is read at
daemon startup; restart the daemon (`always stop && always start`) for
that one to take effect.
