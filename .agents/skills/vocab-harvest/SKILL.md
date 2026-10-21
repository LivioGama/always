---
name: always:vocab-harvest
description: Harvest vocabulary worth keeping from the current chat (or a transcript) into the Always glossary, and manually add a single word. Use when the user says "extract/harvest the terms from this chat", "add these words to my vocabulary/glossary", "remember this word for dictation", or names vocab-harvest. Always filters out terms already in the glossary and presents candidates in three acceptance tiers.
---

# Vocab Harvest

Mine a conversation for terms worth teaching the Always dictation glossary
(`~/.always/glossary.json`), dedupe against what is already known, propose the
survivors in **three tiers** so the user can accept whole groups with one
word, and merge the accepted terms atomically.

## Data model

`~/.always/glossary.json` is a JSON array:

```json
{"term": "Claude Code", "mistranscriptions": ["clod code"], "frequency": 101, "weight": 3, "provenance": "hotkey"}
```

- `term` — canonical spelling the dictation engine should emit.
- `mistranscriptions` — wrong forms to replace (harvested terms start empty).
- `provenance` — `manual` for explicit "add word X"; `llm` for chat-harvested
  suggestions the user approved.
- The daemon hot-reloads on file mtime — external writes take effect without
  a restart. Writes MUST be atomic (temp file + rename) and additive-only.

## Helper

```text
.agents/skills/vocab-harvest/add_terms.py   (repo-relative; always checkout)
```

- `add_terms.py --list` — every known term, lowercased (the exclusion set).
- `add_terms.py --provenance llm TERM ...` — merge approved suggestions.
- `add_terms.py TERM ...` — manual add (provenance `manual`).
- `echo '["a","b"]' | add_terms.py --stdin` — batch merge.
- `add_terms.py --glossary PATH ...` — operate on a copy (testing).

It dedupes case-insensitively against both `term` and `mistranscriptions`,
never modifies existing entries, and reports `{"added": [...], "skipped_already_known": [...]}`.

## Harvest flow

1. **Build the exclusion set.** `add_terms.py --list` — everything already in
   the glossary (terms AND mistranscriptions). Never propose any of these.
2. **Collect candidates** from the current conversation (or a transcript file
   the user points at). Keep terms that dictation plausibly garbles or that are
   worth spelling exactly:
   - proper nouns: people, products, projects, repos, hosts (`genesis`, `exodus`)
   - tool/CLI/library names (`qemu-img`, `tsgo`, `GitNexus`)
   - paths and identifiers (`~/.agent-config`, `bun.lock`)
   - domain jargon, acronyms, non-English words the user uses repeatedly
   - terms the dictation visibly mangled during the chat
   Skip: common English words, one-off filler, anything already known.
3. **Present exactly three tiers** — the user answers with group numbers, not
   word lists:

   | Tier | Label | Contents |
   |---|---|---|
   | 1 | **Obviously keep** | project/tool/host/proper names the user says daily; terms STT already mangled this chat |
   | 2 | **Probably keep** | real technical terms used more than once; less certain proper nouns |
   | 3 | **Unlikely** | borderline: possibly one-off, possibly common English, low-value |

   Format: a short numbered header per group + the candidate terms. End with
   "Which groups do you take? (e.g. '1 and 2', or 'all', or name single words)".
4. **Merge the accepted tiers** via `add_terms.py --provenance llm <terms>` and
   report the added/skipped counts. Terms named individually outside a chosen
   group are added too; the user can also veto single words ("all of 1 except X").
5. **Manual add.** "Add the word X" / "vocab add X" skips the harvest entirely:
   run `add_terms.py X` (provenance `manual`), confirm, done. Still check
   `--list` first — if X is already known, say so and add nothing.

## Rules

- NEVER modify or delete existing glossary entries — append only.
- NEVER guess a term's spelling: use the form as the user wrote it; if only
  heard via dictation and ambiguous, confirm the spelling first.
- If a harvested word looks like a *mishearing correction* rather than new
  vocabulary (e.g. dictation wrote "plot code" for "Claude Code"), propose it
  as a `mistranscriptions` entry on the canonical term instead of a new term —
  ask which the user wants when unsure.
- If `~/.always/glossary.json` does not exist, tell the user Always isn't
  installed/configured and offer to create the file instead of failing.
