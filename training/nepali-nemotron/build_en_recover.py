#!/usr/bin/env python3
"""Build resources/translit/en_recover.tsv — the English-recovery target table.

English words spoken inside a Nepali utterance come back from Nemotron written
in Devanagari (it picks one language per utterance and picks Hindi when the
utterance is mostly Nepali). `translit.rs` then romanises that Devanagari
phonetically, so "speaking" arrives as `spiking` and "between" as `bitwin`.

This table is the target side of the recovery: a phonetic consonant skeleton →
the English words that share it, ranked by how often *he* has typed them.

Two tiers, both restricted to words he actually writes (roman_freq.tsv, mined
from 18,452 of his WhatsApp messages), so the recovery can only ever produce
vocabulary that is already his:

  tier 1  his English vocabulary, validated against /usr/share/dict/web2,
          with a skeleton of >= 3 consonants. Long skeletons carry enough
          phonetic evidence to identify a word on their own.
  tier 2  a closed list of core English function/content words, at any
          skeleton length. Short skeletons are ambiguous, so the set that may
          use them is hand-fixed rather than mined.

Excluded from tier 1: proper names, and any token that is also the romanisation
of a real Nepali word (`nep_support > 0` in dev_roman.tsv) — those are the
tokens that would let a Nepali word be "corrected" into another Nepali word.

Usage: python3 training/nepali-nemotron/build_en_recover.py
"""
import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RES = ROOT / "resources" / "translit"
WEB2 = Path("/usr/share/dict/web2")
PROPER = Path("/usr/share/dict/propernames")

VOWELS = set("aeiou")


def key_english(w):
    """Phonetic consonant skeleton of an English word.

    Must agree with `english_recovery::key_english` in the Rust source.
    """
    out = []
    i = 0
    n = len(w)
    while i < n:
        c = w[i]
        two = w[i : i + 2]
        if two == "ch":
            out.append("C"); i += 2; continue
        if two == "sh":
            out.append("S"); i += 2; continue
        if two == "ph":
            out.append("F"); i += 2; continue
        if two == "th":
            out.append("T"); i += 2; continue
        if two == "gh":
            i += 2; continue                      # silent: night, though
        if two == "wh":
            out.append("V"); i += 2; continue
        if two == "ck":
            out.append("K"); i += 2; continue
        if two == "qu":
            out.append("K"); out.append("V"); i += 2; continue
        if i == 0 and two == "kn":
            out.append("N"); i += 2; continue
        if i == 0 and two == "wr":
            out.append("R"); i += 2; continue
        if c in VOWELS:
            i += 1; continue
        if c == "y":
            if i == 0:
                out.append("Y")
            i += 1; continue
        if c == "h":
            if i == 0:
                out.append("H")
            i += 1; continue
        if c == "c":
            out.append("S" if w[i + 1 : i + 2] in ("e", "i", "y") else "K")
            i += 1; continue
        if c == "x":
            out.append("K"); out.append("S"); i += 1; continue
        if c in ("w", "v"):
            out.append("V"); i += 1; continue
        if c == "q":
            out.append("K"); i += 1; continue
        if c == "z":
            out.append("J"); i += 1; continue
        if c.isalpha():
            out.append(c.upper()); i += 1; continue
        i += 1
    packed = []
    for ch in out:
        if not packed or packed[-1] != ch:
            packed.append(ch)
    return "".join(packed)


# Core English. Short skeletons are ambiguous, so the words allowed to claim one
# are enumerated by hand instead of mined. Every entry still has to appear in his
# own vocabulary to make the table.
CORE = """a about above after again against all almost alone along already also although always am an and another any anyone
anything are around as ask at back bad be because been before being below best better between big both but by call came can
cannot come could day did different do does doing done down during each early easy either else end enough even ever every
everyone everything example far fast few find first for found free from front full get give go going good got great group had
half hand happy hard has have he help her here high him his hold home hour how however if important in into is it its just keep
kind know large last late later least leave left less let life like little live long look lot love made make man many may maybe
me mean might mine more most move much must my myself name near need never new next nice night no not nothing now number of off
often okay old on once one only open or order other our out over own part people perhaps place please point possible probably
problem put quite rather read ready real really right room said same saw say school second see seem send set she should show
side since small so some someone something sometimes soon sorry sound speak speaking start still stop such sure take talk tell
than thank thanks that the their them then there these they thing think this those though thought three through time to today
together too took top true try turn two under until up upon us use used usually very want was way we week well went were what
when where whether which while who whole why will with within without word work world would write wrong year yes yesterday yet
you young your""".split()


def load_tsv(path):
    out = {}
    with path.open() as fh:
        for line in fh:
            k, _, v = line.rstrip("\n").partition("\t")
            out[k] = v
    return out


def is_english(word, web2):
    if word in web2:
        return True
    for suf, base in (
        ("ing", ""), ("ing", "e"), ("ed", ""), ("ed", "e"), ("s", ""),
        ("es", ""), ("ly", ""), ("er", ""), ("est", ""), ("ies", "y"), ("ied", "y"),
    ):
        if not word.endswith(suf):
            continue
        stem = word[: -len(suf)] + base
        if len(stem) >= 3 and stem in web2:
            return True
        if len(stem) >= 4 and stem[-1] == stem[-2] and stem[:-1] in web2:
            return True
    return False


def main():
    if not WEB2.exists():
        sys.exit(f"missing {WEB2} — this generator needs the system word list")
    web2 = {l.strip().lower() for l in WEB2.open()}
    proper = {l.strip().lower() for l in PROPER.open()} if PROPER.exists() else set()

    freq = {k: int(v) for k, v in load_tsv(RES / "roman_freq.tsv").items()}
    nep_support = defaultdict(int)
    for roman in load_tsv(RES / "dev_roman.tsv").values():
        nep_support[roman] += 1

    table = defaultdict(dict)
    t1 = t2 = 0
    for word, f in freq.items():
        if len(word) < 3 or not word.isalpha():
            continue
        if nep_support[word] > 0 or word in proper:
            continue
        if not is_english(word, web2):
            continue
        k = key_english(word)
        if len(k) < 3:
            continue
        table[k][word] = max(table[k].get(word, 0), f)
        t1 += 1
    for word in CORE:
        f = freq.get(word, 0)
        if f == 0:
            continue
        k = key_english(word)
        if not k:
            continue
        if word not in table[k]:
            t2 += 1
        table[k][word] = max(table[k].get(word, 0), f)

    out = RES / "en_recover.tsv"
    lines = []
    for k in sorted(table):
        cands = sorted(table[k].items(), key=lambda t: (-t[1], t[0]))
        lines.append(k + "\t" + " ".join(f"{w},{f}" for w, f in cands) + "\n")
    out.write_text("".join(lines))
    total = sum(len(v) for v in table.values())
    print(f"wrote {out}: {len(table)} keys, {total} targets (tier1 {t1}, tier2 +{t2})")


if __name__ == "__main__":
    main()
