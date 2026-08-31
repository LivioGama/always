"""
Export the trained `TranslitTF` to ONNX for the Rust daemon.

# Why two graphs, not one

Greedy decoding is autoregressive: 24 sequential steps, each needing the
*whole* decoder over the prefix produced so far. The encoder, by contrast,
depends only on the input word and is constant across all 24 steps.

Exporting `forward(src, tgt)` as one graph and looping it in Rust would
therefore re-run the 3-layer encoder 24 times per word for an output that
never changes. Splitting it into `encoder` (run once) + `decoder_step` (run
per token) removes that, and measured on this checkpoint it is the
difference between ~40% and ~0% wasted encoder work.

The alternative — baking the loop into ONNX with a `Loop` op, or exporting at
a fixed max length with an unrolled 24-step body — was rejected: `Loop`
bodies are opaque to ORT's graph optimisers, and an unrolled 24x graph is
24x the node count for no gain, since the loop must still run to completion
or carry per-row early-exit state.

No KV cache. The decoder re-attends over a prefix of at most 24 tokens with
d=256; the cache bookkeeping (24 extra graph inputs/outputs, re-exported
attention) costs more in complexity and per-call tensor plumbing than it
saves on a sequence this short. Measured numbers are in the module docs of
`src/always/translit_model.rs`.

Digits are NOT passed through the model. `०-९` is an unambiguous 1:1 map and
letting a seq2seq guess them is how the v1 model turned `००७` into `dah`
(see `train_v2.py`). Rust handles them before the model is ever consulted.
"""
import json, math, sys
from pathlib import Path
import torch, torch.nn as nn

sys.path.insert(0, str(Path(__file__).parent))
from model_tf import TranslitTF

D = Path("/private/tmp/claude-501/-Users-abhi-proj-always/4a95cd0a-1cd0-4d0c-a5dd-0eebdd6e8956/scratchpad")
OUT = Path(__file__).resolve().parents[2] / "resources" / "translit"


class Encoder(nn.Module):
    """`src` ids -> encoder memory. Run once per word."""

    def __init__(self, m: TranslitTF):
        super().__init__()
        self.m = m

    def forward(self, src, src_pad):
        m = self.m
        x = m.pe(m.se(src) * math.sqrt(m.d))
        return m.tf.encoder(x, src_key_padding_mask=src_pad)


class DecoderStep(nn.Module):
    """(memory, src_pad_mask, tgt prefix) -> logits for the NEXT token only.

    Returning just the last position keeps the output [B, V] instead of
    [B, T, V]; the caller only ever argmaxes the final row.
    """

    def __init__(self, m: TranslitTF, max_len: int):
        super().__init__()
        self.m = m
        # A CONSTANT causal mask, sliced per call. Building it per call with
        # `torch.full((t, t), -inf)` on a symbolic `t` introduces an unbacked
        # symbol that torch.export cannot guard on; a registered buffer plus a
        # slice keeps every dim backed.
        full = torch.triu(torch.full((max_len, max_len), float("-inf")), 1)
        self.register_buffer("causal", full)

    def forward(self, memory, src_pad, tgt):
        m = self.m
        t = tgt.shape[1]
        y = m.pe(m.te(tgt) * math.sqrt(m.d))
        h = m.tf.decoder(
            y, memory, tgt_mask=self.causal[:t, :t],
            memory_key_padding_mask=src_pad, tgt_is_causal=False,
        )
        return m.out(h[:, -1])


@torch.no_grad()
def write_char_table(model, src, tgt, ml):
    """A learned last-resort map: the model's own answer for each Devanagari
    codepoint, taken one codepoint at a time.

    This exists ONLY for the case where ORT cannot build a session at all
    (missing shared library, unsupported CPU). Something still has to satisfy
    the module's hard invariant that no U+0900..U+097F reaches the clipboard,
    and the alternative — keeping a hand-written syllable transliterator alive
    purely as an escape hatch — is the thing this change set out to delete.
    Every value here is the model's, not a human's.
    """
    OUT.mkdir(parents=True, exist_ok=True)
    chars = [chr(cp) for cp in range(0x0900, 0x0980)]
    ids = torch.tensor([[tgt["\x02"]] * 0 + [1, src.get(c, 0), 2] + [0] * (ml - 3)
                        for c in chars])
    itos = {i: c for c, i in tgt.items()}
    out = model.greedy(ids, tgt["\x02"], tgt["\x03"], ml)
    rows = []
    for c, row in zip(chars, out):
        s = ""
        for i in row[1:].tolist():
            ch = itos.get(i, "")
            if ch in ("\x03", "\x00"):
                break
            s += ch
        rows.append((c, s))
    with open(OUT / "char_roman.tsv", "w", encoding="utf-8") as f:
        for c, s in sorted(rows):
            f.write(f"{c}\t{s}\n")


