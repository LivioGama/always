//! Deterministic transcript corrector — the inline, always-on correction
//! pass that runs before every paste (target ≤ 1–2 ms, measured p99 in
//! `det_correct::tests::latency_p99_under_budget`).
//!
//! It replaces the blocking LLM grammar pass in the paste path. The LLM
//! post-process stays available per setting, but only as a speculative
//! warm whose result is used when it is ALREADY cached at paste time — the
//! paste never waits for it (see `event_loop::apply_grammar_blocking_request`).
//!
//! Stages, in order:
//! 1. `local_cleanup` — conservative fillers (um/uh/ah/eh) and adjacent
//!    repeated words.
//! 2. Glossary + learned corrections (`text_match::apply_glossary_tiered`
//!    in `NoLlm` mode): exact learned wrong-forms always; phonetic/edit-
//!    distance matches only at high confidence and never for ambiguous
//!    common words. The glossary is mtime-hot-reloaded by `glossary.rs`.
//! 3. Mechanical text normalisation that the measured LLM edits showed are
//!    safe and frequent: leading stray punctuation from the recognizer
//!    ("-.. I want…"), whitespace before punctuation, doubled commas,
//!    standalone `i` / `i'm` → `I` / `I'm`, and a capital letter after an
//!    internal sentence terminator. The FIRST word's case is never
//!    touched: a dictation merged into the previous paste legitimately
//!    starts lowercase.
//!
//! Deliberately NOT done (each was an LLM edit judged wrong on this user's
//! data, see SPEC.md §9): lowercasing `I`/acronyms, expanding
//! contractions, dropping trailing punctuation, deleting content words.

use crate::always::text_match::{DEFAULT_THRESHOLD, FuzzyMode, apply_glossary_tiered};

/// Full deterministic pass.
pub fn correct(text: &str) -> String {
    let cleaned = crate::always::postprocess::local_cleanup(&keep_filler_punctuation(text));
    // Glossary "terms" longer than the matcher's 3-word / 50-char window
    // can never match a candidate n-gram, but they still cost a full
    // Levenshtein per window (the user's glossary holds 7 whole-sentence
    // entries, ~300 chars each). Drop them up front: identical output,
    // ~10x less work.
    let entries: Vec<_> = crate::glossary::glossary_match_entries()
        .into_iter()
        .filter(|e| e.term.chars().count() <= 50 && e.term.split_whitespace().count() <= 3)
        .collect();
    let glossed = if glossary_may_match(&cleaned, &entries) {
        apply_glossary_tiered(&cleaned, &entries, DEFAULT_THRESHOLD, FuzzyMode::NoLlm).0
    } else {
        cleaned
    };
    normalize(&glossed)
}

/// Fingerprint bucket of a char: a-z, 0-9, or 36 for everything else.
fn bucket(ch: char) -> u8 {
    match ch {
        'a'..='z' => ch as u8 - b'a',
        '0'..='9' => 26 + (ch as u8 - b'0'),
        _ => 36,
    }
}

/// `local_cleanup` drops a filler token whole — `so uh. then` loses the
/// sentence boundary. Move a filler's terminal punctuation onto the
/// previous word first (same filler list as `local_cleanup`).
fn keep_filler_punctuation(text: &str) -> String {
    const FILLERS: &[&str] = &["um", "uh", "ah", "eh"];
    let mut out: Vec<String> = Vec::new();
    for w in text.split_whitespace() {
        let core = w.trim_end_matches(['.', '?', '!', ',']);
        let tail = &w[core.len()..];
        let is_filler = FILLERS.contains(&core.to_ascii_lowercase().as_str());
        if is_filler && tail.contains(['.', '?', '!']) {
            if let Some(prev) = out.last_mut() {
                let prev_core_len = prev.trim_end_matches([',', ';', ':']).len();
                prev.truncate(prev_core_len);
                if !prev.ends_with(['.', '?', '!']) {
                    prev.push_str(tail.trim_start_matches(','));
                }
                continue;
            }
        }
        out.push(w.to_string());
    }
    out.join(" ")
}

