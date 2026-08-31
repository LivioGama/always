//! Recover English words that Nemotron wrote in Devanagari.
//!
//! # Why this exists
//!
//! Nemotron picks **one** language per utterance. He code-switches constantly,
//! so when an utterance is mostly Nepali the model resolves the whole thing
//! onto Hindi — including the English words in it, which come back spelled
//! phonetically in Devanagari. `translit` then romanises that faithfully, and
//! the English arrives mangled:
//!
//! ```text
//! RAW     हर ओवर … एंड आई एम स्पीकिंग इन इंग्लिस … बिटवीन … अगेन
//! ROMAN   hara owar … end aai em spiking in inglis … bitwin … agena
//! MEANT   …          and  I  am speaking in English … between … again
//! ```
//!
//! The information is not lost: `स्पीकिंग` *is* "speaking", written in a script
//! that cannot spell it. The romaniser transliterates where it should
//! recognise. This module recognises.
//!
//! # How
//!
//! Devanagari destroys English **vowels** — it has no way to write the lax/tense
//! contrast, so `speaking`/`spiking`, `and`/`end`, `between`/`bitwin` all
//! collapse — but it preserves **consonants**, including voicing. So the match
//! key is a consonant skeleton:
//!
//! ```text
//! spiking → SPKNG ← speaking      bitwin → BTVN ← between
//! inglis  → NGLS  ← english       agena  → GN   ← again
//! ```
//!
//! A skeleton alone is far too permissive (`DK` is `dekhi`, `decay`, `duck`,
//! `doc`), so a match must additionally survive four filters:
//!
//! 1. **The Nepali veto.** Anything he has ever typed himself
//!    (`roman_freq.tsv`, 9,730 tokens from 18,452 WhatsApp messages) is left
//!    alone. This is what keeps `dekhi`, `boli`, `nepali`, `hunxa`, `vaneko`,
//!    `maa`, `garna` — and his correctly-transcribed English — untouched. It is
//!    the single highest-value guard here: on his own vocabulary the
//!    false-positive rate it produces is exactly zero, by construction.
//! 2. **Nepali orthographic shape.** `x` (his `छ`), the aspirate digraphs
//!    `dh`/`bh`/`jh`/`chh`, an `h` after `k g l r n m d b j v y`, and a
//!    consonant + `y` cluster are Nepali spellings that a Devanagari rendering
//!    of an English word does not produce.
//! 3. **Vowel correspondence.** Each vowel group must be a *plausible*
//!    Devanagari rendering of the English one — `i` may stand for `ea`
//!    (speaking) or `ee` (between), `e` for `a` (and/again), but `o` may not
//!    stand for `e`. This is what separates `owar → over` from `owar → every`.
//! 4. **Evidence proportional to ambiguity.** A one-consonant skeleton may only
//!    rewrite a two-letter token one edit away; a two-consonant skeleton needs
//!    four letters and two edits; longer skeletons carry themselves.
//!
//! Targets come only from his own vocabulary (`en_recover.tsv`, built by
//! `training/nepali-nemotron/build_en_recover.py`), so a recovery can never
//! invent a word he does not use. Ties break on fewest edits, then on how often
//! he writes the word.
//!
//! # Where it runs
//!
//! Only on tokens that came out of a **Devanagari run** — `translit::romanize`
//! calls it per romanised word. Text he actually dictated in English never
//! enters this module, so English-only dictation keeps its zero-allocation
//! `Cow::Borrowed` path and pays nothing.
//!
//! Escape hatch: `ALWAYS_NO_ENGLISH_RECOVERY=1`.

use crate::always::translit::{lookup_sorted, roman_freq};

/// `SKELETON\tword,freq word,freq …`, byte-sorted by skeleton, candidates in
/// descending order of how often he writes them. See module docs for provenance.
static EN_RECOVER: &str = include_str!("../../resources/translit/en_recover.tsv");

/// Longest token this pass will look at. Everything here works in fixed stack
/// buffers; a word longer than this is not a mis-spelled English word anyway.
const MAX_TOKEN: usize = 24;

// ---------------------------------------------------------------------------
// Phonetic skeletons
// ---------------------------------------------------------------------------

