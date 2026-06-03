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
| Default | `1.5` (seconds) |
| Range   | `0.1 – 10.0` |
| What    | How many seconds of silence end an utterance. Whisper is invoked once silence persists this long. |
| Lower → | Snappier; transcription fires sooner; may cut off natural pauses. |
| Higher → | Tolerates long mid-sentence pauses; user feels the "lag" before paste. |

`1.5 s` balances snappiness against natural mid-sentence pauses.
Speculative transcription kicks off at 75 % of this (≈ 1.1 s) so the
Whisper round-trip happens in parallel with the silence wait —
end-to-end paste latency rarely exceeds ~1.5 s from when the user
stops talking. Going below ~`0.7` chops trains of thought into
fragments.

```bash
always config set stt_silence 1.5
```

### `stt_cooldown_ms`

| | |
|---|---|
| Default | `800` (milliseconds) |
| Range   | `0 – 60_000` |
| What    | Minimum time between two consecutive paste actions. Prevents the daemon from double-firing on a single VAD edge. Built-in dedupe also suppresses identical/near-identical pastes for at least 3 seconds regardless of this setting. |
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
| `always toggle-pause` | Mute / unmute mic without killing the daemon |
| `always toggle-auto-enter` | Toggle the "press Return after paste" feature |

---

## When to lower vs. raise

A quick troubleshooting tree:

| Symptom | Likely fix |
|---------|------------|
| Daemon transcribes the AC, my keyboard, my dog | Preset → **Low**, or raise `stt_energy_threshold` |
| Quiet voice / whispered commands ignored | Preset → **High**, or lower `stt_energy_threshold` |
| Sentences get cut off mid-thought | Raise `stt_silence` |
| Lag between speaking and paste feels too long | Lower `stt_silence` |
| One utterance pasted twice | Usually auto-suppressed (3s near-duplicate window). If it persists, raise `stt_cooldown_ms` or check for duplicate daemon processes (`ps aux \| grep always`) |
| Random YouTube-style "thanks for watching" hallucinations | Already filtered by [`hallucination`](../src/always/hallucination.rs); if persistent, raise `silero_threshold` to `0.6` |

---

## Manual correction capture

Whisper isn't perfect: it'll mistranscribe domain jargon ("kubernetics"
for "kubernetes", "doker" for "Docker", your colleague's name as
nonsense). Always lets you teach it without leaving the keyboard. Two
paths feed `~/.always/glossary.json`'s `mistranscriptions` lists, and
both share the same on-disk effect.

### Active hotkey — `⌃⌥X`

The fast path. Workflow:

1. Dictate a sentence. Always pastes it at the cursor.
2. The paste is wrong — fix the bad word(s) inline, in whatever app you're
   in.
3. Select the corrected text.
4. Press `⌃⌥X`.

The daemon snapshots your selection (via simulated `Cmd+C`), diffs it
word-by-word against the most recently pasted transcript, extracts the
`(wrong → right)` pairs that look like real corrections (length ratio +
Levenshtein gate — typo distance only, not unrelated rewrites), and
appends them to `~/.always/glossary.json`.

**Example.** Whisper transcribed:

> deploy the kubernetics cluster to production

You corrected it inline to "kubernetes" and selected the line. After
`⌃⌥X`, your glossary now contains:

```json
[
  {
    "term": "kubernetes",
    "mistranscriptions": ["kubernetics"],
    "frequency": 100
  }
]
```

If `kubernetes` was already in the glossary as a curated entry,
`kubernetics` is appended to its existing `mistranscriptions` list and
nothing else about the entry is touched — `frequency`, casing, other
mistranscriptions all survive untouched.

Re-bind the hotkey:

```bash
always config set shortcut_log_correction "ctrl+alt+x"
```

### Passive clipboard watcher — opt-in

The careful path. Some users prefer "the daemon notices when I re-copy
my correction" over "I have to remember a hotkey." The watcher polls the
system clipboard every 250 ms; when you re-copy a string that closely
resembles the most recently pasted transcript, the diff is queued at
`~/.always/pending_corrections.json` for you to approve or reject from
the menu bar (or the CLI).