/// Character-multiset fingerprint: counts per `bucket`.
fn fingerprint(s: &str) -> ([u8; 37], usize) {
    let mut c = [0u8; 37];
    let mut n = 0;
    for ch in s.chars() {
        let b = bucket(ch) as usize;
        c[b] = c[b].saturating_add(1);
        n += 1;
    }
    (c, n)
}

/// SOUND pre-filter for `apply_glossary_tiered` (same window rules: 1–3
/// word n-grams of trimmed, lowercased cores, ≤ 50 chars). Returns false
/// only when NO window can be an exact wrong-form hit and NO window can
/// reach the fuzzy threshold against any term — then the full matcher
/// would return the text unchanged, and skipping it is exact.
///
/// Fuzzy bound: the matcher accepts `lev/max_len * (0.3 if phonetic)` <
/// 0.18, so it needs `lev/max_len < 0.6` at best. Levenshtein distance
/// is at least `max_len - |multiset intersection|`, so a window whose
/// multiset bound is ≥ 0.6 can never match. This replaces ~O(windows ×
/// terms) Levenshtein + Soundex work with a 37-bucket count per window
/// (the matcher costs ~3 ms per utterance in the debug build the daemon
/// ships as; this check ~50 µs).
fn glossary_may_match(text: &str, entries: &[crate::always::text_match::GlossaryMatchEntry]) -> bool {
    if entries.is_empty() {
        return false;
    }
    let exact: std::collections::HashSet<String> = entries
        .iter()
        .flat_map(|e| {
            e.mistranscriptions.iter().map(|m| {
                m.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>()
            })
        })
        .filter(|m| !m.is_empty())
        .collect();
    let (exact_min, exact_max) = exact
        .iter()
        .fold((usize::MAX, 0), |(lo, hi), m| (lo.min(m.len()), hi.max(m.len())));
    struct Term {
        /// Non-zero buckets of the fingerprint: (bucket, count).
        nz: Vec<(usize, u8)>,
        chars: usize,
        bytes: usize,
        nospace: String,
        soundex: String,
    }
    let terms: Vec<Term> = entries
        .iter()
        .map(|e| {
            let nospace = e.term.to_lowercase().replace(' ', "");
            let (fp, chars) = fingerprint(&nospace);
            let nz = (0..37).filter(|&b| fp[b] > 0).map(|b| (b, fp[b])).collect();
            Term { nz, chars, bytes: nospace.len(), soundex: crate::always::text_match::soundex(&nospace), nospace }
        })
        .collect();
    struct Word {
        core: String,
        alnum: String,
        /// Bucket of every char (see `fingerprint`).
        buckets: Vec<u8>,
        chars: usize,
    }
    let words: Vec<Word> = text
        .split_whitespace()
        .map(|w| {
            let core = w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            let buckets: Vec<u8> = core.chars().map(bucket).collect();
            let chars = buckets.len();
            let alnum = if core.chars().all(char::is_alphanumeric) {
                core.clone()
            } else {
                core.chars().filter(|c| c.is_alphanumeric()).collect()
            };
            Word { core, alnum, buckets, chars }
        })
        .collect();
    let mut buf = String::with_capacity(64);
    let mut fp = [0u8; 37];
    for i in 0..words.len() {
        fp.fill(0);
        let (mut chars, mut bytes, mut alnum_len) = (0usize, 0usize, 0usize);
        let _ = &mut alnum_len;
        for n in 0..3.min(words.len() - i) {
            let w = &words[i + n];
            for &b in &w.buckets {
                fp[b as usize] = fp[b as usize].saturating_add(1);
            }
            chars += w.chars;
            bytes += w.core.len();
            alnum_len += w.alnum.len();
            let ngram = || words[i..=i + n].iter().map(|w| w.core.as_str()).collect::<String>();
            if !exact.is_empty() && (exact_min..=exact_max).contains(&alnum_len) {
                buf.clear();
                for w in &words[i..=i + n] {
                    buf.push_str(&w.alnum);
                }
                if exact.contains(buf.as_str()) {
                    return true;
                }
            }
            if bytes == 0 || bytes > 50 {
                continue;
            }
            for t in &terms {
                // The matcher's own byte-length reject.
                let max_bytes = bytes.max(t.bytes) as f64;
                if bytes.abs_diff(t.bytes) as f64 > (max_bytes * 0.25).max(2.0) {
                    continue;
                }
                let max_len = chars.max(t.chars);
                if max_len == 0 {
                    continue;
                }
                let mut inter = 0usize;
                for &(b, c) in &t.nz {
                    inter += fp[b].min(c) as usize;
                }
                let bound = (max_len - inter.min(max_len)) as f64 / max_len as f64;
                // Cheap necessary condition first (multiset bound), then
                // the matcher's exact score on the rare survivors:
                // `lev/max_bytes * (0.3 if Soundex-equal) < threshold`.
                if bound >= DEFAULT_THRESHOLD / 0.3 {
                    continue;
                }
                let gram = ngram();
                let ws = crate::always::text_match::soundex(&gram);
                let phonetic = !ws.is_empty() && ws == t.soundex;
                if !phonetic && bound >= DEFAULT_THRESHOLD {
                    continue;
                }
                let lev = strsim::levenshtein(&gram, &t.nospace) as f64 / max_bytes;
                let combined = if phonetic { lev * 0.3 } else { lev };
                if combined < DEFAULT_THRESHOLD {
                    return true;
                }
            }
        }
    }
    false
}

