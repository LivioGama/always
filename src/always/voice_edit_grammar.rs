//! Voice-edit grammar: decides whether a WHOLE utterance is an edit command
//! ("scratch that", "change cat to dog", "new line", …).
//!
//! Pure and allocation-light — no I/O, no clocks — so the safety replay over
//! the user's real dictation logs (`voice_edit_replay` tests) exercises the
//! exact function the daemon runs.
//!
//! Exact-grammar style: the normalised utterance must equal one of the
//! phrasings below (optionally wrapped in "please"/"okay"). Anything longer,
//! any extra word, any sentence around it → `None` → the utterance is pasted
//! as dictation. The paste-window gate in `voice_edit.rs` is a second,
//! independent bar on top of this one.

/// Longest utterance (in words, after normalisation) that can be an edit
/// command. "replace the quick brown fox with the lazy old dog" = 10.
pub const MAX_COMMAND_WORDS: usize = 12;
/// Each side of "change X to Y" is at most this many words.
pub const MAX_REPLACE_SIDE_WORDS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseKind {
    /// Title-case every word ("capitalise that").
    Capitalize,
    Upper,
    Lower,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// The last paste ("select that").
    That,
    LastWord,
    LastSentence,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    End,
    Start,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditCommand {
    /// Remove the last paste ("scratch that", "delete that").
    ScratchThat,
    /// Replace `from` with `to` in the last paste (or, failing that, the
    /// text before the caret in the focused field).
    Replace {
        from: String,
        to: String,
    },
    Case(CaseKind),
    MakeList,
    NewLine,
    NewParagraph,
    Undo,
    Redo,
    Select(Unit),
    Delete(Unit),
    GoTo(Place),
    /// Rewrite the selection / last paste in place (Iris, on-device model):
    /// `style` is "shorter", "formal", "casual", "grammar" or
    /// "translate:<language>". Always only hands this off — see
    /// `voice_edit.rs` (`rewrite_request`).
    Rewrite {
        style: String,
    },
    /// Press Return — submits in chat apps and agent prompts. Opt-in
    /// (`spoken_enter`); the phrases are the user's (`spoken_enter_phrases`,
    /// default [`DEFAULT_ENTER_PHRASES`]); off, the words are dictated.
    PressEnter,
}

impl EditCommand {
    /// Stable short name for logs / the shadow dataset / UDS results.
    pub fn name(&self) -> &'static str {
        match self {
            EditCommand::ScratchThat => "scratch_that",
            EditCommand::Replace { .. } => "replace",
            EditCommand::Case(CaseKind::Capitalize) => "capitalize",
            EditCommand::Case(CaseKind::Upper) => "uppercase",
            EditCommand::Case(CaseKind::Lower) => "lowercase",
            EditCommand::MakeList => "make_list",
            EditCommand::NewLine => "new_line",
            EditCommand::NewParagraph => "new_paragraph",
            EditCommand::Undo => "undo",
            EditCommand::Redo => "redo",
            EditCommand::Select(Unit::That) => "select_that",
            EditCommand::Select(Unit::LastWord) => "select_last_word",
            EditCommand::Select(Unit::LastSentence) => "select_last_sentence",
            EditCommand::Select(Unit::All) => "select_all",
            EditCommand::Delete(Unit::That) => "scratch_that",
            EditCommand::Delete(Unit::LastWord) => "delete_last_word",
            EditCommand::Delete(Unit::LastSentence) => "delete_last_sentence",
            EditCommand::Delete(Unit::All) => "delete_all",
            EditCommand::GoTo(Place::End) => "go_to_end",
            EditCommand::GoTo(Place::Start) => "go_to_start",
            EditCommand::Rewrite { .. } => "rewrite",
            EditCommand::PressEnter => "press_enter",
        }
    }
}

/// Lowercase, strip punctuation that STT adds around a short command
/// ("Scratch that." / "New line!" / "Change cat, to dog."), collapse
/// whitespace. Apostrophes and inner hyphens survive ("don't", "e-mail").
pub fn normalize(text: &str) -> String {
    normalize_with(text, true)
}

/// Same tokenisation as [`normalize`] but keeps the speaker's casing —
/// the replacement text of "change cloud code to Claude Code" must keep
/// the capitals the recogniser gave it.
pub fn normalize_cased(text: &str) -> String {
    normalize_with(text, false)
}

