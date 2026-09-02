"""
Devanagari -> Abhi's Roman Nepali.

The point of this file is NOT generic transliteration. Standard schemes
(ITRANS/IAST/ISO-15919) produce `hunchha`, `bhayo`, `chha`. Abhi writes
`hunxa`, `vayo`, `xa`. Training on standard output would teach the model
someone else's spelling.

Conventions derived from 17,604 of his WhatsApp messages (787 Roman-Nepali
tokens, freq >= 15). The two that dominate:

    छ  / च्छ  ->  x     xa 1451, hunxa 1132, parxa 516, xaina 392, raixa 566
    भ        ->  v     vayo 790, vanera 478, vako 387, vaneko 385, voli 279

Everything else follows a light ITRANS with schwa deletion, which is what
his other tokens (garna, garne, timro, malai, dherai) already look like.
"""

# Independent vowels
V = {
    "अ":"a","आ":"aa","इ":"i","ई":"i","उ":"u","ऊ":"u","ए":"e","ऐ":"ai",
    "ओ":"o","औ":"au","ऋ":"ri","अं":"an","अः":"ah",
}
# Dependent vowel signs (matras)
M = {
    "ा":"aa","ि":"i","ी":"i","ु":"u","ू":"u","े":"e","ै":"ai",
    "ो":"o","ौ":"au","ृ":"ri","ं":"n","ँ":"n","ः":"h","्":"",
}
# Consonants, WITH Abhi's overrides applied
C = {
    "क":"k","ख":"kh","ग":"g","घ":"gh","ङ":"n",
    "च":"c","छ":"x","ज":"j","झ":"jh","ञ":"n",      # छ -> x  (his)
    "ट":"t","ठ":"th","ड":"d","ढ":"dh","ण":"n",
    "त":"t","थ":"th","द":"d","ध":"dh","न":"n",
    "प":"p","फ":"ph","ब":"b","भ":"v","म":"m",      # भ -> v  (his)
    "य":"y","र":"r","ल":"l","व":"w","श":"s","ष":"s","स":"s","ह":"h",
    "क्ष":"chh","त्र":"tr","ज्ञ":"gy","ड़":"r","ढ़":"rh","फ़":"f","ज़":"z",
}
DIGITS = {d:str(i) for i,d in enumerate("०१२३४५६७८९")}

# His actual spellings, mined from WhatsApp. Authority for any word he has
# written; the rules above only handle words he never used.
import json as _json, os as _os
_VP = _os.environ.get("ABHI_VOCAB", "/private/tmp/claude-501/-Users-abhi-proj-always/4a95cd0a-1cd0-4d0c-a5dd-0eebdd6e8956/scratchpad/vocab_all.json")
try:
    VOCAB = _json.load(open(_VP))
except Exception:
    VOCAB = {}

VOWEL_SIGNS = set("ािीुूेैोौृ")
INDEP_V = set("अआइईउऊएऐओऔऋ")

def _syllables(w: str):
    """
    Split Devanagari into (consonant_cluster, vowel) pairs.

    Working on syllables rather than characters is the whole point: `gh`,
    `bh`, `kh` are two Latin chars but ONE consonant, so an index-based
    rule deletes the wrong vowel (ghar -> ghra). vowel is "" after a
    virama, the matra value, or "\x01" for an unwritten inherent schwa.
    """
    syl, i = [], 0
    while i < len(w):
        ch = w[i]
        if ch in DIGITS:
            syl.append((DIGITS[ch], "")); i += 1; continue
        if ch in C:
            cons, i = C[ch], i + 1
            # conjuncts: virama binds this consonant to the next
            while i + 1 < len(w) and w[i] == "\u094d" and w[i+1] in C:
                cons += C[w[i+1]]; i += 2
            if i < len(w) and w[i] == "\u094d":      # trailing virama: no vowel
                syl.append((cons, "")); i += 1; continue
            if i < len(w) and w[i] == "\u093e":      # `ा`
                nxt = w[i+1] if i+1 < len(w) else ""
                if nxt in ("\u0908", "\u0907"):     # ाई / ाइ -> ai
                    syl.append((cons, "ai")); i += 2; continue
                syl.append((cons, "\u0002")); i += 1; continue   # \x02 = `ा`, resolved later
            if i < len(w) and w[i] in M:
                syl.append((cons, M[w[i]])); i += 1; continue
            syl.append((cons, "\x01")); continue      # inherent schwa
        if ch in V:
            syl.append(("", V[ch])); i += 1; continue
        syl.append((ch, "")); i += 1
    return syl

def translit_word(w: str) -> str:
    syl = _syllables(w)
    # `ा` is `aa` only when it carries the word's last vowel (kaam, maa);
    # `a` when a later syllable has one (ramro, bato).
    for k, (c, v) in enumerate(syl):
        if v == "\x02":
            later = any(vv not in ("", "\x01") for _, vv in syl[k+1:])
            syl[k] = (c, "a" if later else "aa")
    # Medial schwa deletion: an inherent schwa drops when its syllable is
    # neither first nor last and the FOLLOWING syllable carries a vowel.
    #   मिलको  mi-la-ko -> milko      काङ्ग्रेसको -> kangresko
    for k in range(1, len(syl) - 1):
        c, v = syl[k]
        if v == "\x01" and syl[k+1][1] not in ("",):
            syl[k] = (c, "")
    s = "".join(c + (v if v != "\x01" else "a") for c, v in syl)
    if VOCAB.get(s, 0) > 0:
        return s
    cands = [s]
    if s.endswith("a") and len(s) > 2: cands.append(s[:-1])
    if s.endswith("aa"): cands.append(s[:-1])
    elif s.endswith("a"): cands.append(s + "a")
    seen = {c: VOCAB.get(c, 0) for c in dict.fromkeys(cands)}
    best = max(seen, key=lambda c: seen[c])
    return best if seen[best] > 0 else s

def translit(text: str) -> str:
    return " ".join(translit_word(w) for w in text.split())

if __name__ == "__main__":
    # Ground truth: Devanagari -> what Abhi actually types (from WhatsApp freq)
    CASES = [
        ("छ","xa"), ("हुन्छ","hunxa"), ("छैन","xaina"), ("पर्छ","parxa"),
        ("भयो","vayo"), ("भनेर","vanera"), ("भएको","vaeko"), ("भन्ने","vanne"),
        ("गर्न","garna"), ("गर्ने","garne"), ("गर्नु","garnu"),
        ("मलाई","malai"), ("तिमी","timi"), ("तिम्रो","timro"),
        ("धेरै","dherai"), ("राम्रो","ramro"), ("घर","ghar"), ("काम","kaam"),
    ]
    ok = 0
    for dev, want in CASES:
        got = translit_word(dev)
        mark = "OK " if got == want else "MISS"
        if got == want: ok += 1
        print(f"  {mark} {dev:<10} -> {got:<12} (want {want})")
    print(f"\n{ok}/{len(CASES)} exact  ({ok/len(CASES)*100:.0f}%)")
