//! Devanagari → Abhi's Roman Nepali, on the paste path.
//!
//! # Why this exists
//!
//! Nemotron 3.5 ASR supports 40 language-locales and **Nepali is not one of
//! them**. `ne-NP` occupies prompt slot 46 in parakeet-rs but its embedding is
//! untrained — it returns empty for every input. So when the user speaks
//! Nepali, or code-switches mid-sentence, the model resolves the
//! Devanagari-ish acoustics onto the nearest locale it *does* know: Hindi
//! (`hi-IN`, slot 6). The result is Devanagari script in the middle of English
//! dictation:
//!
//! ```text
//! अच्छी बात है, एक स्पीकिंग ना चाह इट गोस
//! ```
//!
//! `cfg.lang = "en"` does not prevent this. The language prompt *biases*
//! decoding; it does not constrain the output alphabet. Setting it correctly
//! (which `apply_nemotron_language` does) is necessary but not sufficient.
//!
//! The user writes Nepali in Latin script, always — `hunxa`, `vayo`, `maa` —
//! and never wants Devanagari pasted into his editor. This module is the last
//! step before that paste: it rewrites Devanagari runs into his romanisation
//! and leaves every other byte alone.
//!
//! # How it romanises
//!
//! Two tiers, in order:
//!
//! 1. **Exact lookup** (`resources/translit/dev_roman.tsv`, 49,064 entries).
//!    Built offline from the trained transliteration model
//!    (`training/nepali-nemotron/train_v2.py`, 99.0% exact match on held-out
//!    pairs of his own spellings) applied to the 157,905-utterance SLR54
//!    Nepali corpus, then word-aligned against the Devanagari source. The
//!    1,844 pairs mined directly from his WhatsApp messages override the model
//!    wherever the two disagree (18 entries). This tier carries the words he
//!    actually says.
//!
//! 2. **Rule fallback** for anything the table does not have — a syllable-level
//!    transliterator carrying his two non-standard conventions (`छ → x`,
//!    `भ → v`; he writes `hunxa`/`xaina` and `vayo`/`vanera`, not `hunchha`
//!    and `bhayo`), right-to-left schwa deletion, and a re-ranking pass against
//!    the 9,730 Roman tokens he has actually typed (`roman_freq.tsv`).
//!    Measured 39.0% exact against tier 1 used as held-out gold. Tier 2 exists
//!    to produce *readable Latin*, not to be right; tier 1 is what is right.
//!
//! # The invariant
//!
//! **No character in U+0900..U+097F may survive `romanize`.** Every branch of
//! the rule engine either maps a Devanagari codepoint or drops it. An unmapped
//! codepoint reaching the output would silently defeat the entire point of the
//! module, so it is asserted over all 49,064 dictionary keys and over every
//! codepoint in the block — see the `no_devanagari_survives_*` tests.
//!
//! # Cost
//!
//! Both tables are `include_str!`-ed, kept in byte-sorted order, and
//! **binary-searched in place**. There is no map to build, so there is no
//! lazy-init stall on the first Nepali utterance and no heap cost for the
//! overwhelmingly common English-only session. Text with no Devanagari returns
//! `Cow::Borrowed` after a single scan and allocates nothing.
//!
//! # English inside the Nepali
//!
//! Romanising faithfully is exactly wrong for the English words in a
//! code-switched utterance: Nemotron writes them in Devanagari too, so
//! `स्पीकिंग` romanises to `spiking` when it means "speaking". Each romanised
//! word is therefore handed to [`crate::always::english_recovery`], which
//! restores the English spelling when it can prove it and does nothing
//! otherwise. It only ever sees words that came out of a Devanagari run, so
//! Latin-only input still returns `Cow::Borrowed` having touched nothing.
//!
//! Escape hatches: `ALWAYS_NO_ROMANIZE=1` disables the transform entirely,
//! `ALWAYS_NO_ENGLISH_RECOVERY=1` disables only the recovery pass.

use std::borrow::Cow;

/// Devanagari → his Roman spellings, one `dev\troman\n` record per line,
/// byte-sorted by key. See module docs for provenance.
static DEV_ROMAN: &str = include_str!("../../resources/translit/dev_roman.tsv");