fn normalize_with(text: &str, lower: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let c = match ch {
            '\u{2019}' | '\u{2018}' => '\'',
            c => c,
        };
        if c.is_alphanumeric() || c == '\'' || c == '-' {
            if lower {
                out.extend(c.to_lowercase());
            } else {
                out.push(c);
            }
        } else {
            out.push(' ');
        }
    }
    let joined = out.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.trim_matches(|c| c == '-' || c == '\'').to_string()
}

/// Phrases that press Return when `spoken_enter` is on and the user has not
/// set `spoken_enter_phrases`. No bare "enter": a lone short word is the
/// commonest mis-hearing, and Return sends.
pub const DEFAULT_ENTER_PHRASES: &[&str] = &[
    "press enter",
    "hit enter",
    "press return",
    "hit return",
    "send it",
    "send that",
    "submit it",
    "submit that",
];

/// The user's `spoken_enter_phrases` value (comma-separated) as normalized
/// phrases; unset, blank or only invalid entries → the defaults. A phrase
/// longer than a command can be ([`MAX_COMMAND_WORDS`]) is dropped.
pub fn enter_phrases(setting: Option<&str>) -> Vec<String> {
    let custom: Vec<String> = setting
        .unwrap_or("")
        .split(',')
        .map(normalize)
        .filter(|p| !p.is_empty() && p.split(' ').count() <= MAX_COMMAND_WORDS)
        .collect();
    if custom.is_empty() {
        DEFAULT_ENTER_PHRASES
            .iter()
            .map(|p| p.to_string())
            .collect()
    } else {
        custom
    }
}

/// The whole utterance is one of `phrases` (as returned by
/// [`enter_phrases`]), after the same normalisation and polite affixes as
/// every command ("Okay, send it." matches "send it").
pub fn is_enter_phrase(utterance: &str, phrases: &[String]) -> bool {
    if utterance.len() > 120 || utterance.split_whitespace().count() > MAX_COMMAND_WORDS {
        return false;
    }
    let norm = normalize(utterance);
    let core = strip_polite(&norm);
    !core.is_empty() && phrases.iter().any(|p| p == core)
}

/// `text` ends with one of `phrases` after some dictation ("…and run it, go
/// go."): the dictation without the phrase and the punctuation joining them,
/// in the speaker's own casing. `None` when no phrase ends the text or
/// nothing precedes it (a phrase alone is the voice-edit command).
pub fn split_trailing_enter(text: &str, phrases: &[String]) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    // (index into `words`, normalized word) for every word with letters.
    let norm: Vec<(usize, String)> = words
        .iter()
        .enumerate()
        .map(|(i, w)| (i, normalize(w)))
        .filter(|(_, n)| !n.is_empty())
        .collect();
    let cut = phrases
        .iter()
        .filter_map(|p| {
            let pw: Vec<&str> = p.split(' ').collect();
            let k = pw.len();
            (norm.len() > k
                && norm[norm.len() - k..]
                    .iter()
                    .map(|(_, n)| n.as_str())
                    .eq(pw.iter().copied()))
            .then(|| norm[norm.len() - k].0)
        })
        .min()?;
    let rest = words[..cut].join(" ");
    let rest = rest.trim_end_matches(|c: char| {
        matches!(c, ',' | ';' | ':' | '-' | '—' | '–') || c.is_whitespace()
    });
    (!rest.is_empty()).then(|| rest.to_string())
}

const POLITE_PREFIX: &[&str] = &["please ", "okay ", "ok "];
const POLITE_SUFFIX: &[&str] = &[" please"];

fn strip_polite(mut s: &str) -> &str {
    for p in POLITE_PREFIX {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest;
            break;
        }
    }
    for p in POLITE_SUFFIX {
        if let Some(rest) = s.strip_suffix(p) {
            s = rest;
            break;
        }
    }
    s
}

