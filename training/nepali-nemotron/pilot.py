"""
Go/no-go pilot: can Nemotron 3.5 ASR's ne-NP prompt slot (46) be trained at all?

Nepali is NOT among the model's 40 locales — neither the 32 transcription-ready
nor the 8 adaptation-ready (el/he/lt/sl/lv/mt/th/nn). Slot 46 exists in
parakeet-rs's PROMPT_DICTIONARY but returns EMPTY on every input, including
English, which means the prompt embedding is untrained rather than merely weak.

This script answers ONE question cheaply, before anyone commits a weekend:
    after a few hundred steps on a small SLR54 subset,
    does ne-NP emit ANY Nepali?

  yes -> the slot is trainable; scale up with confidence
  no  -> the embedding is dead; stop here

Designed for a Kaggle free T4 (16GB, Turing => fp16, no bf16).
"""
import json, os, subprocess, sys, tarfile, urllib.request
from pathlib import Path

WORK = Path(os.environ.get("PILOT_WORK", "/kaggle/working"))
DATA = WORK / "data"; DATA.mkdir(parents=True, exist_ok=True)
TARGET_LANG = "ne-NP"          # the slot under test
SUBSET_UTTS = int(os.environ.get("SUBSET_UTTS", "4000"))
MAX_STEPS   = int(os.environ.get("MAX_STEPS", "600"))

# SLR54: Large Nepali ASR training set, ~157K utterances, CC BY-SA 4.0.
# 16 shards asr_nepali_0.zip..f.zip (562 MB each, 8.8 GB total) + utt_spk_text.tsv
# Attribution required: Copyright 2016-2018 Google, Inc. (kjartansson-etal-sltu2018)
SLR54 = "https://www.openslr.org/resources/54/"
SHARDS = ["asr_nepali_{}.zip".format(c) for c in "0123456789abcdef"]

def fetch(url: str, dest: Path):
    if dest.exists():
        print(f"  cached {dest.name}"); return
    print(f"  downloading {dest.name} ...")
    urllib.request.urlretrieve(url, dest)

def step1_data(n_shards: int = 1):
    """One shard is plenty for a go/no-go."""
    fetch(SLR54 + "utt_spk_text.tsv", DATA / "utt_spk_text.tsv")
    for s in SHARDS[:n_shards]:
        fetch(SLR54 + s, DATA / s)
        subprocess.run(["unzip", "-qo", str(DATA / s), "-d", str(DATA)], check=True)

def build_manifest(out: Path, limit: int):
    """
    NeMo manifest, one JSON object per line. `target_lang` is load-bearing:
    it drives the prompt-based language conditioning, and a wrong or
    unrecognised value trains the wrong slot (or none).
    """
    import soundfile as sf
    tsv = (DATA / "utt_spk_text.tsv").read_text(encoding="utf-8").splitlines()
    written = 0
    with out.open("w", encoding="utf-8") as fh:
        for line in tsv:
            parts = line.split("\t")
            if len(parts) < 3:
                continue
            utt, _spk, text = parts[0], parts[1], parts[2].strip()
            # SLR54 ships WAV (about.html: "consists of wave files"), but
            # accept flac too in case a mirror re-encodes.
            audio = next(DATA.glob(f"**/{utt}.wav"), None) or next(DATA.glob(f"**/{utt}.flac"), None)
            if audio is None or not text:
                continue
            info = sf.info(str(audio))
            fh.write(json.dumps({
                "audio_filepath": str(audio),
                "duration": round(info.frames / info.samplerate, 3),
                "text": text,
                "target_lang": TARGET_LANG,
            }, ensure_ascii=False) + "\n")
            written += 1
            if written >= limit:
                break
    print(f"  manifest: {written} utterances -> {out}")
    return written

if __name__ == "__main__":
    print("[1/4] data"); step1_data()
    print("[2/4] manifest")
    n = build_manifest(WORK / "train_ne.json", SUBSET_UTTS)
    if n == 0:
        sys.exit("no utterances built — check SLR54 layout")
    print("[3/4] train  -> see train.sh")
    print("[4/4] evaluate slot 46 before/after")