/// Roman tokens he has actually typed, `word\tcount\n`, byte-sorted. Used only
/// to re-rank schwa variants for out-of-vocabulary words.
static ROMAN_FREQ: &str = include_str!("../../resources/translit/roman_freq.tsv");

/// Unicode Devanagari block. The whole point of this module is that nothing in
/// this range reaches the clipboard.
const DEV_START: char = '\u{0900}';
const DEV_END: char = '\u{097F}';

const VIRAMA: char = '\u{094D}';
const ZWNJ: char = '\u{200C}';
const ZWJ: char = '\u{200D}';

/// Sentinel for `ा`, whose Latin value depends on where it lands in the word.
const AA_PENDING: &str = "\u{2}";
/// Sentinel for an unwritten inherent schwa.
const SCHWA: &str = "\u{1}";

/// True if `c` is in the Devanagari block.
#[inline]
pub fn is_devanagari(c: char) -> bool {
    (DEV_START..=DEV_END).contains(&c)
}

/// Devanagari punctuation that ends a word rather than belonging to one:
/// danda, double danda, and the abbreviation signs. These live in the same
/// Unicode block as the letters, so without this they would be swallowed into
/// the token being looked up and `हो।` would resolve to `ho`, silently losing
/// the sentence break. Dictation output is punctuation-sensitive; it stays.
#[inline]
fn is_devanagari_punctuation(c: char) -> bool {
    matches!(c, '।' | '॥' | '॰' | 'ॱ')
}

/// The Latin this punctuation stands in for. The abbreviation signs have no
/// English equivalent and are dropped.
#[inline]
fn punctuation_latin(c: char) -> &'static str {
    match c {
        '।' | '॥' => ".",
        _ => "",
    }
}

/// True if `text` contains any Devanagari at all. This is the fast-path guard:
/// English-only dictation pays one scan and nothing else.
#[inline]
pub fn contains_devanagari(text: &str) -> bool {
    text.chars().any(is_devanagari)
}

// ---------------------------------------------------------------------------
// Static table lookup
// ---------------------------------------------------------------------------

/// Binary-search a byte-sorted `key\tvalue\n` blob without building a map.
///
/// A probe lands at an arbitrary byte, so it snaps backwards to the start of
/// the record it landed in and compares whole records. That is why the loop
/// carries byte bounds but never compares partial keys.
pub(crate) fn lookup_sorted<'a>(blob: &'a str, key: &str) -> Option<&'a str> {
    let bytes = blob.as_bytes();
    let (mut lo, mut hi) = (0usize, bytes.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let mut start = mid;
        while start > lo && bytes[start - 1] != b'\n' {
            start -= 1;
        }
        let end = match bytes[start..].iter().position(|&b| b == b'\n') {
            Some(off) => start + off,
            None => bytes.len(),
        };
        let (k, v) = match blob[start..end].split_once('\t') {
            Some(kv) => kv,
            None => return None,
        };
        match k.cmp(key) {
            std::cmp::Ordering::Equal => return Some(v),
            std::cmp::Ordering::Less => {
                let next = end + 1;
                if next <= lo {
                    return None;
                }
                lo = next;
            }
            std::cmp::Ordering::Greater => {
                if start <= lo {
                    return None;
                }
                hi = start;
            }
        }
    }
    None
}

/// His spelling for `word`, if he — or the model trained on him — has one.
fn dictionary(word: &str) -> Option<&'static str> {
    lookup_sorted(DEV_ROMAN, word)
}

/// How often he has typed this Roman token. 0 = never.
pub(crate) fn roman_freq(word: &str) -> u32 {
    lookup_sorted(ROMAN_FREQ, word)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Rule engine (fallback for words the table does not have)
// ---------------------------------------------------------------------------

/// Independent vowel letters.
fn independent_vowel(c: char) -> Option<&'static str> {
    Some(match c {
        'अ' => "a",
        'आ' => "aa",
        'इ' | 'ई' => "i",
        'उ' | 'ऊ' => "u",
        'ए' | 'ऍ' | 'ऎ' => "e",
        'ऐ' => "ai",
        'ओ' | 'ऑ' | 'ऒ' => "o",
        'औ' => "au",
        'ऋ' | 'ॠ' => "ri",
        'ऌ' | 'ॡ' => "li",
        _ => return None,
    })
}