fn exact(s: &str) -> Option<EditCommand> {
    use EditCommand as E;
    Some(match s {
        "scratch that"
        | "delete that"
        | "remove that"
        | "erase that"
        | "strike that"
        | "scratch that last bit"
        | "scratch the last bit" => E::ScratchThat,
        "capitalise that" | "capitalize that" | "capitalise it" | "capitalize it"
        | "title case that" => E::Case(CaseKind::Capitalize),
        "uppercase that"
        | "upper case that"
        | "all caps that"
        | "make that uppercase"
        | "make that upper case"
        | "make that all caps"
        | "caps that" => E::Case(CaseKind::Upper),
        "lowercase that" | "lower case that" | "make that lowercase" | "make that lower case" => {
            E::Case(CaseKind::Lower)
        }
        "make that a list"
        | "make it a list"
        | "make that a bulleted list"
        | "make that a bullet list"
        | "make that a bullet point list"
        | "turn that into a list"
        | "make that into a list"
        | "make this a list" => E::MakeList,
        "new line" | "newline" | "next line" => E::NewLine,
        "new paragraph" | "next paragraph" => E::NewParagraph,
        "undo" | "undo that" => E::Undo,
        "redo" | "redo that" => E::Redo,
        "select that" => E::Select(Unit::That),
        "select the last word" | "select last word" => E::Select(Unit::LastWord),
        "select the last sentence" | "select last sentence" => E::Select(Unit::LastSentence),
        "select all" | "select everything" => E::Select(Unit::All),
        "delete the last word" | "delete last word" | "remove the last word" => {
            E::Delete(Unit::LastWord)
        }
        "delete the last sentence" | "delete last sentence" | "remove the last sentence" => {
            E::Delete(Unit::LastSentence)
        }
        "go to the end"
        | "go to end"
        | "jump to the end"
        | "move to the end"
        | "go to the end of the text" => E::GoTo(Place::End),
        "go to the start"
        | "go to the beginning"
        | "go to start"
        | "jump to the start"
        | "jump to the beginning"
        | "move to the start" => E::GoTo(Place::Start),
        _ => return None,
    })
}

/// Every way "change X to Y" can split. The executor picks the split whose
/// `from` actually occurs in the target text — "change to to two" has two
/// candidate splits and only the text decides which one the user meant.
pub fn replace_candidates(norm: &str) -> Vec<(String, String)> {
    replace_candidates_cased(norm, norm)
}

