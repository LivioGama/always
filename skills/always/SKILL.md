---
name: always
description: Manipulate the configuration and custom vocabulary of the Always voice activation daemon. Use when the user wants to view or change Always preferences (API keys, energy thresholds, silence timeouts, auto-enter behavior), add domain-specific terms to the glossary so Whisper transcribes them correctly, or import vocabulary from existing STT software.
---

# Always — Configuration & Vocabulary Skill

Always is a voice activation daemon that transcribes speech via Groq's Whisper API. Two
things make it usable day-to-day:

1. **Preferences** — stored in a SQLite DB managed by the `always config` subcommand.
2. **Glossary** — a `glossary.json` file at the project root that biases Whisper toward
   domain-specific terms and tells the LLM post-processor how to fix common
   mistranscriptions.

This skill covers both.

## When to use this skill

- The user asks to set, view, or reset an Always preference (API key, threshold, etc.).
- The user wants Whisper to recognize a specific term ("add `Kubernetes` to the
  vocabulary", "Whisper keeps hearing 'cuber netties' instead of `Kubernetes`").
- The user wants to import vocabulary from another STT tool already on the system.

## Configuration — `always config`

```bash
always config show                                    # Show all current preferences
always config set <key> <value>                       # Set one preference
always config reset                                    # Reset all preferences to defaults
```

**Known keys** (check `always config show` for the authoritative list):

| Key | Type | Purpose |
|---|---|---|
| `groq_api_key` | string | Groq API key (required for transcription) |
| `deepgram_api_key` | string | Optional Deepgram fallback key |
| `stt_energy_threshold` | float | Mic energy gate for STT (lower = more sensitive, default `0.05`) |
| `hear_energy_threshold` | float | Mic energy gate for the always-listening loop (default `0.002`) |
| `stt_cooldown_ms` | int | Cooldown between transcriptions, ms (default `1500`) |
| `stt_silence` | float | Silence timeout to end a phrase, seconds (default `2.0`) |
| `stt_auto_enter` | bool | Press Enter automatically after pasting (default `false`) |
| `always_log_path` | string | Custom log path |

**Examples:**

```bash
always config set groq_api_key "gsk_..."
always config set stt_silence 0.4
always config set stt_auto_enter true
```

## Vocabulary — `glossary.json`

Located at the project root (or set `ALWAYS_GLOSSARY_PATH` to override). Each entry:

```json
{
  "term": "Kubernetes",
  "mistranscriptions": ["cuber netties", "kubernetics"],
  "frequency": 100
}
```

- `term` *(string, required)* — canonical spelling.
- `mistranscriptions` *(array of strings, optional)* — common phonetic misreads;
  the LLM post-processor will rewrite these to `term`.
- `frequency` *(int, optional, default 100)* — higher values get priority in
  Whisper's 224-token bias prompt.

### Adding a term safely

Use `jq` so the file stays valid JSON. Always write to a temp file and move:

```bash
jq '. += [{"term":"Kubernetes","mistranscriptions":["cuber netties"],"frequency":100}]' \
  glossary.json > glossary.json.tmp && mv glossary.json.tmp glossary.json
```

If the term already exists, update it instead of duplicating:

```bash
jq '(map(select(.term == "Kubernetes")) | length) as $n
    | if $n > 0
      then map(if .term == "Kubernetes"
              then .mistranscriptions += ["cuber netties"] | .mistranscriptions |= unique
              else . end)
      else . += [{"term":"Kubernetes","mistranscriptions":["cuber netties"],"frequency":100}]
      end' \
  glossary.json > glossary.json.tmp && mv glossary.json.tmp glossary.json
```

### Removing a term

```bash
jq 'map(select(.term != "Kubernetes"))' \
  glossary.json > glossary.json.tmp && mv glossary.json.tmp glossary.json
```

### Listing terms

```bash
jq -r '.[].term' glossary.json
```

### Importing from installed STT software

```bash
always vocab import
```

This scans for Dragon NaturallySpeaking, macOS Dictation, Windows Speech Recognition,
Google Speech Recognition, and OpenAI Whisper, then imports their vocabularies.

## Token budget

Whisper's `prompt` field has a 224-token hard limit. Always uses ~120 words to leave
headroom. Entries are sorted by `frequency` descending — high-frequency terms make it
into the bias prompt; low-frequency ones still help the LLM post-processor.

## After changing the glossary

The glossary is loaded once per daemon run via `OnceLock`. Restart the daemon for
changes to take effect:

```bash
always stop && always start
```
