"""
Transformer seq2seq for Devanagari -> Roman.

Replaces the GRU+attention model, whose decoder looped over timesteps in
Python: ~19,000 tiny sequential GPU ops per epoch, where MPS launch overhead
dominated actual compute (2.27s per batch of 512 -> 29.5 min/epoch).
A transformer decodes all positions in ONE parallel pass under teacher
forcing, which is the same arithmetic in a fraction of the wall-clock.
"""
import math, torch, torch.nn as nn

class PosEnc(nn.Module):
    def __init__(self, d, maxlen=64):
        super().__init__()
        pe = torch.zeros(maxlen, d)
        pos = torch.arange(maxlen).unsqueeze(1).float()
        div = torch.exp(torch.arange(0, d, 2).float() * (-math.log(10000.0) / d))
        pe[:, 0::2] = torch.sin(pos * div); pe[:, 1::2] = torch.cos(pos * div)
        self.register_buffer("pe", pe.unsqueeze(0))
    def forward(self, x): return x + self.pe[:, : x.size(1)]

class TranslitTF(nn.Module):
    def __init__(self, ns, nt, d=256, heads=4, layers=3, ff=768, drop=0.1):
        super().__init__()
        self.se = nn.Embedding(ns, d, padding_idx=0)
        self.te = nn.Embedding(nt, d, padding_idx=0)
        self.pe = PosEnc(d)
        self.tf = nn.Transformer(d, heads, layers, layers, ff, drop,
                                 batch_first=True, norm_first=True)
        self.out = nn.Linear(d, nt)
        self.d = d
    def forward(self, s, t):
        sm = s == 0
        tm = nn.Transformer.generate_square_subsequent_mask(t.size(1), device=t.device)
        h = self.tf(self.pe(self.se(s) * math.sqrt(self.d)),
                    self.pe(self.te(t) * math.sqrt(self.d)),
                    tgt_mask=tm, src_key_padding_mask=sm,
                    memory_key_padding_mask=sm, tgt_is_causal=True)
        return self.out(h)

    @torch.no_grad()
    def greedy(self, s, bos, eos, maxlen):
        """Batched greedy decode. Still sequential (autoregressive), but only
        at inference, and over a whole batch at once."""
        B = s.size(0)
        cur = torch.full((B, 1), bos, device=s.device, dtype=torch.long)
        done = torch.zeros(B, dtype=torch.bool, device=s.device)
        for _ in range(maxlen - 1):
            nxt = self(s, cur)[:, -1].argmax(-1)
            nxt = torch.where(done, torch.zeros_like(nxt), nxt)
            cur = torch.cat([cur, nxt.unsqueeze(1)], 1)
            done |= nxt == eos
            if bool(done.all()): break
        return cur
