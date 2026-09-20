"""
Two-stage learned transliteration: Devanagari -> Abhi's Roman.

STAGE 1 pre-train on Aksharantar `nep` (2,397,414 Nepali pairs, AI4Bharat).
  Teaches general Devanagari->Latin that GENERALISES to unseen words. This is
  the part v1 lacked: 1,660 pairs gave 87.5% on a held-out slice of the same
  high-frequency vocabulary and produced garbage ("karnkerko", "dah") on the
  real 49,403-word corpus. Scoring well on an unrepresentative held-out set
  is not generalisation.

STAGE 2 fine-tune on Abhi's 1,844 pairs, mined from 18,452 of his WhatsApp
  messages. Aksharantar romanises with standard conventions (`mandachhan`);
  he writes `x` for छ and `v` for भ. Stage 2 moves the learned weights onto
  HIS conventions without losing stage 1's coverage.

Digits are handled outside the model: ०-९ are an unambiguous 1:1 map, and
letting a seq2seq guess them is how "००७" became "dah".
"""
import json, random, sys, time
from pathlib import Path
import torch, torch.nn as nn
sys.path.insert(0, str(Path(__file__).parent))
from train_translit import encode, BOS, EOS
from model_tf import TranslitTF

D = Path("/private/tmp/claude-501/-Users-abhi-proj-always/4a95cd0a-1cd0-4d0c-a5dd-0eebdd6e8956/scratchpad")
DIGITS = {d: str(i) for i, d in enumerate("०१२३४५६७८९")}
MAXLEN = 24

def load_aksharantar(limit=400_000):
    pairs = []
    for line in open(D / "nep/nep_train.json", errors="replace"):
        try: o = json.loads(line)
        except: continue
        a, b = o.get("native word", ""), o.get("english word", "")
        if a and b and len(a) <= MAXLEN - 2 and len(b) <= MAXLEN - 2:
            pairs.append((a, b))
        if len(pairs) >= limit: break
    return pairs

def main():
    random.seed(0); torch.manual_seed(0)
    t0 = time.time()
    ak = load_aksharantar()
    mine = [(a, b) for a, b in json.load(open(D / "translit_pairs.json"))
            if len(a) <= MAXLEN - 2 and len(b) <= MAXLEN - 2]
    print(f"stage1 pairs {len(ak)}   stage2 pairs {len(mine)}")

    # shared vocab across both stages so stage 2 can reuse stage 1's weights
    src = {"\x00": 0, BOS: 1, EOS: 2}; tgt = dict(src)
    for a, b in ak + mine:
        for c in a: src.setdefault(c, len(src))
        for c in b: tgt.setdefault(c, len(tgt))
    for d in DIGITS: src.setdefault(d, len(src))
    itos = {i: c for c, i in tgt.items()}
    print(f"src vocab {len(src)}  tgt vocab {len(tgt)}")

    dev = "mps" if torch.backends.mps.is_available() else "cpu"
    model = TranslitTF(len(src), len(tgt)).to(dev)
    lossf = nn.CrossEntropyLoss(ignore_index=0)

    def run(pairs, epochs, lr, tag, bs=512):
        opt = torch.optim.AdamW(model.parameters(), lr=lr)
        S = torch.tensor([encode(a[:MAXLEN-2], src, MAXLEN) for a, _ in pairs])
        T = torch.tensor([encode(b[:MAXLEN-2], tgt, MAXLEN) for _, b in pairs])
        for ep in range(1, epochs + 1):
            model.train()
            perm = torch.randperm(len(S))
            tot = nb = 0
            for i in range(0, len(S), bs):
                idx = perm[i:i+bs]
                s, t = S[idx].to(dev), T[idx].to(dev)
                lg = model(s, t[:, :-1])
                l = lossf(lg.reshape(-1, lg.size(-1)), t[:, 1:].reshape(-1))
                opt.zero_grad(); l.backward()
                nn.utils.clip_grad_norm_(model.parameters(), 1.0)
                opt.step(); tot += l.item(); nb += 1
            print(f"  [{tag}] epoch {ep}/{epochs} loss {tot/max(nb,1):.4f} ({time.time()-t0:.0f}s)")
            torch.save({"model": model.state_dict(), "src": src, "tgt": tgt,
                        "ms": MAXLEN, "mt": MAXLEN}, D / "translit_v2.pt")

    run(ak, 4, 3e-4, "stage1-general", bs=1024)
    run(mine, 60, 1e-4, "stage2-abhi", bs=128)
    print(f"\nsaved translit_v2.pt  ({time.time()-t0:.0f}s total)")

if __name__ == "__main__":
    main()
