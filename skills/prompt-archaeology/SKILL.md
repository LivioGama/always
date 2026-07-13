---
name: prompt-archaeology
description: Recover lost prompts, spoken ideas, and partially submitted instructions from local AI session logs, voice transcription logs, app-focus telemetry, prompt-history files, and pasted-text artifacts. Use when the user asks to find a prompt they remember saying or typing, especially with fuzzy keywords, approximate dates, Always/Superwhisper/voice dictation, Codex/Claude/Devin sessions, or questions like "where did my prompt/code go?"
---

# Prompt Archaeology

## Overview

Reconstruct missing prompts by treating remembered phrases as anchors, then expanding through nearby timestamps, focused apps, pasted text, and AI session logs. Prefer evidence over recall: report exact files, lines, timestamps, and whether the prompt was spoken, pasted, or actually submitted.

## Workflow

1. Start with the user's strongest anchor words.
   - Search exact variants first: original wording, singular/plural, grammar-corrected spelling, and likely transcription mistakes.
   - For voice prompts, search concept clusters too: related nouns, tools, model names, provider names, and distinctive phrases.

2. Bound the time window.
   - Use the user's date memory if available.
   - Search by month/day first rather than global broad searches.
   - If a broad search is needed, exclude noisy directories such as `node_modules`, browser caches, and generated build outputs.

3. Search in layers.
   - AI transcripts: `~/.codex/sessions`, `~/.claude`, Devin local transcripts, project `.jsonl` logs, prompt-history state files.
   - Voice logs: `~/Library/Logs/Always/always.YYYY-MM-DD`, `~/Library/Application Support/superwhisper`, `~/Library/Application Support/vox`.
   - Draft/paste artifacts: Codex attachments, `pasted-text.txt`, editor histories, prompt drafts.
   - App telemetry: Always `uds_focused_app_changed`, `transcription_received`, `transcription_pasted`, and submit/abort events.

4. When an anchor hits in a voice log, reconstruct the speech window.
   - Extract all `transcription_received` rows around the anchor, usually 5-15 minutes before and after.
   - Also extract `transcription_pasted` and `uds_focused_app_changed` in the same window.
   - Use `transcription_received` as raw evidence and `transcription_pasted`/grammar-corrected text as a cleaned version.
   - Watch for focus changes that explain where the prompt was drafted or submitted.

5. Verify whether the prompt was submitted.
   - A voice log proves it was spoken/pasted, not necessarily sent.
   - Look for app focus moving to ChatGPT/Codex/Claude/Devin right after the pasted sequence.
   - Search exact cleaned phrases in AI session logs after the voice timestamp.
   - State the confidence level: submitted prompt, pasted draft, spoken-only idea, or reconstructed prompt.

6. Present the result compactly.
   - Lead with the likely answer and why.
   - Include clickable absolute file links with line numbers.
   - Give a reconstructed prompt only after naming the raw source and timestamps.
   - Do not print secrets or API keys found in logs; redact or summarize credential evidence.

## Useful Commands

Extract a focused Always window:

```bash
scripts/extract-always-window.sh ~/Library/Logs/Always/always.2026-06-13 2026-06-13T00:47:00Z 2026-06-13T00:58:00Z
```

Search June Always speech for conceptual anchors:

```bash
for f in ~/Library/Logs/Always/always.2026-06-*; do
  jq -r 'select(.fields.message == "transcription_received")
    | select(.fields.text | test("determin|provider|providers|model|models|harness|TPS|token per second|visual|OpenRouter|static JSON"; "i"))
    | [.timestamp, .fields.text] | @tsv' "$f" 2>/dev/null
done
```

Search AI session logs without drowning in project files:

```bash
rg -n -i --hidden --glob '!node_modules/**' --glob '!Library/**' \
  'exact phrase|anchor phrase|distinctive tool name' \
  ~/.codex ~/.claude ~/.local/share/devin ~/Documents 2>/dev/null
```

Query Superwhisper SQLite by date and anchors:

```bash
sqlite3 -tabs "$HOME/Library/Application Support/superwhisper/database/superwhisper.sqlite" \
  "SELECT r.datetime, coalesce(c.c3,c.c2,c.c1,'')
   FROM recording r JOIN recording_fts_content c ON c.c0 = r.id
   WHERE r.datetime >= '2026-06-01'
     AND r.datetime < '2026-06-20'
     AND lower(coalesce(c.c3,c.c2,c.c1,'')) LIKE '%harness%'
   ORDER BY r.datetime;"
```
