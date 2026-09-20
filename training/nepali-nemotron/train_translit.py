"""
Learned Devanagari -> Abhi's-Roman transliteration.

WHY NOT RULES: a hand-written rule set was scoring 100% on the words its
rules were tuned against and 25% on held-out vocabulary. It also breaks
silently on any word nobody anticipated. This learns the mapping as weights
from Abhi's own writing, so an unseen word is transliterated by the same
learned conventions rather than falling off the end of a rule table.

SUPERVISION: pairs where the bootstrap transliteration of an SLR54 word
equals a spelling Abhi demonstrably types (his 12 WhatsApp exports,
18,452 messages). Labels come from him, not from a standard scheme.

REGISTER: an earlier version down-weighted spellings confined to one chat,
to stop puspa (~70% of the corpus) defining "how he writes" via `hilo`. That
was wrong twice over. First, `hilo` is a playful respelling of an ENGLISH
word -- it is never a transliteration target, so it cannot reach this model
at all. Second, the Nepali-language tech conversations live in only one or
two chats (Bidwot, Shishir); down-weighting single-chat vocabulary would
have suppressed exactly the domain terms he most needs. All pairs count
equally. Every chat he sent is equally important.

Char-level seq2seq, small enough to train on the Mac's CPU/MPS in minutes.
"""
import json, math, random, sys
from pathlib import Path
import torch, torch.nn as nn

D = Path("/private/tmp/claude-501/-Users-abhi-proj-always/4a95cd0a-1cd0-4d0c-a5dd-0eebdd6e8956/scratchpad")
PAIRS = json.load(open(D / "translit_pairs.json"))

BOS, EOS, PAD = "\x02", "\x03", "\x00"

def build_vocab(pairs):
    src = {PAD: 0, BOS: 1, EOS: 2}
    tgt = {PAD: 0, BOS: 1, EOS: 2}
    for a, b in pairs:
        for c in a: src.setdefault(c, len(src))
        for c in b: tgt.setdefault(c, len(tgt))
    return src, tgt

class Seq2Seq(nn.Module):
    """GRU encoder-decoder with attention. Small on purpose: ~1.8k pairs."""
    def __init__(self, ns, nt, h=256, emb=96):
        super().__init__()
        self.se = nn.Embedding(ns, emb, padding_idx=0)
        self.te = nn.Embedding(nt, emb, padding_idx=0)
        self.enc = nn.GRU(emb, h, batch_first=True, bidirectional=True)
        self.dec = nn.GRU(emb + 2 * h, h, batch_first=True)
        self.att = nn.Linear(h + 2 * h, 1)
        self.out = nn.Linear(h + 2 * h, nt)
        self.h = h

    def forward(self, s, t):
        eo, _ = self.enc(self.se(s))                       # B,S,2h
        B, T = t.shape
        hid = torch.zeros(1, B, self.h, device=s.device)
        emb = self.te(t)
        logits = []
        for i in range(T):
            q = hid[-1].unsqueeze(1).expand(-1, eo.size(1), -1)
            a = torch.softmax(self.att(torch.cat([q, eo], -1)).squeeze(-1)
                              .masked_fill(s == 0, -1e9), -1)
            ctx = (a.unsqueeze(-1) * eo).sum(1)
            o, hid = self.dec(torch.cat([emb[:, i], ctx], -1).unsqueeze(1), hid)
            logits.append(self.out(torch.cat([o.squeeze(1), ctx], -1)))
        return torch.stack(logits, 1)

def encode(w, v, maxlen):
    ids = [v[BOS]] + [v.get(c, 0) for c in w] + [v[EOS]]
    return ids + [0] * (maxlen - len(ids))

def main():
    random.seed(0); torch.manual_seed(0)
    pairs = [(a, b) for a, b in PAIRS if a and b]
    random.shuffle(pairs)
    n_val = max(80, len(pairs) // 10)
    val, train = pairs[:n_val], pairs[n_val:]
    print(f"pairs: {len(pairs)}  train {len(train)}  held-out {len(val)}")

    src, tgt = build_vocab(pairs)
    itos = {i: c for c, i in tgt.items()}
    ms = max(len(a) for a, _ in pairs) + 2
    mt = max(len(b) for _, b in pairs) + 2

    dev = "mps" if torch.backends.mps.is_available() else "cpu"
    model = Seq2Seq(len(src), len(tgt)).to(dev)
    opt = torch.optim.AdamW(model.parameters(), lr=3e-3)
    lossf = nn.CrossEntropyLoss(ignore_index=0)

    S = torch.tensor([encode(a, src, ms) for a, _ in train], device=dev)
    T = torch.tensor([encode(b, tgt, mt) for _, b in train], device=dev)
    VS = torch.tensor([encode(a, src, ms) for a, _ in val], device=dev)

    best = 0.0
    for ep in range(1, 121):
        model.train()
        perm = torch.randperm(len(S), device=dev)
        tot = 0.0
        for i in range(0, len(S), 64):
            idx = perm[i:i + 64]
            s, t = S[idx], T[idx]
            logits = model(s, t[:, :-1])
            l = lossf(logits.reshape(-1, logits.size(-1)), t[:, 1:].reshape(-1))
            opt.zero_grad(); l.backward()
            nn.utils.clip_grad_norm_(model.parameters(), 1.0)
            opt.step(); tot += l.item()
        if ep % 10 == 0 or ep == 1:
            acc = evaluate(model, VS, val, tgt, itos, mt, dev)
            print(f"  epoch {ep:>3}  loss {tot/max(1,len(S)//64):.4f}  held-out exact {acc*100:.1f}%")
            if acc > best:
                best = acc
                torch.save({"model": model.state_dict(), "src": src, "tgt": tgt,
                            "ms": ms, "mt": mt}, D / "translit_model.pt")
    print(f"\nbest held-out exact-match: {best*100:.1f}%  -> translit_model.pt")

@torch.no_grad()
def evaluate(model, VS, val, tgt, itos, mt, dev):
    model.eval()
    ok = 0
    for k in range(len(val)):
        s = VS[k:k+1]
        cur = torch.tensor([[tgt[BOS]]], device=dev)
        out = []
        for _ in range(mt):
            nxt = model(s, cur)[0, -1].argmax().item()
            if itos[nxt] == EOS: break
            out.append(itos[nxt])
            cur = torch.cat([cur, torch.tensor([[nxt]], device=dev)], 1)
        if "".join(out) == val[k][1]: ok += 1
    return ok / len(val)

if __name__ == "__main__":
    main()
