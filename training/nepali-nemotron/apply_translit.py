"""Apply the learned model to all 157,905 SLR54 transcripts.

Decoding is cached per unique word (49,403 of them) rather than per token
occurrence (~1.7M), which is what makes this minutes instead of hours.
"""
import json, sys, collections
from pathlib import Path
import torch
sys.path.insert(0, str(Path(__file__).parent))
from train_translit import Seq2Seq, encode, BOS, EOS

D = Path("/private/tmp/claude-501/-Users-abhi-proj-always/4a95cd0a-1cd0-4d0c-a5dd-0eebdd6e8956/scratchpad")
ck = torch.load(D / "translit_model.pt", weights_only=False)
src, tgt, ms, mt = ck["src"], ck["tgt"], ck["ms"], ck["mt"]
itos = {i: c for c, i in tgt.items()}
dev = "mps" if torch.backends.mps.is_available() else "cpu"
model = Seq2Seq(len(src), len(tgt)).to(dev); model.load_state_dict(ck["model"]); model.eval()

def _enc(w):
    """Truncate to the training max length. SLR54 contains words longer than
    anything in the 1,844 supervised pairs, and `encode` only pads."""
    return encode(w[: ms - 2], src, ms)

@torch.no_grad()
def decode_batch(words):
    S = torch.tensor([_enc(w) for w in words], device=dev)
    B = len(words)
    cur = torch.full((B, 1), tgt[BOS], device=dev)
    done = [False] * B; outs = [[] for _ in range(B)]
    for _ in range(mt):
        nxt = model(S, cur)[:, -1].argmax(-1)
        for i in range(B):
            if not done[i]:
                ch = itos[nxt[i].item()]
                if ch == EOS: done[i] = True
                else: outs[i].append(ch)
        if all(done): break
        cur = torch.cat([cur, nxt.unsqueeze(1)], 1)
    return ["".join(o) for o in outs]

def main():
    freq = collections.Counter()
    rows = []
    for line in open(D / "utt.tsv", errors="replace"):
        p = line.rstrip("\n").split("\t")
        if len(p) >= 3:
            rows.append((p[0], p[2]))
            for w in p[2].split(): freq[w] += 1
    uniq = [w for w in freq if any("ऀ" <= c <= "ॿ" for c in w)]
    print(f"unique Devanagari words: {len(uniq)}   utterances: {len(rows)}")
    cache = {}
    for i in range(0, len(uniq), 256):
        chunk = uniq[i:i + 256]
        for w, r in zip(chunk, decode_batch(chunk)):
            cache[w] = r
        if i % 5120 == 0:
            print(f"  decoded {i}/{len(uniq)}")
    out = open(D / "slr54_roman_learned.tsv", "w", encoding="utf-8")
    for uid, text in rows:
        out.write(f"{uid}\t{' '.join(cache.get(w, w) for w in text.split())}\n")
    out.close()
    print(f"\nwrote slr54_roman_learned.tsv")
    for i, l in enumerate(open(D / "slr54_roman_learned.tsv", encoding="utf-8")):
        if i >= 5: break
        print("   ", l.strip())

if __name__ == "__main__":
    main()