/// Stage 3 only (pure; no glossary). Public for tests and the harness.
pub fn normalize(text: &str) -> String {
    let artifacts = fix_recognizer_artifacts(text.trim());
    let trimmed = strip_leading_junk(&artifacts);
    let mut out = String::with_capacity(trimmed.len());
    // Pass 1: whitespace before , . ? ! ; : and doubled commas.
    let mut prev: Option<char> = None;
    let chars: Vec<char> = trimmed.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        // Only punctuation that closes a word (followed by space or end)
        // pulls the preceding space in — `./run.sh` must stay intact.
        let closes = chars.get(i + 1).is_none_or(|n| n.is_whitespace());
        if closes && matches!(c, ',' | '.' | '?' | '!' | ';' | ':') {
            while out.ends_with(' ') {
                out.pop();
            }
            if c == ',' && prev == Some(',') {
                continue;
            }
        }
        if c == ' ' && prev == Some(' ') {
            continue;
        }
        out.push(c);
        prev = Some(c);
    }
    // Pass 2: word-level casing.
    let words: Vec<&str> = out.split(' ').collect();
    let mut result: Vec<String> = Vec::with_capacity(words.len());
    let mut sentence_start = false;
    for (idx, w) in words.iter().enumerate() {
        let mut word = (*w).to_string();
        if is_lower_i(&word) {
            word.replace_range(0..1, "I");
        } else if idx > 0 && sentence_start {
            word = capitalize_first(&word);
        }
        sentence_start = w.ends_with(['.', '?', '!']) && !is_abbreviation(w);
        result.push(word);
    }
    result.join(" ")
}