/// Dependent vowel signs (matras).
fn matra(c: char) -> Option<&'static str> {
    Some(match c {
        'ा' => AA_PENDING,
        'ि' | 'ी' => "i",
        'ु' | 'ू' => "u",
        'े' | 'ॆ' | 'ॅ' => "e",
        'ै' => "ai",
        'ो' | 'ॊ' | 'ॉ' => "o",
        'ौ' => "au",
        'ृ' | 'ॄ' => "ri",
        _ => return None,
    })
}

/// Consonants, **with his overrides applied**: `छ → x` and `भ → v`. A standard
/// transliterator emits `chha` and `bha`; those are the two commonest
/// consonants in his Nepali and he writes neither of them that way.
fn consonant(c: char) -> Option<&'static str> {
    Some(match c {
        'क' | '\u{0958}' => "k",
        'ख' | '\u{0959}' => "kh",
        'ग' | '\u{095A}' => "g",
        'घ' => "gh",
        'ङ' => "n",
        'च' => "c",
        'छ' => "x",
        'ज' => "j",
        '\u{095B}' => "z",
        'झ' => "jh",
        'ञ' => "n",
        'ट' => "t",
        'ठ' => "th",
        'ड' => "d",
        '\u{095C}' => "r",
        'ढ' => "dh",
        '\u{095D}' => "rh",
        'ण' => "n",
        'त' => "t",
        'थ' => "th",
        'द' => "d",
        'ध' => "dh",
        'न' | 'ऩ' => "n",
        'प' => "p",
        'फ' => "ph",
        '\u{095E}' => "f",
        'ब' => "b",
        'भ' => "v",
        'म' => "m",
        'य' | '\u{095F}' => "y",
        'र' | 'ऱ' => "r",
        'ल' | 'ळ' | 'ऴ' => "l",
        'व' => "w",
        'श' | 'ष' | 'स' => "s",
        'ह' => "h",
        _ => return None,
    })
}

/// Devanagari digits → ASCII.
fn devanagari_digit(c: char) -> Option<char> {
    match c {
        '०'..='९' => char::from_u32(c as u32 - '०' as u32 + '0' as u32),
        _ => None,
    }
}

/// A consonant cluster plus the vowel that follows it.
///
/// Working on syllables rather than characters is what makes schwa deletion
/// tractable: `gh`, `bh`, `kh` are two Latin characters but one consonant, so a
/// character-indexed rule deletes the wrong vowel (`ghar` → `ghra`).
struct Syllable {
    onset: String,
    vowel: &'static str,
}

impl Syllable {
    fn new(onset: String, vowel: &'static str) -> Self {
        Self { onset, vowel }
    }
    /// A syllable "has a vowel" for schwa-deletion purposes when it carries any
    /// vowel at all, written or inherent.
    fn has_vowel(&self) -> bool {
        !self.vowel.is_empty()
    }
}

