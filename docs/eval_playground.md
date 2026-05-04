# Eval Playground (proposed)

Closed-loop regression test for the voice-to-text pipeline. Daemon
prompts the user to dictate target sentences, captures the result,
shows a diff, records the verdict. Builds a ground-truth corpus that
catches Whisper drift, glossary regressions, and post-process bugs
*before* a release.

> **Status:** design only. Not built yet. Track via this doc; convert
> to issues / PR when the simplified pipeline (post `pre-pipeline-simplification`
> tag) has stabilized in daily use.

---

## Why this exists

Voice transcription regressions are invisible to unit tests. Real bugs
we hit in 2026:

| Bug | How it slipped through unit tests |
|---|---|
| `idea` → `IntelliJ IDEA` | No test asserted dictating "idea" pasted "idea" |
| `analyzed` → `analyZed` | `Vocabulary::apply` was tested with synthetic strings, not real Whisper output |
| `Yes.` → filtered as hallucination | Filter unit tests didn't exercise real one-word affirmatives |
| Premature paste mid-utterance | VAD tested on synthetic frames, not real speech with pauses |

A unit test cannot ask the question *"if I dictate this, do I get this?"*.
Only a closed loop with the user holding a microphone can.

---

## CLI surface

```bash
always eval                           # interactive walk-through, all rows
always eval --ids regression-001,jargon-007
always eval --category jargon         # filter by category
always eval --replay-failures         # re-run only failed rows from last session
always eval --json                    # machine-readable output (for CI)
```

### Per-row interaction

```
[3/30] regression-007 (jargon)
Expected: I have an idea about Kubernetes.

→ Dictate now (5 s timeout, ⌃C to skip)…

Whisper raw     : I have an idea about Kubernetes.
Post-processed  : I have an idea about Kubernetes.
Expected        : I have an idea about Kubernetes.

Diff: ✓ exact match.

Verdict (y / n / s = skip / r = re-dictate): _
```

Failure example:

```
[12/30] glossary-002 (jargon)
Expected: deploy the kubernetes cluster

Whisper raw     : deploy the kubernetics cluster
Post-processed  : deploy the kubernetics cluster

Diff (word-level):
  deploy the [kubernetes ↔ kubernetics] cluster

Verdict (y / n / s / r): n
Optional note: glossary entry exists; LLM postprocess is OFF
```

---

## Corpus format — `tests/eval_corpus.json`

```json
[
  {
    "id": "regression-007",
    "category": "jargon",
    "expected": "I have an idea about Kubernetes.",
    "added": "2026-05-04",
    "regression_of": "70eac2e",
    "notes": "Whisper used to bias `idea` into `IntelliJ IDEA` via initial_prompt."
  },
  {
    "id": "edge-yes",
    "category": "edge",
    "expected": "Yes.",
    "added": "2026-05-04",
    "regression_of": "8e6066b",
    "notes": "One-word affirmative; was filtered as hallucination phrase."
  },
  {
    "id": "ordinary-001",
    "category": "ordinary",
    "expected": "Tell me when the deployment finishes.",
    "added": "2026-05-04"
  }
]
```

**Categories** (initial set, extend as needed):

| Category | Purpose |
|---|---|
| `regression` | Sentences pinned to a specific commit that fixed a real bug |
| `jargon` | Domain-specific terms (Kubernetes, OAuth, GraphQL, TypeScript…) |
| `ordinary` | Plain English — the bulk of real dictation |
| `edge` | One-word affirmatives, numbers, punctuation, symbols |
| `code` | Code-style fragments (`git status`, `npm install foo --save-dev`) |
| `pause` | Sentences a natural speaker would pause mid-way through |

---

## Result schema — `~/.always/eval_results.jsonl`

Append-only NDJSON. One record per row per run:

```json
{
  "run_id": "01J5W6Q3ZF8...",
  "ts": "2026-05-04T22:14:33Z",
  "target_id": "regression-007",
  "expected": "I have an idea about Kubernetes.",
  "raw": "I have an idea about Kubernetes.",
  "processed": "I have an idea about Kubernetes.",
  "verdict": "pass",
  "note": null,
  "audio_path": "~/.always/eval_failures/regression-007-01J5W6Q3.wav",
  "git_sha": "70eac2e",
  "corpus_version": 1
}
```