/// Word-level recognizer artifacts measured in the user's transcripts:
/// - a leading `you..` / `you...` (the Apple recognizer's hallucinated
///   "you" on a breath) is dropped entirely;
/// - an ellipsis glued to a word INSIDE the text (`how.. hard`,
///   `and... going`) is a hesitation mark, not punctuation: dropped. A
///   trailing ellipsis at the very end is kept;
/// - `it?'s` → `it's`;
/// - a comma right after a leading/ clause-initial `and`/`but`/`or`
///   (`and, later`) is dropped;
/// - an empty comma slot `, ,` collapses to `,`;
/// - a false-start fragment immediately restarted as a longer word
///   (`your yourself`, `invol involve`) keeps only the completed word.
fn fix_recognizer_artifacts(text: &str) -> String {
    let text = text.replace("?'s", "'s").replace("?’s", "’s");
    let mut words: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    // Leading "you.."
    if words.len() > 1 {
        let first = words[0].to_ascii_lowercase();
        if first.starts_with("you..") && first.trim_end_matches('.') == "you" {
            words.remove(0);
        }
    }
    let n = words.len();
    let next_starts_lowercase: Vec<bool> = (0..n)
        .map(|i| {
            words
                .get(i + 1)
                .and_then(|w| w.chars().next())
                .is_some_and(char::is_lowercase)
        })
        .collect();
    let mut out: Vec<String> = Vec::with_capacity(n);
    for (i, w) in words.into_iter().enumerate() {
        let mut w = w;
        // Interior "word.." / "word..." → "word" — only when the next word
        // continues the sentence (lowercase): `in... Actually` is a real
        // trailing-off, kept as is.
        if i + 1 < n
            && w.ends_with("..")
            && w.trim_end_matches('.').chars().any(char::is_alphanumeric)
            && next_starts_lowercase[i]
        {
            w = w.trim_end_matches('.').to_string();
        }
        // Empty comma slot: a bare "," right after a word that already
        // ends in a comma (`of, , GPU`).
        if w == "," && out.last().is_some_and(|p: &String| p.ends_with(',')) {
            continue;
        }
        // Clause-initial "and," / "but," / "or," (first word, or right
        // after a sentence terminator) followed by more words.
        let clause_start = out.last().is_none_or(|p: &String| p.ends_with(['.', '?', '!']));
        if i + 1 < n && clause_start {
            let lw = w.to_ascii_lowercase();
            if matches!(lw.as_str(), "and," | "but," | "or,") {
                w.pop();
            }
        }
        // False-start fragment: previous word is a ≥4-letter prefix of
        // this one and this one is ≥3 letters longer.
        if let Some(prev) = out.last() {
            let p = prev.to_ascii_lowercase();
            let c = w.to_ascii_lowercase();
            let c_core = c.trim_end_matches(|ch: char| !ch.is_alphanumeric());
            if p.len() >= 4
                && p.chars().all(char::is_alphabetic)
                && c_core.len() >= p.len() + 3
                && c_core.starts_with(&p)
            {
                out.pop();
            }
        }
        out.push(w);
    }
    out.join(" ")
}

/// `i`, `i'm`, `i've`, `i'll`, `i'd` (plus trailing punctuation).
fn is_lower_i(word: &str) -> bool {
    let core: String = word
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .to_string();
    matches!(core.as_str(), "i" | "i'm" | "i've" | "i'll" | "i'd" | "i’m" | "i’ve" | "i’ll" | "i’d")
}