/// [`replace_candidates`] where `to` keeps the casing from `cased` (same
/// tokenisation, see [`normalize_cased`]). `from` stays lowercase — it is
/// matched case-insensitively.
pub fn replace_candidates_cased(norm: &str, cased: &str) -> Vec<(String, String)> {
    let words: Vec<&str> = norm.split(' ').collect();
    let cased_words: Vec<&str> = cased.split(' ').collect();
    let cased_words = if cased_words.len() == words.len() {
        cased_words
    } else {
        words.clone()
    };
    if words.len() < 4 {
        return Vec::new();
    }
    let verb = words[0];
    let seps: &[&str] = match verb {
        "change" | "correct" | "fix" => &["to", "into"],
        "replace" | "swap" => &["with", "by"],
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    // words[1..i] = from, words[i] = separator, words[i+1..] = to
    for i in 2..words.len() - 1 {
        if !seps.contains(&words[i]) {
            continue;
        }
        let from = &words[1..i];
        let to = &cased_words[i + 1..];
        if from.len() > MAX_REPLACE_SIDE_WORDS || to.len() > MAX_REPLACE_SIDE_WORDS {
            continue;
        }
        // "change that to …" / "replace it with …" is a rewrite of the
        // whole paste, not a find-replace — not part of this grammar.
        if from.len() == 1 && matches!(from[0], "that" | "it" | "this") {
            continue;
        }
        out.push((from.join(" "), to.join(" ")));
    }
    out
}

/// Parse a whole utterance. `None` = not an edit command (paste it).
/// `Replace` is returned with the LAST valid split; callers that have the
/// target text should use [`replace_candidates`] to pick the split whose
/// `from` is present.
/// Rewrite phrasings. Deliberately narrow: "fix that" / "shorter" alone are
/// everyday agent prompts and are NOT here.
fn rewrite(s: &str) -> Option<String> {
    let style = match s {
        "make that shorter"
        | "make it shorter"
        | "make this shorter"
        | "shorten that"
        | "make that more concise"
        | "make it more concise" => "shorter",
        "make that more formal"
        | "make it more formal"
        | "make this more formal"
        | "make that formal"
        | "make that sound more formal" => "formal",
        "make that more casual"
        | "make it more casual"
        | "make that friendlier"
        | "make it friendlier"
        | "make that more friendly"
        | "make that sound friendlier" => "casual",
        "fix the grammar"
        | "fix grammar"
        | "fix the grammar in that"
        | "fix the grammar of that"
        | "correct the grammar" => "grammar",
        _ => {
            for p in [
                "translate that to ",
                "translate that into ",
                "translate it to ",
                "translate it into ",
                "translate this to ",
                "translate this into ",
            ] {
                if let Some(lang) = s.strip_prefix(p) {
                    let n = lang.split(' ').count();
                    if (1..=2).contains(&n) {
                        return Some(format!("translate:{lang}"));
                    }
                }
            }
            return None;
        }
    };
    Some(style.to_string())
}

/// `parse`, also accepting the utterance addressed to Iris ("Iris, scratch
/// that", "Hey Iris make that shorter"). Returns (command, addressed).
pub fn parse_addressed(utterance: &str) -> Option<(EditCommand, bool)> {
    let words: Vec<&str> = utterance.split_whitespace().collect();
    let w = |i: usize| words.get(i).map(|x| normalize(x)).unwrap_or_default();
    let skip = if w(0) == "iris" {
        1
    } else if matches!(w(0).as_str(), "hey" | "ok" | "okay") && w(1) == "iris" {
        2
    } else {
        0
    };
    if skip > 0 {
        let rest = words[skip..].join(" ");
        return parse(&rest).map(|c| (c, true));
    }
    parse(utterance).map(|c| (c, false))
}

pub fn parse(utterance: &str) -> Option<EditCommand> {
    // Cheap pre-filter before any allocation-heavy work: real dictation is
    // overwhelmingly longer than any command.
    if utterance.len() > 120 || utterance.split_whitespace().count() > MAX_COMMAND_WORDS {
        return None;
    }
    let norm = normalize(utterance);
    if norm.is_empty() {
        return None;
    }
    let core = strip_polite(&norm);
    if let Some(cmd) = exact(core) {
        return Some(cmd);
    }
    if let Some(style) = rewrite(core) {
        return Some(EditCommand::Rewrite { style });
    }
    let cased = normalize_cased(utterance);
    // Polite affixes are stripped by position, so trim the cased form to
    // the same word window.
    let skip = norm.split(' ').count() - core.split(' ').count();
    let lead = if norm.starts_with(core) {
        0
    } else {
        skip.min(1)
    };
    let cased_core: Vec<&str> = cased
        .split(' ')
        .skip(lead)
        .take(core.split(' ').count())
        .collect();
    let cands = replace_candidates_cased(core, &cased_core.join(" "));
    cands
        .into_iter()
        .last()
        .map(|(from, to)| EditCommand::Replace { from, to })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_phrases_parse() {
        assert_eq!(parse("Scratch that."), Some(EditCommand::ScratchThat));
        assert_eq!(parse("delete that"), Some(EditCommand::ScratchThat));
        assert_eq!(parse("New line."), Some(EditCommand::NewLine));
        assert_eq!(parse("New paragraph"), Some(EditCommand::NewParagraph));
        assert_eq!(parse("Undo."), Some(EditCommand::Undo));
        assert_eq!(parse("Redo that"), Some(EditCommand::Redo));
        assert_eq!(
            parse("Capitalise that."),
            Some(EditCommand::Case(CaseKind::Capitalize))
        );
        assert_eq!(parse("Make that a list."), Some(EditCommand::MakeList));
        assert_eq!(
            parse("Select the last sentence."),
            Some(EditCommand::Select(Unit::LastSentence))
        );
        assert_eq!(
            parse("Delete the last word"),
            Some(EditCommand::Delete(Unit::LastWord))
        );
        assert_eq!(parse("Go to the end."), Some(EditCommand::GoTo(Place::End)));
        assert_eq!(parse("Please scratch that"), Some(EditCommand::ScratchThat));
        assert_eq!(parse("Okay, undo."), Some(EditCommand::Undo));
    }

    #[test]
    fn replace_parses_both_verbs() {
        assert_eq!(
            parse("Change cat to dog."),
            Some(EditCommand::Replace {
                from: "cat".into(),
                to: "dog".into()
            })
        );
        assert_eq!(
            parse("Replace Tuesday with Wednesday"),
            Some(EditCommand::Replace {
                from: "tuesday".into(),
                to: "Wednesday".into()
            })
        );
        assert_eq!(
            parse("Please change cloud code to Claude Code."),
            Some(EditCommand::Replace {
                from: "cloud code".into(),
                to: "Claude Code".into()
            })
        );
        // Ambiguous split: both offered, executor chooses by presence.
        let c = replace_candidates("change to to two");
        assert!(c.contains(&("to".into(), "two".into())));
    }

    #[test]
    fn sentences_around_commands_never_parse() {
        for s in [
            "Scratch that, I meant the other file",
            "Can you delete that file for me",
            "Undo the last commit",
            "Change the API to use REST and then deploy it to production please now",
            "I want to change the color to blue so it matches the header design",
            "new line of code",
            "Press enter to continue",
            "Enter your password",
            "Send it to John tomorrow",
            "Okay so delete that and start again",
            "change that to something nicer",
            "",
            "   ",
        ] {
            assert_eq!(parse(s), None, "{s:?}");
        }
    }

    #[test]
    fn press_enter_phrases() {
        let defaults = enter_phrases(None);
        for s in [
            "Press enter.",
            "press enter",
            "Hit enter!",
            "Send it.",
            "Submit that",
            "Okay, send it.",
        ] {
            assert!(is_enter_phrase(s, &defaults), "{s:?}");
        }
        // A bare "enter" is dictation: a lone short word is the most common
        // mis-hearing, and pressing Return on it would send.
        assert!(!is_enter_phrase("Enter.", &defaults));
        for s in [
            "Press enter to continue",
            "Enter your password",
            "Send it to John tomorrow",
        ] {
            assert!(!is_enter_phrase(s, &defaults), "{s:?}");
        }
        // Never a fixed-grammar command: only through the setting.
        assert_eq!(parse("Press enter."), None);
    }

    #[test]
    fn a_phrase_ending_the_dictation_is_split_off() {
        let mine = enter_phrases(Some("go go"));
        let split = |t: &str| split_trailing_enter(t, &mine);
        assert_eq!(
            split("Please add a test and run it go go").as_deref(),
            Some("Please add a test and run it")
        );
        assert_eq!(
            split("Please add a test and run it, go go.").as_deref(),
            Some("Please add a test and run it")
        );
        assert_eq!(
            split("Run the tests. Go, go!").as_deref(),
            Some("Run the tests.")
        );
        // A phrase alone is the voice-edit command, not a split.
        assert_eq!(split("Go go."), None);
        // Not at the end: dictation.
        assert_eq!(split("go go gadget is a cartoon"), None);
        assert_eq!(split("Let it go"), None);
        // With the defaults, the multi-word phrase that ends it is cut.
        let defaults = enter_phrases(None);
        assert_eq!(
            split_trailing_enter("Looks good, send it.", &defaults).as_deref(),
            Some("Looks good")
        );
    }

    /// Why event_loop matches the phrase BEFORE `local_cleanup`: the cleanup
    /// removes adjacent repeats, and a user's phrase may be one ("go go").
    #[test]
    fn a_repeated_word_phrase_matches_only_before_cleanup() {
        let mine = enter_phrases(Some("go go"));
        assert!(is_enter_phrase("Go go.", &mine));
        let cleaned = crate::always::postprocess::local_cleanup("Go go.");
        assert!(
            !is_enter_phrase(&cleaned, &mine),
            "cleanup gave {cleaned:?}"
        );
    }

    #[test]
    fn enter_phrases_are_the_users() {
        let mine = enter_phrases(Some(" Go ahead!, ship it ,, "));
        assert_eq!(mine, ["go ahead", "ship it"]);
        assert!(is_enter_phrase("Go ahead.", &mine));
        assert!(is_enter_phrase("Okay, ship it", &mine));
        // The defaults no longer apply once the user chose their own.
        assert!(!is_enter_phrase("Press enter.", &mine));
        // Blank or only over-long entries fall back to the defaults.
        assert_eq!(enter_phrases(Some(" , ")), enter_phrases(None));
        let long = "one two three four five six seven eight nine ten eleven twelve thirteen";
        assert_eq!(enter_phrases(Some(long)), enter_phrases(None));
    }

    #[test]
    fn rewrite_and_addressed() {
        assert_eq!(
            parse("Make that shorter."),
            Some(EditCommand::Rewrite {
                style: "shorter".into()
            })
        );
        assert_eq!(
            parse("Translate that to Nepali."),
            Some(EditCommand::Rewrite {
                style: "translate:nepali".into()
            })
        );
        assert_eq!(parse("Fix that."), None);
        assert_eq!(parse("Make it shorter and add a summary at the top"), None);
        assert_eq!(
            parse_addressed("Iris, scratch that."),
            Some((EditCommand::ScratchThat, true))
        );
        assert_eq!(
            parse_addressed("Hey Iris, make that more formal"),
            Some((
                EditCommand::Rewrite {
                    style: "formal".into()
                },
                true
            ))
        );
        assert_eq!(
            parse_addressed("scratch that"),
            Some((EditCommand::ScratchThat, false))
        );
        assert_eq!(parse_addressed("Iris, what time is it?"), None);
    }

    #[test]
    fn normalize_strips_punctuation_and_case() {
        assert_eq!(normalize("  Scratch   THAT!! "), "scratch that");
        assert_eq!(
            normalize("Change \u{201C}cat\u{201D}, to dog."),
            "change cat to dog"
        );
        assert_eq!(normalize("don\u{2019}t"), "don't");
    }
}