fn syllabify(word: &str) -> Vec<Syllable> {
    let chars: Vec<char> = word.chars().collect();
    let mut out: Vec<Syllable> = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];

        if let Some(d) = devanagari_digit(ch) {
            out.push(Syllable::new(d.to_string(), ""));
            i += 1;
            continue;
        }

        if let Some(c) = consonant(ch) {
            let mut onset = c.to_string();
            i += 1;
            // Conjuncts: a virama binds this consonant to the next.
            while i + 1 < chars.len() && chars[i] == VIRAMA {
                match consonant(chars[i + 1]) {
                    Some(next) => {
                        // च्छ is his `x`, not `cx` — the cluster is one sound.
                        if onset == "c" && next == "x" {
                            onset = "x".to_string();
                        } else {
                            onset.push_str(next);
                        }
                        i += 2;
                    }
                    None => break,
                }
            }
            if i < chars.len() && chars[i] == VIRAMA {
                // Trailing virama: the consonant carries no vowel.
                out.push(Syllable::new(onset, ""));
                i += 1;
                continue;
            }
            if i < chars.len() {
                if let Some(m) = matra(chars[i]) {
                    i += 1;
                    // ाई / ाइ is a single `ai`, not `aai`.
                    let mut v = m;
                    if v == AA_PENDING && i < chars.len() && (chars[i] == 'ई' || chars[i] == 'इ') {
                        v = "ai";
                        i += 1;
                    }
                    out.push(Syllable::new(onset, v));
                    continue;
                }
            }
            out.push(Syllable::new(onset, SCHWA));
            continue;
        }

        if let Some(v) = independent_vowel(ch) {
            out.push(Syllable::new(String::new(), v));
            i += 1;
            continue;
        }

        // Anusvara / candrabindu / visarga ride *after* the vowel they modify,
        // so each becomes its own zero-vowel syllable. An inherent schwa in
        // front of one is always written out: अंश is `ansa` → `ans`, never
        // `a-n-s` with the schwa silently dropped by the cluster rule.
        if ch == 'ं' || ch == 'ँ' || ch == 'ः' {
            if let Some(prev) = out.last_mut() {
                if prev.vowel == SCHWA {
                    prev.vowel = "a";
                }
            }
            let sound = if ch == 'ः' { "h" } else { "n" };
            out.push(Syllable::new(sound.to_string(), ""));
            i += 1;
            continue;
        }

        if ch == 'ॐ' {
            out.push(Syllable::new("om".to_string(), ""));
            i += 1;
            continue;
        }

        if ch == '।' || ch == '॥' {
            out.push(Syllable::new(".".to_string(), ""));
            i += 1;
            continue;
        }

        if ch == ZWJ || ch == ZWNJ || ch == VIRAMA {
            i += 1;
            continue;
        }

        if is_devanagari(ch) {
            // Nukta, stress marks, Vedic accents, unassigned. Dropping them is
            // deliberate: the invariant is that no Devanagari codepoint reaches
            // the clipboard, and there is no sensible Latin for these.
            i += 1;
            continue;
        }

        // Non-Devanagari caught inside a run — pass through verbatim.
        out.push(Syllable::new(ch.to_string(), ""));
        i += 1;
    }
    out
}

/// Resolve `ा`, delete schwas, flatten to Latin.
fn assemble(mut syl: Vec<Syllable>) -> String {
    // `ा` is `aa` only when it carries the word's last vowel AND that syllable
    // is closed (`kaam`, `naam`), or the word is a single syllable (`maa`,
    // `waa`). A word-final open `ा` is a plain `a`: he writes `salama`,
    // `durima`, `akalma`, not `salamaa`.
    let last_vowel = syl
        .iter()
        .rposition(|s| !s.vowel.is_empty() && s.vowel != SCHWA);
    let vowel_bearing = syl.iter().filter(|s| s.has_vowel()).count();
    let n = syl.len();
    for k in 0..n {
        if syl[k].vowel == AA_PENDING {
            let closed = k + 1 < n;
            let mono = vowel_bearing == 1;
            syl[k].vowel = if last_vowel == Some(k) && (closed || mono) {
                "aa"
            } else {
                "a"
            };
        }
    }

    // Schwa deletion, right to left — which is what makes it alternate
    // correctly. अकबर a-kə-bə-rə: drop the final schwa, then keep `bə` (its
    // right neighbour is now vowel-less), then drop `kə` → `akbar`. A
    // left-to-right pass drops both and yields `akbra`.
    //
    // Two-syllable words are left alone entirely: Nepali keeps its final schwa
    // (`garna`, `huna`, `dina`, `tara`) and deleting inside a word that short
    // destroys it.
    if n > 2 {
        if syl[n - 1].vowel == SCHWA {
            syl[n - 1].vowel = "";
        }
        for k in (1..n - 1).rev() {
            if syl[k].vowel == SCHWA && !syl[k + 1].vowel.is_empty() {
                syl[k].vowel = "";
            }
        }
    }

    let mut out = String::with_capacity(n * 3);
    for s in &syl {
        out.push_str(&s.onset);
        out.push_str(if s.vowel == SCHWA { "a" } else { s.vowel });
    }
    out
}