/// Common lowercase abbreviations whose period does not end a sentence.
fn is_abbreviation(word: &str) -> bool {
    let w = word.to_ascii_lowercase();
    matches!(w.as_str(), "e.g." | "i.e." | "etc." | "vs." | "approx." | "dr." | "mr." | "mrs." | "ms.")
        // "3.5." style numbers and single letters ("a.") are not sentence ends.
        || w.trim_end_matches('.').chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn capitalize_first(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        // Only plain lowercase letters; leave URLs/paths/identifiers alone.
        Some(first) if first.is_lowercase() && !word.contains(['/', '_', '@']) => {
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
        _ => word.to_string(),
    }
}

/// Recognizer artifacts like "-.. I want" / ", so": leading characters
/// that are neither alphanumeric nor an opening quote/bracket/sign that a
/// dictation could legitimately start with.
fn strip_leading_junk(text: &str) -> &str {
    let start = text
        .char_indices()
        .find(|&(_, c)| c.is_alphanumeric() || matches!(c, '"' | '\'' | '(' | '[' | '`' | '$' | '#' | '@' | '+' | '~' | '/' | '“' | '‘'))
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text[start..].trim_start()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_recognizer_leading_junk() {
        assert_eq!(normalize("-.. I want to watch it."), "I want to watch it.");
        assert_eq!(normalize(", so then"), "so then");
        assert_eq!(normalize("\"quoted\" start"), "\"quoted\" start");
        assert_eq!(normalize("$HOME is set"), "$HOME is set");
    }

    #[test]
    fn fixes_spacing_and_doubled_commas() {
        assert_eq!(normalize("yes , I think so ."), "Yes, I think so.".replacen('Y', "y", 1));
        assert_eq!(normalize("one,, two"), "one, two");
        assert_eq!(normalize("a  b"), "a b");
    }

    #[test]
    fn capitalises_i_and_sentence_starts_but_never_the_first_word() {
        assert_eq!(normalize("and i think so. then i'm done"), "and I think so. Then I'm done");
        assert_eq!(normalize("ok. is it fine? yes! sure"), "ok. Is it fine? Yes! Sure");
        assert_eq!(normalize("version 3.5. next"), "version 3.5. next");
        assert_eq!(normalize("use e.g. this"), "use e.g. this");
        assert_eq!(normalize("done. ./run.sh now"), "done. ./run.sh now");
    }

    #[test]
    fn removes_measured_recognizer_artifacts() {
        assert_eq!(normalize("you.. Can you turn the lights off, please?"), "Can you turn the lights off, please?");
        assert_eq!(normalize("you... Orion browser is fast"), "Orion browser is fast");
        assert_eq!(normalize("how.. hard would it be"), "how hard would it be");
        assert_eq!(normalize("and... going for dinner now"), "and going for dinner now");
        assert_eq!(normalize("You go to..."), "You go to...");
        assert_eq!(normalize("in... Actually, I'll log in"), "in... Actually, I'll log in");
        assert_eq!(normalize("like that or, orang data"), "like that or, orang data");
        assert_eq!(normalize("what it?'s called"), "what it's called");
        assert_eq!(normalize("and, later, still not"), "and later, still not");
        assert_eq!(normalize("in terms of, , GPU"), "in terms of, GPU");
        assert_eq!(normalize("treat your yourself like"), "treat yourself like");
        // Short prefixes are real words: never dropped.
        assert_eq!(normalize("the then president"), "the then president");
        assert_eq!(normalize("for fortune"), "for fortune");
    }

    #[test]
    fn prefilter_never_skips_a_real_glossary_hit() {
        use crate::always::text_match::GlossaryMatchEntry;
        let entries = vec![
            GlossaryMatchEntry { term: "Claude".into(), mistranscriptions: vec!["cloud".into()] },
            GlossaryMatchEntry { term: "winnow".into(), mistranscriptions: vec!["Vino".into()] },
            GlossaryMatchEntry { term: "Kubernetes".into(), mistranscriptions: vec!["cuber netties".into()] },
        ];
        let samples = [
            "ask cloud to fix it",
            "deploy to the cloud",
            "use Vino for this",
            "spin up cuber netties please",
            "kubernetis cluster is down",
            "claud code is great",
            "nothing related here at all",
            "the quick brown fox jumps over the lazy dog",
        ];
        for t in samples {
            let full = apply_glossary_tiered(t, &entries, DEFAULT_THRESHOLD, FuzzyMode::NoLlm);
            let skipped = !glossary_may_match(t, &entries);
            if skipped {
                assert_eq!(full.0, t, "prefilter skipped a text the matcher changes: {t}");
                assert!(full.1.is_empty(), "prefilter skipped a text with substitutions: {t}");
            }
        }
        assert!(!glossary_may_match("nothing related here at all", &entries));
        assert!(glossary_may_match("ask cloud to fix it", &entries));
        assert!(glossary_may_match("claud code is great", &entries));
    }

    #[test]
    fn leaves_clean_text_unchanged() {
        let t = "Home assistant here, have you installed it? Have you, I mean, yes.";
        assert_eq!(normalize(t), t);
        let t = "I don't really see any changes that I requested, like, bring to foreground.";
        assert_eq!(normalize(t), t);
    }

    /// Evaluation harness: run the corrector over mined (input, LLM)
    /// pairs and print both side by side for manual review.
    /// `ALWAYS_DET_PAIRS=<pairs.jsonl> cargo test --features local-stt --lib det_correct_eval -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn det_correct_eval() {
        let Ok(path) = std::env::var("ALWAYS_DET_PAIRS") else { return };
        let data = std::fs::read_to_string(path).unwrap();
        let (mut same_llm, mut det_changed, mut llm_changed, mut n) = (0, 0, 0, 0);
        let mut times: Vec<u128> = Vec::new();
        for (i, line) in data.lines().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            let input = v["input"].as_str().unwrap();
            let llm = v["llm"].as_str().unwrap();
            let t0 = std::time::Instant::now();
            let det = correct(input);
            times.push(t0.elapsed().as_micros());
            n += 1;
            same_llm += usize::from(det == llm);
            det_changed += usize::from(det != input);
            llm_changed += usize::from(llm != input);
            if det != input || llm != input {
                println!("#{:02}\n  IN : {input}\n  DET: {det}\n  LLM: {llm}", i + 1);
            }
        }
        times.sort_unstable();
        println!(
            "EVAL n={n} det_changed={det_changed} llm_changed={llm_changed} det==llm={same_llm} replay_wall_us p50={} p95={} max={}",
            times[times.len() / 2],
            times[times.len() * 95 / 100],
            times.last().unwrap()
        );
    }

    #[test]
    fn latency_p99_under_budget() {
        let sample = "and also when I like speak, it is pretty fast, but to insert text as well as to press enter It's not fast and natural at all. it takes like 10 5 seconds, uh, uh, 10 seconds or something like that making it not can you look why that is happening? can you look at the locks and everything and can you fix it to make it newer instant as fast as possible, please?";
        let _ = correct(sample); // load glossary once
        // Thread CPU time, not wall time: the dev machine runs at load
        // average 40-180 (other agents' builds), so wall-clock p99 here
        // measures the scheduler, not this code. Wall p99 is printed too.
        fn cpu_us() -> u128 {
            let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
            ts.tv_sec as u128 * 1_000_000 + ts.tv_nsec as u128 / 1_000
        }
        let mut wall: Vec<u128> = Vec::new();
        let mut us: Vec<u128> = (0..300)
            .map(|_| {
                let w = std::time::Instant::now();
                let t = cpu_us();
                std::hint::black_box(correct(std::hint::black_box(sample)));
                let cpu = cpu_us() - t;
                wall.push(w.elapsed().as_micros());
                cpu
            })
            .collect();
        wall.sort_unstable();
        println!("det_correct wall p50={}us p99={}us", wall[wall.len() / 2], wall[wall.len() * 99 / 100]);
        us.sort_unstable();
        // Stage split (diagnostics).
        let stage = |f: &dyn Fn()| {
            let mut v: Vec<u128> = (0..200)
                .map(|_| {
                    let t = std::time::Instant::now();
                    f();
                    t.elapsed().as_micros()
                })
                .collect();
            v.sort_unstable();
            (v[v.len() / 2], v[v.len() * 99 / 100])
        };
        let ents = crate::glossary::glossary_match_entries();
        println!(
            "stages p50/p99 us: local_cleanup={:?} entries={:?} glossary={:?} normalize={:?}",
            stage(&|| { std::hint::black_box(crate::always::postprocess::local_cleanup(sample)); }),
            stage(&|| { std::hint::black_box(crate::glossary::glossary_match_entries()); }),
            stage(&|| { std::hint::black_box(apply_glossary_tiered(sample, &ents, DEFAULT_THRESHOLD, FuzzyMode::NoLlm)); }),
            stage(&|| { std::hint::black_box(normalize(sample)); }),
        );
        let filtered: Vec<_> = ents
            .iter()
            .filter(|e| e.term.chars().count() <= 50 && e.term.split_whitespace().count() <= 3)
            .cloned()
            .collect();
        println!(
            "prefilter={:?} may_match={} terms={:?} min_total={}",
            stage(&|| { std::hint::black_box(glossary_may_match(sample, &filtered)); }),
            glossary_may_match(sample, &filtered),
            filtered.iter().map(|e| e.term.as_str()).collect::<Vec<_>>(),
            us[0]
        );
        let p50 = us[us.len() / 2];
        let p99 = us[us.len() * 99 / 100];
        println!("det_correct cpu p50={p50}us p99={p99}us");
        // Debug build, loaded machine: still far inside the 2 ms budget.
        assert!(p99 < 2_000, "p99 {p99}us");
    }
}