Triggers:

* You re-copy a string within 60 seconds of the paste it's a candidate
  correction for. After 60 s, the paste is forgotten — the watcher
  assumes you've moved on.
* The full-string normalized Levenshtein similarity is ≥ 0.55, and a
  per-word similarity gate clears for at least one substitution. Below
  that, the re-copy is treated as unrelated content and ignored.

Stale entries are pruned after 7 days. The queue is capped at 100
pending items; past that, the oldest unreviewed entry is evicted on
insert.

Opt in:

```bash
always config set passive_correction_capture true
```

The watcher is **off by default** — it's a probabilistic match against
your real clipboard, and silently mutating user data is the wrong
default.

### CLI

```bash
$ always corrections list
No pending corrections.
```

When entries are pending, the listing is one row per item with the full
UUID, source, age, and the diff:

```
ID                                     Source     Queued         Wrong → Right
9d3e4c5e-…full UUID printed…           passive    2min           kubernetics → kubernetes
1f8a0b2c-…full UUID printed…           hotkey     just now       supr whisper → SuperWhisper
```

```bash
always corrections approve <UUID>      # apply the pair to glossary, drop from queue
always corrections reject  <UUID>      # drop from queue without applying
always corrections clear               # drop everything (prints count first)
always corrections capture             # run the active capture flow without the hotkey
```

`approve`/`reject` exit non-zero on an unknown UUID so wrapping shell
scripts can detect a stale reference (e.g. an entry already actioned
from another window).

`capture` is the manual equivalent of `⌃⌥X` — useful when the hotkey
is shadowed by another app, or for scripting. It uses the same 60-second
"recent paste" window.

### How merging works

Both paths land at `correction::apply_pairs_to_glossary`, which is the
single place glossary writes happen:

* If a glossary entry already exists with `term == pair.right`, the
  `wrong` token is appended to that entry's `mistranscriptions` list
  (deduped against what's already there). **`frequency` and other
  fields are not touched.**
* Otherwise, a new entry `{"term": right, "mistranscriptions": [wrong],
  "frequency": 100}` is created.

In other words: curated glossary entries you've hand-tuned will never
be overwritten — only their `mistranscriptions` arrays are extended.

### Privacy

* **The passive watcher polls the clipboard** via `pbpaste` every
  250 ms. It does not keylog and it does not read individual keystrokes.
* **The active hotkey simulates `Cmd+C`** in the foreground app, reads
  the new pasteboard contents, and then restores the previous clipboard
  value (best-effort). Clipboard-history apps like Paste or Maccy may
  still see the temporary copy event during that ~100 ms window — that's
  inherent to how `Cmd+C` works on macOS, not something Always can
  suppress.
* **Nothing is sent over the network.** All correction data lives on
  your laptop in `~/.always/`.

### When to use which

| | **Active hotkey (`⌃⌥X`)** | **Passive watcher** |
|---|---|---|
| Trigger | Explicit keypress | Re-copy after a paste |
| Optimizes for | Recall — captures every correction the user makes | Precision — captures only what's confidently a correction |
| User effort | Press one shortcut | None (just copy as you would anyway) |
| False positives | Almost zero — you choose what to record | Possible — gated to ≥ 0.55 string similarity + 60 s window |
| Default | On (hotkey is bound) | Off (`passive_correction_capture = false`) |
| Writes glossary directly | Yes | No — queues for review |

If you're curating a project glossary aggressively, leave the watcher
off and use the hotkey deliberately. If you mostly trust the diff and
want a low-effort feedback loop, turn on passive capture and skim
`always corrections list` once a day.

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

The daemon hot-reloads `stt_silence`, `stt_cooldown_ms`,
`stt_energy_threshold`, `hear_energy_threshold`, and `silero_threshold`
through the Settings app without a daemon restart.
