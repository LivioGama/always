"""Transliterate all 157,905 SLR54 transcripts into Abhi's romanisation."""
import sys, collections, torch
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from model_tf import TranslitTF
from train_translit import encode, BOS, EOS

D = Path("/private/tmp/claude-501/-Users-abhi-proj-always/4a95cd0a-1cd0-4d0c-a5dd-0eebdd6e8956/scratchpad")
DIG = {d: str(i) for i, d in enumerate("०१२३४५६७८९")}
ck = torch.load(D / "translit_v2.pt", weights_only=False)
src, tgt, ML = ck["src"], ck["tgt"], ck["ms"]
itos = {i: c for c, i in tgt.items()}
dev = "mps" if torch.backends.mps.is_available() else "cpu"
m = TranslitTF(len(src), len(tgt)).to(dev); m.load_state_dict(ck["model"]); m.eval()

def decode(words):
    S = torch.tensor([encode(w[:ML-2], src, ML) for w in words], device=dev)
    out, res = m.greedy(S, tgt[BOS], tgt[EOS], ML), []
    for row in out:
        s = ""
        for i in row[1:].tolist():
            c = itos.get(i, "")
            if c == EOS or c == "\x00": break
            s += c
        res.append(s)
    return res

def main():
    rows, freq = [], collections.Counter()
    for line in open(D / "utt.tsv", errors="replace"):
        p = line.rstrip("\n").split("\t")
        if len(p) >= 3:
            rows.append((p[0], p[2]))
            for w in p[2].split(): freq[w] += 1
    # digits are a 1:1 map -- never let the model guess them (v1 emitted "dah")
    cache = {w: "".join(DIG[c] for c in w) for w in freq if all(c in DIG for c in w)}
    todo = [w for w in freq if w not in cache]
    print(f"utterances {len(rows)}  unique words {len(freq)}  to decode {len(todo)}")
    for i in range(0, len(todo), 512):
        chunk = todo[i:i+512]
        for w, r in zip(chunk, decode(chunk)): cache[w] = r or w
        if i % 10240 == 0: print(f"  {i}/{len(todo)}")
    with open(D / "slr54_abhi.tsv", "w", encoding="utf-8") as f:
        for uid, text in rows:
            f.write(f"{uid}\t{' '.join(cache.get(w, w) for w in text.split())}\n")
    print(f"wrote slr54_abhi.tsv")
    for i, l in enumerate(open(D / "slr54_abhi.tsv", encoding="utf-8")):
        if i >= 4: break
        print("   ", l.strip())

if __name__ == "__main__":
    main()