/// A consonant skeleton, held on the stack so the paste path allocates nothing.
struct Skeleton {
    buf: [u8; MAX_TOKEN],
    len: usize,
}

impl Skeleton {
    fn new() -> Self {
        Self {
            buf: [0; MAX_TOKEN],
            len: 0,
        }
    }
    /// Push a phoneme, collapsing a doubled one (`ll`, `tt`, `pp`) into one.
    fn push(&mut self, c: u8) {
        if self.len > 0 && self.buf[self.len - 1] == c {
            return;
        }
        if self.len < MAX_TOKEN {
            self.buf[self.len] = c;
            self.len += 1;
        }
    }
    fn as_str(&self) -> &str {
        // Only ASCII uppercase is ever pushed.
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

#[inline]
fn is_vowel(c: u8) -> bool {
    matches!(c, b'a' | b'e' | b'i' | b'o' | b'u')
}

/// Consonant skeleton of a token **our romaniser produced**.
///
/// The romaniser's conventions are known exactly, which is what makes this
/// tighter than a general soundex: `c` and `x` are both `छ`-family affricates,
/// `w` is `व`, aspiration is a trailing `h` and carries no phonemic weight here.
fn key_roman(w: &str) -> Skeleton {
    let b = w.as_bytes();
    let mut k = Skeleton::new();
    let mut i = 0;
    while i < b.len() {
        let two = &b[i..(i + 2).min(b.len())];
        match two {
            b"ch" => {
                k.push(b'C');
                i += 2;
                continue;
            }
            b"sh" => {
                k.push(b'S');
                i += 2;
                continue;
            }
            b"ph" => {
                k.push(b'F');
                i += 2;
                continue;
            }
            b"th" => {
                k.push(b'T');
                i += 2;
                continue;
            }
            b"kh" => {
                k.push(b'K');
                i += 2;
                continue;
            }
            b"gh" => {
                k.push(b'G');
                i += 2;
                continue;
            }
            b"bh" => {
                k.push(b'B');
                i += 2;
                continue;
            }
            b"dh" => {
                k.push(b'D');
                i += 2;
                continue;
            }
            b"jh" | b"zh" => {
                k.push(b'J');
                i += 2;
                continue;
            }
            _ => {}
        }
        let c = b[i];
        i += 1;
        if is_vowel(c) {
            continue;
        }
        match c {
            // A word-initial `y`/`h` is a consonant; anywhere else it is a
            // glide or an aspiration mark the skeleton does not carry.
            b'y' | b'h' => {
                if i == 1 {
                    k.push(c.to_ascii_uppercase());
                }
            }
            b'x' | b'c' => k.push(b'C'),
            b'w' | b'v' => k.push(b'V'),
            b'q' => k.push(b'K'),
            b'z' => k.push(b'J'),
            _ if c.is_ascii_alphabetic() => k.push(c.to_ascii_uppercase()),
            _ => {}
        }
    }
    k
}

/// Consonant skeleton of an **English** word.
///
/// Deliberately not the same function as `key_roman`: English orthography maps
/// letters to sounds differently (`c` is /k/ or /s/, `gh` is usually silent,
/// `x` is two phonemes). Both sides have to land in the same phonetic space for
/// a comparison to mean anything.
///
/// Must agree with `key_english` in
/// `training/nepali-nemotron/build_en_recover.py`, which builds the table this
/// is searched against — see `key_english_matches_the_generated_table`.
///
/// Test-only: at runtime the English side is already keyed, in the table. This
/// exists so the two implementations can be held to each other, because if they
/// drift the table silently stops being reachable for the words that moved.
#[cfg(test)]
fn key_english(w: &str) -> Skeleton {
    let b = w.as_bytes();
    let mut k = Skeleton::new();
    let mut i = 0;
    while i < b.len() {
        let two = &b[i..(i + 2).min(b.len())];
        match two {
            b"ch" => {
                k.push(b'C');
                i += 2;
                continue;
            }
            b"sh" => {
                k.push(b'S');
                i += 2;
                continue;
            }
            b"ph" => {
                k.push(b'F');
                i += 2;
                continue;
            }
            b"th" => {
                k.push(b'T');
                i += 2;
                continue;
            }
            // night, though, weight — silent.
            b"gh" => {
                i += 2;
                continue;
            }
            b"wh" => {
                k.push(b'V');
                i += 2;
                continue;
            }
            b"ck" => {
                k.push(b'K');
                i += 2;
                continue;
            }
            b"qu" => {
                k.push(b'K');
                k.push(b'V');
                i += 2;
                continue;
            }
            b"kn" if i == 0 => {
                k.push(b'N');
                i += 2;
                continue;
            }
            b"wr" if i == 0 => {
                k.push(b'R');
                i += 2;
                continue;
            }
            _ => {}
        }
        let c = b[i];
        i += 1;
        if is_vowel(c) {
            continue;
        }
        match c {
            b'y' | b'h' => {
                if i == 1 {
                    k.push(c.to_ascii_uppercase());
                }
            }
            b'c' => {
                // Soft before e/i/y, hard otherwise.
                let soft = matches!(b.get(i), Some(b'e') | Some(b'i') | Some(b'y'));
                k.push(if soft { b'S' } else { b'K' });
            }
            b'x' => {
                k.push(b'K');
                k.push(b'S');
            }
            b'w' | b'v' => k.push(b'V'),
            b'q' => k.push(b'K'),
            b'z' => k.push(b'J'),
            _ if c.is_ascii_alphabetic() => k.push(c.to_ascii_uppercase()),
            _ => {}
        }
    }
    k
}

// ---------------------------------------------------------------------------
// Vowel correspondence
// ---------------------------------------------------------------------------

// Coarse vowel classes, as bit flags so an English spelling can name the whole
// set of Devanagari renderings it is allowed to have collapsed into.
const V_A: u16 = 1 << 0;
const V_E: u16 = 1 << 1;
const V_I: u16 = 1 << 2;
const V_O: u16 = 1 << 3;
const V_U: u16 = 1 << 4;
const V_AI: u16 = 1 << 5;
const V_AU: u16 = 1 << 6;
const V_OI: u16 = 1 << 7;
/// Long `आ`. Kept apart from `V_A` on purpose: the romaniser writes `aa` only
/// for a genuinely long vowel, and English /ʌ/ (*but*, *up*) is never written
/// that way. Collapsing the two turns `baata` — his "बात" — into `but`.
const V_AA: u16 = 1 << 8;

/// The class our romaniser's output stands for. Repeated letters are the
/// romaniser writing vowel length (`aa`, `ee`), which carries no class of its
/// own.
fn roman_vowel_class(group: &str) -> u16 {
    if group == "aa" {
        return V_AA;
    }
    let mut squeezed = [0u8; 8];
    let mut n = 0;
    for &c in group.as_bytes() {
        if n > 0 && squeezed[n - 1] == c {
            continue;
        }
        if n == squeezed.len() {
            return 0;
        }
        squeezed[n] = c;
        n += 1;
    }
    match &squeezed[..n] {
        b"a" => V_A,
        b"e" | b"ea" => V_E,
        b"i" | b"ie" => V_I,
        b"o" => V_O,
        b"u" | b"eu" => V_U,
        b"ai" => V_AI,
        b"au" | b"ou" | b"ao" => V_AU,
        b"oi" => V_OI,
        b"ia" | b"ua" => V_A | V_I | V_U,
        _ => 0,
    }
}

/// Which Devanagari renderings this English vowel spelling may have collapsed
/// into. `0` means "no idea" — and no idea means no match.
///
/// This is the filter that does the real work. It permits the collapses
/// Devanagari genuinely forces (`a`→`e` in *and*, `ea`→`i` in *speaking*,
/// `ee`→`i` in *between*, `ai`→`e` in *again*) and refuses the ones it does not
/// (`e`→`o`, which is the difference between `owar → over` and `owar → every`).
fn english_vowel_classes(group: &str) -> u16 {
    match group.as_bytes() {
        b"a" => V_A | V_E | V_AA,
        b"e" => V_E | V_I | V_A,
        b"i" => V_I | V_AI,
        b"o" => V_O | V_A | V_U,
        b"u" => V_U | V_A,
        b"ee" | b"ea" | b"eo" => V_I | V_E,
        b"ie" | b"ei" => V_I | V_E | V_AI,
        b"oo" => V_U | V_O,
        b"ou" => V_AU | V_U | V_O | V_A,
        b"ai" | b"ay" => V_E | V_AI | V_A,
        b"au" | b"aw" => V_O | V_AU | V_AA,
        b"oa" => V_O,
        b"oi" | b"oy" => V_OI,
        b"y" => V_I | V_E | V_AI,
        b"ue" | b"eu" => V_U,
        b"ui" => V_U | V_I,
        b"eau" => V_O,
        b"io" => V_I | V_A,
        b"ia" => V_A | V_I | V_AA,
        b"ua" => V_A | V_U | V_AA,
        b"ye" => V_AI | V_I,
        b"oe" => V_O | V_U,
        b"aa" => V_A | V_AA,
        b"iou" | b"ao" => V_A | V_AA,
        _ => 0,
    }
}

/// Iterate maximal vowel runs. `y` counts as a vowel for English (but never
/// word-initially, where it is a consonant), and never for our romaniser's
/// output, which has no vocalic `y`.
fn vowel_groups(w: &str, english: bool) -> impl Iterator<Item = &str> {
    let b = w.as_bytes();
    let vowelish = move |i: usize, c: u8| is_vowel(c) || (english && c == b'y' && i > 0);
    let mut i = 0;
    // A silent final `e` (`state`, `mate`, `above`) spells no vowel of its own,
    // so it must not count as a group — otherwise every English word ending in
    // one looks a syllable longer than the Devanagari it is being matched to.
    let end = if english
        && b.len() >= 4
        && b[b.len() - 1] == b'e'
        && !vowelish(b.len() - 2, b[b.len() - 2])
    {
        b.len() - 1
    } else {
        b.len()
    };
    std::iter::from_fn(move || {
        while i < end && !vowelish(i, b[i]) {
            i += 1;
        }
        if i >= end {
            return None;
        }
        let start = i;
        while i < end && vowelish(i, b[i]) {
            i += 1;
        }
        Some(&w[start..i])
    })
}

/// True when every vowel of `roman` is a rendering `english` could have
/// collapsed into — same count, each one compatible.
fn vowels_compatible(roman: &str, english: &str) -> bool {
    let mut r = vowel_groups(roman, false);
    let mut e = vowel_groups(english, true);
    let mut any = false;
    loop {
        match (r.next(), e.next()) {
            (None, None) => return any,
            (Some(rg), Some(eg)) => {
                any = true;
                if roman_vowel_class(rg) & english_vowel_classes(eg) == 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

// ---------------------------------------------------------------------------
// Distance
// ---------------------------------------------------------------------------

/// Levenshtein distance, capped at `max`. Two stack rows, no allocation.
fn edit_distance(a: &str, b: &str, max: usize) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut prev = [0usize; MAX_TOKEN + 2];
    let mut cur = [0usize; MAX_TOKEN + 2];
    if b.len() + 1 > prev.len() {
        return max + 1;
    }
    for (j, slot) in prev.iter_mut().enumerate().take(b.len() + 1) {
        *slot = j;
    }
    for i in 1..=a.len() {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
            row_min = row_min.min(cur[j]);
        }
        if row_min > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// True when the token is spelled the way his Nepali is spelled, and therefore
/// is not a Devanagari rendering of an English word.
///
/// `x` is his `छ`. The aspirate digraphs and a post-consonantal `h` come from
/// `ख घ ठ ढ थ ध भ झ` — English borrowed into Devanagari uses the unaspirated
/// letters, so `dakha`, `alha`, `sakha` are Nepali no matter what they rhyme
/// with. A consonant followed by `y` is the Sanskritic `kriya`/`sandhya`
/// cluster, which English spellings do not produce here.
fn nepali_shaped(w: &str) -> bool {
    let b = w.as_bytes();
    if w.contains('x') || w.contains("chh") {
        return true;
    }
    for i in 1..b.len() {
        let aspirated = matches!(
            b[i - 1],
            b'k' | b'g' | b'l' | b'r' | b'n' | b'm' | b'd' | b'b' | b'j' | b'v' | b'y'
        );
        if b[i] == b'h' && aspirated {
            return true;
        }
        if b[i] == b'y' && i + 1 < b.len() && !is_vowel(b[i - 1]) {
            return true;
        }
    }
    false
}

/// How much evidence a match needs, given how much the skeleton constrains it.
///
/// A one-consonant skeleton (`M`) is nearly no evidence, so it may only rewrite
/// a two-letter token one edit away — `em → am`, and essentially nothing else.
/// Longer skeletons identify a word largely on their own.
fn passes_evidence_gate(skeleton_len: usize, token_len: usize, distance: usize) -> bool {
    match skeleton_len {
        0 => false,
        1 => token_len == 2 && distance <= 1,
        2 => token_len >= 4 && distance <= 2,
        _ => token_len >= 4 && distance <= 3,
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// True when the pass is switched off by the environment.
pub fn disabled() -> bool {
    matches!(
        std::env::var("ALWAYS_NO_ENGLISH_RECOVERY").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// The English word `roman` is a Devanagari-mediated spelling of, if that can be
/// established with confidence. `None` — leave the token alone — is the default
/// and by far the commonest answer.
///
/// `roman` must be a single token of our romaniser's output. A wrong rewrite is
/// worse than none: he can read `spiking` and understand it, whereas `spike`
/// would mislead. Every filter here exists to make `None` the answer whenever
/// there is any doubt.
pub fn recover_word(roman: &str) -> Option<&'static str> {
    if roman.len() < 2 || roman.len() > MAX_TOKEN {
        return None;
    }
    if !roman.bytes().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    // Anything he writes himself is right by definition — his Nepali, and his
    // English that transcribed correctly.
    if roman_freq(roman) > 0 {
        return None;
    }
    if nepali_shaped(roman) {
        return None;
    }

    // The romaniser writes an unwritten final schwa (`अगेन` → `agena`,
    // `इट` → `ita`). Try the token as it stands, then without it.
    let trimmed = roman
        .strip_suffix('a')
        .filter(|t| t.len() >= 3)
        .unwrap_or(roman);
    let variants: [&str; 2] = [roman, trimmed];
    let variant_count = if trimmed.len() == roman.len() { 1 } else { 2 };

    for variant in &variants[..variant_count] {
        let skeleton = key_roman(variant);
        let key = skeleton.as_str();
        if key.is_empty() {
            continue;
        }
        let Some(row) = lookup_sorted(EN_RECOVER, key) else {
            continue;
        };
        let variant_starts_with_vowel = variant.as_bytes().first().is_some_and(|&c| is_vowel(c));

        let mut best: Option<(usize, u32, &'static str)> = None;
        for entry in row.split(' ') {
            let Some((word, freq)) = entry.split_once(',') else {
                continue;
            };
            // The token already *is* one of the targets: it is correct English
            // that merely happens to be absent from his vocabulary. Do nothing.
            if word == roman {
                return None;
            }
            if word.as_bytes().first().is_some_and(|&c| is_vowel(c)) != variant_starts_with_vowel {
                continue;
            }
            if !vowels_compatible(variant, word) {
                continue;
            }
            let distance = edit_distance(variant, word, 3);
            if !passes_evidence_gate(key.len(), variant.len(), distance) {
                continue;
            }
            let freq: u32 = freq.parse().unwrap_or(0);
            // Fewest edits wins; his own usage breaks the tie.
            let better = match best {
                None => true,
                Some((d, f, _)) => distance < d || (distance == d && freq > f),
            };
            if better {
                best = Some((distance, freq, word));
            }
        }
        if let Some((_, _, word)) = best {
            return Some(word);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole transcript from the report, romanised. This is the real thing:
    /// he said "…and I am speaking in English in between … dekhi nepali boli …
    /// and I am speaking English English again" and got the left column.
    const REPORTED: &str = "hara owar weski ksetron khabar kastoc end aai em spiking in \
inglis in bitwin end mopheri dekhi nepali boli rat buc ki puja insera mahta tu end ap tu end \
ita aai em spiking inglis inglis agena";

    fn recover_all(text: &str) -> String {
        text.split(' ')
            .map(|t| recover_word(t).unwrap_or(t))
            .collect::<Vec<_>>()
            .join(" ")
    }

    // -- the reported utterance ---------------------------------------------

    #[test]
    fn recovers_the_english_in_the_reported_utterance() {
        for (mangled, english) in [
            ("spiking", "speaking"),
            ("inglis", "english"),
            ("bitwin", "between"),
            ("agena", "again"),
            ("em", "am"),
        ] {
            assert_eq!(
                recover_word(mangled),
                Some(english),
                "{mangled} should recover to {english}"
            );
        }
    }

    #[test]
    fn leaves_the_nepali_in_the_reported_utterance_alone() {
        // These are correct Roman Nepali. Rewriting any of them is the failure
        // mode that would make this pass a net loss.
        for nepali in [
            "dekhi", "nepali", "boli", "ki", "khabar", "rat", "ita", "aai",
        ] {
            assert_eq!(recover_word(nepali), None, "{nepali} is Nepali, leave it");
        }
    }

    #[test]
    fn the_reported_utterance_comes_out_readable() {
        assert_eq!(
            recover_all(REPORTED),
            "hara over weski ksetron khabar kastoc end aai am speaking in english in between \
end mopheri dekhi nepali boli rat buc ki puja insera mahta tu end up tu end ita aai am \
speaking english english again"
        );
    }

    #[test]
    fn recovery_is_idempotent() {
        let once = recover_all(REPORTED);
        assert_eq!(recover_all(&once), once);
    }

    // -- guards --------------------------------------------------------------

    #[test]
    fn his_own_vocabulary_is_never_rewritten() {
        // The primary guard, stated as a property: every token he has ever
        // typed survives this pass byte-identical. That covers all of his
        // Nepali and all of his correctly-transcribed English at once.
        let mut checked = 0;
        for line in include_str!("../../resources/translit/roman_freq.tsv").lines() {
            let (word, _) = line.split_once('\t').expect("well-formed record");
            assert_eq!(recover_word(word), None, "rewrote his own token {word:?}");
            checked += 1;
        }
        assert!(
            checked > 9_000,
            "expected his full vocabulary, saw {checked}"
        );
    }

    #[test]
    fn nepali_spellings_are_vetoed_by_shape() {
        for nepali in [
            "hunxa", "garnexa", "dakha", "sandhya", "byana", "sakha", "bhitra",
        ] {
            assert!(nepali_shaped(nepali), "{nepali} should be shape-vetoed");
        }
        // Shape is the second guard, not the only one: `vaneko` and `maa` are
        // plainly spelled and are held by the vocabulary veto instead.
        for nepali in ["vaneko", "maa", "garna", "dekhi", "boli"] {
            assert_eq!(recover_word(nepali), None, "{nepali} must survive");
        }
        for english in [
            "spiking",
            "inglis",
            "bitwin",
            "agena",
            "em",
            "owar",
            "saphtwera",
        ] {
            assert!(!nepali_shaped(english), "{english} should not be vetoed");
        }
    }

    #[test]
    fn a_word_that_is_already_english_is_left_alone() {
        // `speaking` is in the target table. Handed back to the pass it must
        // not hop to another word with the same skeleton.
        assert_eq!(recover_word("speaking"), None);
        assert_eq!(recover_word("between"), None);
    }

    #[test]
    fn vowel_correspondence_rejects_implausible_collapses() {
        // Both share the skeleton VR and both are in his vocabulary; only one
        // is a vowel-plausible reading of `owar`.
        assert_eq!(recover_word("owar"), Some("over"));
        assert!(vowels_compatible("spiking", "speaking"));
        assert!(vowels_compatible("bitwin", "between"));
        assert!(vowels_compatible("agen", "again"));
        // `every` shares the VR skeleton with `over` and he writes it more
        // often, so only the vowels can tell them apart: `o` is not a
        // Devanagari rendering of the `e` in *every*.
        assert!(!vowels_compatible("owar", "every"));
        // A silent final `e` spells no vowel, so `state` counts one group, not
        // two — otherwise every such word looks a syllable too long.
        assert_eq!(vowel_groups("state", true).count(), 1);
        assert_eq!(vowel_groups("between", true).count(), 2);
        // `ste` does share `state`'s skeleton and vowels; it is the evidence
        // gate that refuses it — two consonants may not rewrite a 3-letter token.
        assert_eq!(recover_word("ste"), None);
        // His "बात" romanises to `baata`. The romaniser writes `aa` only for a
        // long vowel and English /ʌ/ is never long, so `but` is not a reading
        // of it — this was a live false positive before `V_AA` existed.
        assert!(!vowels_compatible("baat", "but"));
        assert_eq!(recover_word("baata"), None);
        // But a genuinely long English `a` still matches.
        assert!(vowels_compatible("kaar", "car"));
    }

    #[test]
    fn short_skeletons_need_short_tokens() {
        // `M` is one consonant of evidence. It may rewrite `em`, and nothing
        // longer — `imam`, `ama` must not become `am`.
        assert!(passes_evidence_gate(1, 2, 1));
        assert!(!passes_evidence_gate(1, 3, 1));
        assert!(!passes_evidence_gate(1, 2, 2));
        assert!(!passes_evidence_gate(2, 3, 1));
        assert!(passes_evidence_gate(4, 6, 3));
        assert!(!passes_evidence_gate(4, 6, 4));
    }

    #[test]
    fn nothing_outside_plain_lowercase_ascii_is_touched() {
        for odd in ["", "a", "Speaking", "spiking!", "12", "ma'am", "स्पीकिंग"] {
            assert_eq!(recover_word(odd), None, "{odd:?} should be left alone");
        }
    }

    // -- the table -----------------------------------------------------------

    #[test]
    fn table_is_byte_sorted_and_well_formed() {
        let mut previous = "";
        for line in EN_RECOVER.lines() {
            let (key, row) = line.split_once('\t').expect("well-formed record");
            assert!(previous < key, "not byte-sorted at {key:?}");
            previous = key;
            assert!(
                key.bytes().all(|c| c.is_ascii_uppercase()),
                "skeleton {key:?} is not a skeleton"
            );
            for entry in row.split(' ') {
                let (word, freq) = entry.split_once(',').expect("word,freq");
                assert!(!word.is_empty() && word.bytes().all(|c| c.is_ascii_lowercase()));
                freq.parse::<u32>().expect("numeric frequency");
            }
        }
    }

    #[test]
    fn key_english_matches_the_generated_table() {
        // The generator computes the skeleton in Python. If the two drift, the
        // table is silently unreachable for the words that moved — so every row
        // is checked against the Rust implementation.
        for line in EN_RECOVER.lines() {
            let (key, row) = line.split_once('\t').expect("well-formed record");
            for entry in row.split(' ') {
                let (word, _) = entry.split_once(',').expect("word,freq");
                assert_eq!(
                    key_english(word).as_str(),
                    key,
                    "{word} keys differently in Rust than in the generator"
                );
            }
        }
    }

    #[test]
    fn skeletons_collapse_the_distortions_devanagari_makes() {
        assert_eq!(
            key_roman("spiking").as_str(),
            key_english("speaking").as_str()
        );
        assert_eq!(
            key_roman("bitwin").as_str(),
            key_english("between").as_str()
        );
        assert_eq!(
            key_roman("inglis").as_str(),
            key_english("english").as_str()
        );
        assert_eq!(key_roman("agen").as_str(), key_english("again").as_str());
        assert_eq!(key_roman("owar").as_str(), key_english("over").as_str());
    }

    #[test]
    fn edit_distance_is_bounded_and_correct() {
        assert_eq!(edit_distance("spiking", "speaking", 3), 2);
        assert_eq!(edit_distance("bitwin", "between", 3), 3);
        assert_eq!(edit_distance("em", "am", 3), 1);
        assert_eq!(
            edit_distance("abc", "xyz", 2),
            3,
            "capped result exceeds max"
        );
        assert_eq!(edit_distance("same", "same", 3), 0);
    }

    #[test]
    fn the_escape_hatch_is_readable() {
        // Not toggled here — the env is process-wide and other tests run in
        // parallel. Just prove the accessor works.
        let _ = disabled();
    }
}
