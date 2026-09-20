#!/usr/bin/env python3
"""
Build the two static tables that `src/always/translit.rs` compiles in.

    python3 training/nepali-nemotron/build_tables.py [ARTIFACT_DIR]

Outputs, both byte-sorted so the Rust side can binary-search them in place:

    resources/translit/dev_roman.tsv    Devanagari -> his Roman spelling
    resources/translit/roman_freq.tsv   Roman token -> how often he typed it

Why a table and not the model
-----------------------------
`train_v2.py` produces a 19 MB transformer that is 99.0% exact on held-out
pairs of his own spellings. Running it would mean either a Python subprocess or
an ONNX runtime on the paste critical path, which already costs ~870 ms of
decode. Instead the model is run ONCE, offline, over a whole Nepali corpus, and
what ships is the answer sheet: model quality at hash-lookup latency.

Sources
-------
    utt.tsv           SLR54, 157,905 Nepali utterances in Devanagari
                      (CC BY-SA 4.0, (c) Google Inc. 2016-2018)
    slr54_abhi.tsv    the same utterances after `apply_v2.py` — line-aligned
    translit_pairs.json  1,844 Devanagari -> Roman pairs mined from 12 WhatsApp
                      exports; these are HIS spellings, not the model's
    vocab_all.json    9,736 Roman tokens he has actually typed, with counts

Pipeline
--------
1. Word-align `utt.tsv` against `slr54_abhi.tsv`. Both files are per-utterance
   and the transliteration is word-for-word, so a token-count match gives a
   clean alignment: 157,905/157,905 lines, 49,403 distinct Devanagari types,
   zero types with conflicting Roman forms.
2. Strip Devanagari punctuation (danda and friends) out of the keys. SLR54
   sentences end in `।`, so without this the table learns `होइनन्। -> hoinn`
   and silently eats the sentence break at lookup time.
3. Let his 1,844 mined pairs override the model wherever they disagree.
4. Repair the model where two independent derivations outvote it: if the
   model's output is a string he has NEVER typed, and the rule engine's output
   IS one he types, take the rule engine's. This is deliberately narrow — it
   uses the rule engine's raw output with no schwa re-ranking, because the
   re-ranker strips a final `a` and lands on English words in his vocabulary
   (`उमा -> um`, `नेता -> net`, `चना -> can`). Restricted this way it fires 39
   times and every one is a fix, including the two worst entries in the whole
   table: `छ -> xaxa` (his 6th commonest word) and `म -> mam`.

`rules3.py` mirrors the Rust rule engine. If `src/always/translit.rs` changes
its rules, mirror them there and re-run this script.
"""

import json
import os
import sys
import collections

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
OUT = os.path.join(REPO, "resources", "translit")

DEFAULT_ARTIFACTS = (
    "/private/tmp/claude-501/-Users-abhi-proj-always/"
    "4a95cd0a-1cd0-4d0c-a5dd-0eebdd6e8956/scratchpad"
)

PUNCT = set("।॥॰ॱ")


def is_devanagari(c):
    return "ऀ" <= c <= "ॿ"


def ok_key(k):
    return bool(k) and all(
        (is_devanagari(c) and c not in PUNCT) or c in "‌‍" for c in k
    )


def ok_value(v):
    return bool(v) and v.isascii() and all(c.isalnum() for c in v)


def strip_punct(k):
    return "".join(c for c in k if c not in PUNCT)


def align(art):
    """Word-align the Devanagari corpus with its transliteration."""
    dev = [l.rstrip("\n").split("\t") for l in open(os.path.join(art, "utt.tsv"))]
    rom = [l.rstrip("\n").split("\t") for l in open(os.path.join(art, "slr54_abhi.tsv"))]
    pair = collections.defaultdict(collections.Counter)
    aligned = skipped = 0
    for a, b in zip(dev, rom):
        if len(a) < 3 or len(b) < 2 or a[0] != b[0]:
            skipped += 1
            continue
        dt, rt = a[2].split(), b[1].split()
        if len(dt) != len(rt):
            skipped += 1
            continue
        aligned += 1
        for x, y in zip(dt, rt):
            pair[x][y] += 1
    print(f"aligned {aligned} utterances, skipped {skipped}")
    ambiguous = sum(1 for v in pair.values() if len(v) > 1)
    print(f"{len(pair)} distinct Devanagari types, {ambiguous} ambiguous")
    return pair


def main():
    art = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_ARTIFACTS
    sys.path.insert(0, HERE)
    import rules3  # mirror of the Rust rule engine (lives next to this file)

    # -- roman_freq.tsv --------------------------------------------------
    vocab = json.load(open(os.path.join(art, "vocab_all.json")))
    vocab = {w: c for w, c in vocab.items() if w.isascii() and w.isalnum()}
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "roman_freq.tsv"), "w") as f:
        for w, c in sorted(vocab.items()):
            f.write(f"{w}\t{c}\n")
    print(f"roman_freq.tsv: {len(vocab)} tokens")

    # -- dev_roman.tsv ---------------------------------------------------
    pair = align(art)
    # Collapse punctuation-stripped keys; the more frequent source wins.
    candidates = collections.defaultdict(list)
    for k, counter in pair.items():
        ks = strip_punct(k)
        v = counter.most_common(1)[0][0]
        if ok_key(ks) and ok_value(v):
            candidates[ks].append((sum(counter.values()), v))
    table = {k: max(vs)[1] for k, vs in candidates.items()}

    supervised = {
        strip_punct(d): r
        for d, r in json.load(open(os.path.join(art, "translit_pairs.json")))
    }
    overrides = sum(1 for k, v in supervised.items() if table.get(k) != v)
    for k, v in supervised.items():
        if ok_key(k) and ok_value(v):
            table[k] = v
    print(f"his own pairs overrode the model {overrides} times")

    repairs = 0
    for k, v in list(table.items()):
        if k in supervised:
            continue
        base = rules3.assemble(rules3.syllabify(k))
        if base != v and vocab.get(v, 0) == 0 and vocab.get(base, 0) > 0:
            table[k] = base
            repairs += 1
    print(f"attestation repairs: {repairs}")

    with open(os.path.join(OUT, "dev_roman.tsv"), "w") as f:
        for k, v in sorted(table.items()):
            f.write(f"{k}\t{v}\n")
    print(f"dev_roman.tsv: {len(table)} entries")


if __name__ == "__main__":
    main()