`audio_path` set only on `verdict == fail` so a future fix can replay
the same audio without re-dictation.

---

## Implementation outline

### v1 — interactive CLI (~3–4 h)

* New subcommand `always eval` (Clap subcommand under `Commands`).
* Reads `tests/eval_corpus.json` from the repo root or via `ALWAYS_EVAL_CORPUS`.
* For each row:
  1. Print expected text.
  2. Trigger one-shot daemon capture via existing UDS or by spawning
     `always run --once` (need to add `--once` to the daemon).
  3. Wait up to 8 s for a `TranscriptFinal` event (or `TranscriptionFiltered`).
  4. Compute `expected ↔ raw` and `expected ↔ processed` word diffs
     using the existing `correction::diff_words` machinery.
  5. Read y/n/s/r from stdin.
* Append to `~/.always/eval_results.jsonl`.
* Print summary at end: total / pass / fail / skip + per-category breakdown.

### v2 — failure replay (~+2 h)

* On `verdict == fail`, save the captured WAV to
  `~/.always/eval_failures/<target_id>-<run_id>.wav`.
* Add `--replay-failures` flag: skip the dictation step and feed the
  saved WAV directly into `stt::transcribe_from_bytes`. Lets a fix be
  validated against the exact original audio.

### v3 — CI integration (~+3 h)

* GitHub Actions job: `always eval --json --replay-failures-only` runs
  every PR against a checked-in corpus of `*.wav` from the
  `eval_failures/` dir.
* Trend dashboard: pass rate over time per category (Codecov-style
  comments on PRs).
* Required for merge once corpus reaches ~50 rows.

---

## Design choices worth pinning early

- **Single utterance per row** — clean diff semantics; multi-sentence
  rows would conflate VAD bugs with content bugs.
- **Verdict is binary** (✓/✗) + optional free-text. No partial credit.
- **Diff display** — word-level, ANSI-colored (green match, red
  miss, yellow substitution). Reuse `correction::diff_words` for the
  pair extraction.
- **Audio capture for failures** — save the raw WAV. Without this,
  any "fix" is unreproducible: the same target text dictated twice
  hits a different acoustic. With saved WAVs, the test becomes
  deterministic.
- **`corpus_version` field** so old runs survive corpus growth.

---

## Scope

| Phase | Effort | What lands |
|---|:---:|---|
| v1 | 3–4 h | Interactive CLI + JSONL results + 30-row seed corpus |
| v2 | +2 h | Failure replay via saved WAVs |
| v3 | +3 h | CI integration + trend dashboard |

Best built as a **standalone session** once the simplified pipeline has
been used in production for a few days — by then we'll have a real
list of regressions to seed the corpus with rather than a synthetic
guess.

---

## Open questions

1. **One-shot daemon mode.** Today the daemon runs as an always-on
   service. The eval needs to capture exactly one utterance and exit.
   Cleanest: add `always run --once` that exits after the first
   non-filtered transcription. Alternative: send a `CaptureOnce`
   command over UDS.

2. **Audio capture for replay.** SoX `rec` already produces raw audio
   we tee into the WAV encoder. Add a flag to also write the audio to
   `~/.always/eval_failures/` when the daemon is in eval mode.

3. **Per-user vs shared corpus.** Should each user maintain their own
   `eval_corpus.json` (their own jargon), or do we ship a canonical
   one that everyone uses? Probably both: ship a baseline 30-row
   corpus + allow `--corpus path/to/personal.json` override.

4. **Whisper non-determinism.** Same audio → same Groq Whisper output
   ~95 % of the time. The 5 % that vary make CI flaky. Mitigation:
   require 3 consecutive replay passes, or relax the equality check
   to "Levenshtein distance ≤ 1 word".

---

## Related

- `tests/correction_e2e_test.rs` — file-level regression tests for the
  diff + glossary apply logic.
- `tests/stt_resilience_test.rs` — wiremock-based contract tests for
  the Groq client. Doesn't exercise real audio.
- The `pre-pipeline-simplification` git tag — baseline to roll back to
  if the simpler pipeline has missed corner cases.