/// Rule-transliterate one Devanagari word, then re-rank the obvious schwa
/// variants against tokens he has actually typed. The rules cannot know whether
/// he writes `hune` or `hunee`; his own writing can.
fn rule_word(word: &str) -> String {
    let base = assemble(syllabify(word));
    if base.is_empty() || roman_freq(&base) > 0 {
        return base;
    }
    let mut candidates: Vec<String> = Vec::with_capacity(2);
    if base.ends_with("aa") {
        candidates.push(base[..base.len() - 1].to_string());
    } else if base.ends_with('a') {
        if base.len() > 2 {
            candidates.push(base[..base.len() - 1].to_string());
        }
        candidates.push(format!("{base}a"));
    }
    let mut best = base;
    let mut best_freq = 0u32;
    for c in candidates {
        let f = roman_freq(&c);
        if f > best_freq {
            best_freq = f;
            best = c;
        }
    }
    best
}

/// His spelling for one Devanagari word: table first, rules second.
fn romanize_word(word: &str) -> String {
    match dictionary(word) {
        Some(hit) => hit.to_string(),
        None => rule_word(word),
    }
}

/// His spelling for one Devanagari word, with an English word recovered from it
/// if that is what it really was.
///
/// Nemotron picks one language per utterance, so the English words inside a
/// mostly-Nepali sentence come back spelled phonetically in Devanagari and
/// romanise to `spiking`, `inglis`, `bitwin`. Recovery only ever sees tokens
/// that came out of a Devanagari run, which is why English dictation — where
/// there is no run at all — never pays for it. See
/// `crate::always::english_recovery`.
fn romanize_word_recovered(word: &str, recover: bool) -> String {
    let roman = romanize_word(word);
    if !recover {
        return roman;
    }
    match crate::always::english_recovery::recover_word(&roman) {
        Some(english) => english.to_string(),
        None => roman,
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// True when the transform is switched off by the environment.
fn disabled() -> bool {
    matches!(
        std::env::var("ALWAYS_NO_ROMANIZE").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Rewrite every Devanagari run in `text` into his Roman Nepali, leaving all
/// other bytes exactly as they were.
///
/// This is deliberately *not* a translation and *not* a language detector. It
/// walks the string and substitutes a Latin spelling for each maximal run of
/// Devanagari. English words, punctuation, spacing, emoji and code pass through
/// byte-identical, which is what makes it safe to run on every utterance rather
/// than only the ones something flagged as Nepali — a code-switched sentence
/// comes out uniformly Latin.
///
/// Idempotent: the output contains no Devanagari, so a second call returns
/// `Cow::Borrowed` unchanged.
pub fn romanize(text: &str) -> Cow<'_, str> {
    if !contains_devanagari(text) || disabled() {
        return Cow::Borrowed(text);
    }
    // Read once per utterance, not once per word — this is the paste path.
    let recover = !crate::always::english_recovery::disabled();
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    // A run breaks on anything that is neither Devanagari nor an invisible
    // joiner. Joiners stay inside the run so a conjunct spelled with ZWJ does
    // not split a word in half.
    for c in text.chars() {
        if is_devanagari(c) && !is_devanagari_punctuation(c)
            || ((c == ZWJ || c == ZWNJ) && !run.is_empty())
        {
            run.push(c);
        } else {
            if !run.is_empty() {
                out.push_str(&romanize_word_recovered(&run, recover));
                run.clear();
            }
            if is_devanagari_punctuation(c) {
                out.push_str(punctuation_latin(c));
            } else {
                out.push(c);
            }
        }
    }
    if !run.is_empty() {
        out.push_str(&romanize_word_recovered(&run, recover));
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- English recovered out of a code-switched utterance -------------------

    #[test]
    fn romanize_recovers_english_written_in_devanagari() {
        // The reported utterance. Nemotron resolved the whole thing onto Hindi
        // because it was mostly Nepali, so the English in it came back in
        // Devanagari and used to paste as `end aai em spiking in inglis in
        // bitwin`. The Nepali beside it must come through untouched.
        let spoken = "एंड आई एम स्पीकिंग इन इंग्लिस इन बिटवीन एंड देखि नेपाली बोली अगेन";
        let got = romanize(spoken);
        assert!(!contains_devanagari(&got), "Devanagari survived: {got}");
        assert_eq!(
            got,
            "end aai am speaking in english in between end dekhi nepali boli again"
        );
    }

    #[test]
    fn romanize_stays_idempotent_with_recovery_on() {
        let spoken = "एंड आई एम स्पीकिंग इन इंग्लिस इन बिटवीन एंड देखि नेपाली बोली अगेन";
        let once = romanize(spoken).into_owned();
        assert!(matches!(romanize(&once), Cow::Borrowed(_)));
        assert_eq!(romanize(&once), once);
    }

    // -- the invariant -------------------------------------------------------

    #[test]
    fn no_devanagari_survives_any_dictionary_key() {
        for line in DEV_ROMAN.lines() {
            let (dev, _) = line.split_once('\t').expect("well-formed record");
            let got = romanize(dev);
            assert!(
                !contains_devanagari(&got),
                "Devanagari survived for {dev:?}: {got:?}"
            );
        }
    }

    #[test]
    fn no_devanagari_survives_any_codepoint() {
        // Including unassigned and Vedic codepoints the dictionary never sees.
        for cp in 0x0900u32..=0x097F {
            let c = char::from_u32(cp).unwrap();
            let input = format!("a {c} b");
            let got = romanize(&input);
            assert!(
                !contains_devanagari(&got),
                "Devanagari U+{cp:04X} survived: {got:?}"
            );
        }
    }

    #[test]
    fn tables_are_byte_sorted() {
        // Binary search is only correct while the files stay in byte order.
        for (name, blob) in [("dev_roman", DEV_ROMAN), ("roman_freq", ROMAN_FREQ)] {
            let mut prev = "";
            for line in blob.lines() {
                let (k, _) = line.split_once('\t').expect("well-formed record");
                assert!(prev <= k, "{name} out of order: {prev:?} then {k:?}");
                prev = k;
            }
        }
    }

    #[test]
    fn no_dictionary_key_swallows_punctuation() {
        // Regression: the table was first generated straight from the SLR54
        // alignment, which left 362 sentence-final keys like `होइनन्।` whose
        // Roman value had quietly dropped the danda. Looking `हो।` up as one
        // token then returned `ho` and the sentence break vanished.
        for line in DEV_ROMAN.lines() {
            let (k, _) = line.split_once('\t').expect("well-formed record");
            assert!(
                !k.chars().any(is_devanagari_punctuation),
                "key carries punctuation: {k:?}"
            );
        }
    }

    // -- the reported failure ------------------------------------------------

    #[test]
    fn the_utterance_that_was_pasted_into_his_editor() {
        // Verbatim from the report: Nemotron emitted this with lang=en.
        let got = romanize("अच्छी बात है, एक स्पीकिंग ना चाह इट गोस");
        assert!(!contains_devanagari(&got), "still Devanagari: {got}");
        assert!(got.is_ascii(), "not plain Latin: {got}");
        // Punctuation and spacing are structural — they must survive untouched.
        assert!(got.contains(", "), "punctuation lost: {got}");
        assert_eq!(
            got.split_whitespace().count(),
            9,
            "token count changed: {got}"
        );
        let words: Vec<&str> = got.split_whitespace().collect();
        assert_eq!(words[2].trim_end_matches(','), "hai");
        assert_eq!(words[3], "ek");
    }

    // -- tier 1: his own spellings ------------------------------------------

    #[test]
    fn his_conventions_win_over_standard_transliteration() {
        // Standard schemes give hunchha/chhaina/parchha and bhayo/bhanera.
        assert_eq!(romanize("हुन्छ"), "hunxa");
        assert_eq!(romanize("छैन"), "xaina");
        assert_eq!(romanize("पर्छ"), "parxa");
        assert_eq!(romanize("भयो"), "vayo");
        assert_eq!(romanize("भनेर"), "vanera");
        assert_eq!(romanize("मा"), "maa");
    }

    #[test]
    fn common_words_round_trip() {
        assert_eq!(romanize("गर्न"), "garna");
        assert_eq!(romanize("मलाई"), "malai");
        assert_eq!(romanize("धेरै"), "dherai");
        assert_eq!(romanize("राम्रो"), "ramro");
        assert_eq!(romanize("काम"), "kaam");
    }

    // -- tier 2: rules ------------------------------------------------------

    #[test]
    fn rules_handle_words_outside_the_table() {
        for w in ["ग्ल्याक्सी", "क्वान्टम्", "ऍक्स"] {
            assert!(dictionary(w).is_none(), "{w} is in the table, pick another");
            let got = romanize(w);
            assert!(!contains_devanagari(&got), "{w} -> {got}");
            assert!(!got.is_empty(), "{w} produced nothing");
            assert!(got.is_ascii(), "{w} -> {got} is not plain Latin");
        }
    }

    #[test]
    fn schwa_deletion_runs_right_to_left() {
        // Left-to-right deletion yields `akbra`; the alternating right-to-left
        // pass is what produces the correct `akbar`.
        assert_eq!(rule_word("अकबर"), "akbar");
        // Two-syllable words keep both schwas — Nepali does not drop the final.
        assert_eq!(rule_word("तर"), "tara");
    }

    #[test]
    fn devanagari_digits_become_ascii() {
        assert_eq!(romanize("०१२३४५६७८९"), "0123456789");
    }

    #[test]
    fn danda_becomes_a_period() {
        assert_eq!(romanize("हो।"), "ho.");
        assert_eq!(romanize("हो। हुन्छ॥"), "ho. hunxa.");
        // The abbreviation signs have no English equivalent and are dropped
        // rather than passed through as Devanagari.
        assert!(!contains_devanagari(&romanize("डा॰")));
    }

    // -- safety -------------------------------------------------------------

    #[test]
    fn latin_text_is_returned_untouched_without_allocating() {
        let inputs = [
            "just a normal english sentence",
            "cargo build --release --lib",
            "Ship it. Now! 100% — really?",
            "",
            "emoji 🎉 and accents café naïve",
        ];
        for s in inputs {
            let got = romanize(s);
            assert!(matches!(got, Cow::Borrowed(_)), "allocated for {s:?}");
            assert_eq!(got, s);
        }
    }

    #[test]
    fn english_around_devanagari_is_byte_identical() {
        let got = romanize("send the हुन्छ report to Bob at 5pm, ok?");
        assert_eq!(got, "send the hunxa report to Bob at 5pm, ok?");
    }

    #[test]
    fn is_idempotent() {
        for s in [
            "अच्छी बात है, एक स्पीकिंग ना चाह इट गोस",
            "mixed हुन्छ english",
            "pure english only",
        ] {
            let once = romanize(s).into_owned();
            let twice = romanize(&once).into_owned();
            assert_eq!(once, twice, "not idempotent for {s:?}");
        }
    }

    #[test]
    fn whitespace_and_newlines_are_preserved_exactly() {
        assert_eq!(romanize("  हो\n\tहुन्छ  \n"), "  ho\n\thunxa  \n");
    }

    // -- lookup ------------------------------------------------------------

    #[test]
    fn binary_search_finds_first_last_and_missing_keys() {
        let first = DEV_ROMAN.lines().next().unwrap().split_once('\t').unwrap();
        let last = DEV_ROMAN.lines().last().unwrap().split_once('\t').unwrap();
        assert_eq!(dictionary(first.0), Some(first.1));
        assert_eq!(dictionary(last.0), Some(last.1));
        assert_eq!(dictionary("zzz-not-a-key"), None);
        assert_eq!(dictionary(""), None);
    }

    #[test]
    fn binary_search_agrees_with_linear_scan() {
        // Sampled across the whole table so an off-by-one in the snap-back
        // cannot hide in an untested region.
        for (i, line) in DEV_ROMAN.lines().enumerate() {
            if i % 97 != 0 {
                continue;
            }
            let (k, v) = line.split_once('\t').unwrap();
            assert_eq!(dictionary(k), Some(v), "miss at line {i} key {k:?}");
        }
    }
}
