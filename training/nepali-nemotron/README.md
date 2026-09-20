# Nepali for Nemotron 3.5 ASR

Goal: Abhi speaks Nepali, Always types it in **his** romanisation — the way
he writes on WhatsApp — with no manual language switching.

## Why this is needed

Nemotron 3.5 ASR supports 40 language-locales. **Nepali is not one of them**,
in either tier:

- 32 transcription-ready locales — work out of the box
- 8 adaptation-ready (el-GR he-IL lt-LT sl-SI lv-LV mt-MT th-TH nn-NO) —
  tokenizer knows them, fine-tune unlocks them
- `ne-NP` is in **neither**. parakeet-rs exposes prompt slot 46 for it, but
  the model returns EMPTY for every input on that slot, English included —
  the embedding is untrained, not weak.

Devanagari itself IS covered (`hi-IN` is transcription-ready), so this is
teaching a language onto an existing script, not a new writing system.

## Pipeline

1. **Mine Abhi's romanisation** from 12 WhatsApp exports (18,452 of his
   messages, 9,736 distinct tokens). His conventions are not standard:
   `छ -> x` (xa, hunxa, parxa, xaina) and `भ -> v` (vayo, vanera, vako).
   A standard transliterator produces `chha`/`bha` and is wrong on his two
   commonest consonants.
2. **Learn Devanagari -> his Roman** (`train_v2.py`), two stages:
   - pre-train on Aksharantar `nep`, 2.4M pairs, for coverage
   - fine-tune on 1,844 pairs mined from his own writing, for his style
3. **Transliterate SLR54** (157,905 Nepali utterances, CC BY-SA, Google) into
   his romanisation -> training transcripts.
4. **Fine-tune Nemotron** on (Nepali audio -> his Roman text) with `ne-NP`
   prompt conditioning; export int8 ONNX in the layout parakeet-rs loads.

## What failed, and why it is recorded here

`translit.py` (rules) scored 18/18 on the words its rules were tuned against
and **25%** on held-out vocabulary. Hand-tuning against your own test set is
not evidence.

`train_translit.py` (v1, 1,660 pairs) reported **87.5% held-out** and still
emitted `karnkerko` for काङ्ग्रेसको and `dah` for ००७. The held-out slice was
drawn from the same high-frequency vocabulary as training, so it measured
memorisation, not generalisation. Fixed in v2 by pre-training on 2.4M pairs
and handling digits outside the model.

## Attribution

- SLR54 Nepali ASR corpus — CC BY-SA 4.0, (c) Google Inc. 2016-2018
  (kjartansson-etal-sltu2018)
- Aksharantar — AI4Bharat
- Nemotron 3.5 ASR — NVIDIA, OpenMDW-1.1

## What actually shipped

Step 4 (fine-tuning Nemotron on Nepali audio) was never needed to stop
Devanagari reaching the editor. The model resolves Nepali acoustics onto
`hi-IN` and emits Devanagari; rewriting that Devanagari after the fact solves
the user-visible problem without touching the ASR model at all.

`build_tables.py` runs the v2 transliterator **once, offline**, over SLR54 and
word-aligns the result back onto the Devanagari source, producing a static
answer sheet that `src/always/translit.rs` compiles in:

    resources/translit/dev_roman.tsv     49,064 Devanagari -> his Roman
    resources/translit/roman_freq.tsv     9,730 Roman tokens he has typed

Model quality at binary-search latency: 28 ns for an English utterance, 7.2 µs
for a full Devanagari sentence, against a ~870 ms decode. No Python, no ONNX,
no model load on the paste path.

`rules3.py` mirrors the Rust rule engine that handles words the table lacks.
Change the rules in `src/always/translit.rs`, mirror them there, re-run
`build_tables.py`.

Regenerate with:

    python3 training/nepali-nemotron/build_tables.py [ARTIFACT_DIR]

Two entries in that table were badly wrong before the attestation repair step —
`छ -> xaxa` (his 6th commonest word) and `म -> mam`. The model is weak on
one- and two-character inputs, which the 1,844 supervised pairs happened not to
cover. The repair only fires where the rule engine and his own typing agree
against an output he has never written: 39 entries, all fixes.