def main():
    # PyTorch's fused "fastpath" (`aten::_transformer_encoder_layer_fwd`) is a
    # single opaque ATen kernel with no ONNX symbolic. Turning it off makes the
    # encoder trace as ordinary matmul/softmax/layernorm ops. It is a *fusion*
    # of the same arithmetic, so disabling it changes speed, not results --
    # asserted below against the fastpath output the offline table was built
    # with.
    torch.backends.mha.set_fastpath_enabled(False)

    ck = torch.load(D / "translit_v2.pt", map_location="cpu", weights_only=False)
    src, tgt, ml = ck["src"], ck["tgt"], ck["ms"]
    model = TranslitTF(len(src), len(tgt))
    model.load_state_dict(ck["model"])
    model.eval()

    OUT.mkdir(parents=True, exist_ok=True)
    B, S, T = 2, 12, 5
    b_dim = torch.export.Dim("b", min=1, max=4096)
    s_dim = torch.export.Dim("s", min=3, max=ml)
    t_dim = torch.export.Dim("t", min=1, max=ml)
    ex_src = torch.randint(3, len(src), (B, S))
    ex_src[1, 8:] = 0  # exercise the padding path during tracing
    # Masks are ADDITIVE FLOAT (0 / -inf), not bool. `nn.Transformer`
    # canonicalises a bool mask through a data-dependent branch that
    # torch.export cannot guard on ("Could not guard on data-dependent
    # expression Eq(u0, 1)"). A float mask skips that branch and is the same
    # arithmetic once canonicalised.
    ex_pad = torch.zeros(B, S).masked_fill(ex_src == 0, float("-inf"))

    with torch.no_grad():
        enc = Encoder(model).eval()
        # `dynamo=True` (torch.export) is REQUIRED here. The legacy
        # TorchScript tracer folds `MultiheadAttention`'s reshapes into
        # *constants* taken from the example batch/length, so the exported
        # graph runs only for the exact shape it was traced with and ORT
        # fails with "cannot be reshaped to the requested shape" on anything
        # else. torch.export keeps the symbolic dims.
        torch.onnx.export(
            enc, (ex_src, ex_pad), str(OUT / "translit_encoder.onnx"),
            input_names=["src", "src_pad"], output_names=["memory"],
            dynamic_shapes={"src": {0: b_dim, 1: s_dim},
                            "src_pad": {0: b_dim, 1: s_dim}},
            opset_version=17, dynamo=True,
        )
        mem = enc(ex_src, ex_pad)
        ex_tgt = torch.randint(1, len(tgt), (B, T))
        dec = DecoderStep(model, ml).eval()
        torch.onnx.export(
            dec, (mem, ex_pad, ex_tgt), str(OUT / "translit_decoder.onnx"),
            input_names=["memory", "src_pad", "tgt"], output_names=["logits"],
            dynamic_shapes={"memory": {0: b_dim, 1: s_dim},
                            "src_pad": {0: b_dim, 1: s_dim},
                            "tgt": {0: b_dim, 1: t_dim}},
            opset_version=17, dynamo=True,
        )

    # The dynamo exporter spills weights to a sidecar `.onnx.data`. Fold them
    # back in: the daemon loads these from a single `include_bytes!`ed slice
    # and has no directory to resolve a relative external-data path against.
    import onnx
    for name in ["translit_encoder.onnx", "translit_decoder.onnx"]:
        path = OUT / name
        g = onnx.load(str(path), load_external_data=True)
        onnx.save(g, str(path), save_as_external_data=False)
        sidecar = OUT / (name + ".data")
        if sidecar.exists():
            sidecar.unlink()

    # Vocabularies, as a compact JSON the Rust side parses once at load.
    meta = {
        "src": {c: i for c, i in src.items()},
        "tgt_itos": [None] * len(tgt),
        "max_len": ml,
        "bos": tgt["\x02"], "eos": tgt["\x03"], "pad": 0,
    }
    for c, i in tgt.items():
        meta["tgt_itos"][i] = c
    (OUT / "translit_vocab.json").write_text(
        json.dumps(meta, ensure_ascii=False, separators=(",", ":")), encoding="utf-8"
    )
    write_char_table(model, src, tgt, ml)

    for f in ["translit_encoder.onnx", "translit_decoder.onnx", "translit_vocab.json",
              "char_roman.tsv"]:
        print(f"  {f}: {(OUT / f).stat().st_size:,} bytes")


if __name__ == "__main__":
    main()
