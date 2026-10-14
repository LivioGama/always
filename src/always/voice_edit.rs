//! Voice editing of what was just dictated: "scratch that", "change X to
//! Y", "capitalise that", "make that a list", "new line", "undo", "select
//! the last sentence", "delete the last word", "go to the end", …
//!
//! Always owns this because it knows exactly what it pasted and where:
//! every successful paste pushes an [`Anchor`] (the exact payload, the app,
//! and — snapshotted off the paste path — the focused AX element).
//!
//! SAFETY CONTRACT (dictation is never swallowed or slowed):
//! - An utterance is claimed ONLY when the whole utterance is an edit
//!   command ([`voice_edit_grammar::parse`]) AND [`gate_verdict`] passes:
//!   a paste happened ≤ [`EDIT_WINDOW`] ago, in the same app and the same
//!   AX field, nothing committed it (auto-enter Return), and the user has
//!   not typed or clicked since.
//! - The command must also be executable right now (the paste is still
//!   where we left it, the field is writable, not a secure field). If not,
//!   the utterance is pasted as normal dictation.
//! - If execution fails, the original selection is restored and the
//!   utterance is pasted as normal dictation.
//! - Every grammar hit (claimed or declined) is appended to the shadow
//!   dataset `voice_edit_shadow.jsonl` in the config dir.
//!
//! Primary path = native AX: set the selected range, replace the selected
//! text (one undoable edit), put the caret back. Fallback = synthesised
//! keystrokes, used only when the field exposes no writable AX text and
//! the field is provably untouched since the paste.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::always::ax_edit::{self, AxElem, KeyTarget, Mods, Stroke, keys, len_u16, u16_to_byte};
use crate::always::voice_edit_grammar::{
    self as grammar, CaseKind, EditCommand, Place, Unit, normalize, normalize_cased,
};

/// A paste older than this can no longer be edited by a bare command.
pub const EDIT_WINDOW: Duration = Duration::from_secs(30);
/// Pastes remembered per field (so "scratch that" twice removes two).
const MAX_ANCHORS: usize = 8;
/// Our own synthetic events (⌘V, edit keys) reach the keyboard tap
/// asynchronously; input inside this window after we post is ours.
const SELF_POST_GRACE: Duration = Duration::from_millis(250);
/// Delay before snapshotting the focused element after a paste — off the
/// paste thread, so the paste itself is never slowed.
const SNAPSHOT_DELAY: Duration = Duration::from_millis(60);

#[derive(Debug, Clone)]
pub struct Anchor {
    pub id: u64,
    /// Exact payload written (fresh paste: "Text " with trailing space;
    /// merge: the delta, often with a leading space).
    pub text: String,
    pub at: Instant,
    pub app: Option<String>,
    pub elem: Option<AxElem>,
    /// Paste landed in a secure (password) field: never edit.
    pub secure: bool,
}

impl Anchor {
    /// Replay helper: an anchor `age_ms` old with no AX snapshot.
    pub fn for_replay(text: &str, age_ms: i64) -> Anchor {
        let age = Duration::from_millis(age_ms.max(0) as u64);
        Anchor {
            id: 0,
            text: format!("{} ", text.trim_end()),
            at: Instant::now().checked_sub(age).unwrap_or_else(Instant::now),
            app: None,
            elem: None,
            secure: false,
        }
    }
}

#[derive(Default)]
struct State {
    anchors: Vec<Anchor>,
    next_id: u64,
    committed_at: Option<Instant>,
    user_input_at: Option<Instant>,
    /// Last edit whose effect on the text we do not model (undo, new line,
    /// delete word…): keystroke arithmetic on the anchor is unsafe after it.
    untracked_at: Option<Instant>,
    self_post_until: Option<Instant>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

// ---------------------------------------------------------------------------
// Hooks called from the existing pipeline (each one line at the call site)
// ---------------------------------------------------------------------------

/// After a successful paste (`event_loop::deliver_paste`). `payload` is the
/// exact clipboard text that was pasted.
pub fn note_paste(payload: &str) {
    note_paste_in(payload, None, false);
}

/// [`note_paste`] aimed at one app (`pid`) — the live test uses this so the
/// field snapshot never depends on which app is frontmost. `sync` takes the
/// snapshot inline instead of on a background thread.
pub fn note_paste_in(payload: &str, pid: Option<i32>, sync: bool) {
    if payload.trim().is_empty() {
        return;
    }
    let app = crate::always::pause::current_app();
    let id = {
        let mut st = STATE.lock();
        let now = Instant::now();
        st.self_post_until = Some(now + SELF_POST_GRACE);
        // A paste into another app starts a new stack.
        if st.anchors.last().is_some_and(|a| a.app != app) {
            st.anchors.clear();
        }
        st.next_id += 1;
        let id = st.next_id;
        st.anchors.push(Anchor {
            id,
            text: payload.to_string(),
            at: now,
            app,
            elem: None,
            secure: false,
        });
        if st.anchors.len() > MAX_ANCHORS {
            st.anchors.remove(0);
        }
        id
    };
    let snap = move || {
        if !sync {
            std::thread::sleep(SNAPSHOT_DELAY);
        }
        let elem = match pid {
            Some(p) => ax_edit::focused_element_of(p),
            None => ax_edit::focused_element(),
        };
        let mut st = STATE.lock();
        // A paste into a different field than the previous anchors drops
        // them: "scratch that" only ever walks back within one field.
        let new_field = elem.clone();
        if let Some(e) = &new_field {
            let differs = st
                .anchors
                .iter()
                .filter(|a| a.id != id)
                .any(|a| a.elem.as_ref().is_some_and(|p| !p.same_as(e)));
            if differs {
                st.anchors.retain(|a| a.id == id);
            }
        }
        if let Some(a) = st.anchors.iter_mut().find(|a| a.id == id) {
            a.secure = elem.as_ref().is_some_and(|e| e.is_secure());
            a.elem = elem;
        }
    };
    if sync {
        snap();
    } else {
        std::thread::spawn(snap);
    }
}

/// Auto-enter pressed Return: the pasted text was submitted.
pub fn note_committed() {
    let mut st = STATE.lock();
    st.committed_at = Some(Instant::now());
    st.self_post_until = Some(Instant::now() + SELF_POST_GRACE);
}

/// A real key press or mouse click from the user (keyboard tap).
pub fn note_user_input() {
    let now = Instant::now();
    let mut st = STATE.lock();
    if st.self_post_until.is_some_and(|t| now < t) {
        return;
    }
    if st.anchors.is_empty() {
        return;
    }
    st.user_input_at = Some(now);
}

fn mark_self_post() {
    STATE.lock().self_post_until = Some(Instant::now() + SELF_POST_GRACE);
}

static REWRITER_ALIVE: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));
/// Iris pings `VoiceEditRewriterAlive` well inside this.
const REWRITER_TTL: Duration = Duration::from_secs(15);

/// Iris's rewrite handler is connected (heartbeat over UDS).
pub fn note_rewriter_alive() {
    *REWRITER_ALIVE.lock() = Some(Instant::now());
}

pub fn rewriter_alive() -> bool {
    REWRITER_ALIVE.lock().is_some_and(|t| t.elapsed() <= REWRITER_TTL)
}

// ---------------------------------------------------------------------------
// Gate (pure)
// ---------------------------------------------------------------------------

pub struct GateInput<'a> {
    pub cmd: &'a EditCommand,
    pub anchor: Option<&'a Anchor>,
    pub same_app: bool,
    /// `None` when AX could not identify the field at paste or now.
    pub same_field: Option<bool>,
    pub committed_since_paste: bool,
    pub user_input_since_paste: bool,
}

/// Whether a parsed command may claim the utterance. Pure: the replay test
/// runs this exact function over the user's real logs.
pub fn gate_verdict(g: &GateInput<'_>) -> Result<(), &'static str> {
    let Some(anchor) = g.anchor else { return Err("no_recent_paste") };
    if anchor.at.elapsed() > EDIT_WINDOW {
        return Err("window_expired");
    }
    if anchor.secure {
        return Err("secure_field");
    }
    if !g.same_app {
        return Err("app_changed");
    }
    if g.same_field == Some(false) {
        return Err("field_changed");
    }
    if g.committed_since_paste {
        return Err("committed");
    }
    if g.user_input_since_paste {
        return Err("user_input_since_paste");
    }
    if let EditCommand::Replace { .. } = g.cmd {
        // Find-replace by bare command only inside what was just pasted.
        if replace_in(&anchor.text, g.cmd, None).is_none() {
            return Err("replace_target_absent");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Text helpers (pure)
// ---------------------------------------------------------------------------

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '\u{2019}'
}

/// Last case-insensitive, word-bounded occurrence of `needle` in `hay`
/// (byte range). Whitespace in `needle` matches any whitespace run.
pub fn find_word_ci(hay: &str, needle: &str) -> Option<(usize, usize)> {
    let nwords: Vec<&str> = needle.split_whitespace().collect();
    if nwords.is_empty() {
        return None;
    }
    let mut best = None;
    for (start, _) in hay.char_indices() {
        if hay[..start].chars().next_back().is_some_and(is_word_char) {
            continue;
        }
        let mut pos = start;
        let mut ok = true;
        for (wi, w) in nwords.iter().enumerate() {
            if wi > 0 {
                let ws: usize = hay[pos..]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .map(char::len_utf8)
                    .sum();
                if ws == 0 {
                    ok = false;
                    break;
                }
                pos += ws;
            }
            let mut hc = hay[pos..].chars();
            for nc in w.chars() {
                match hc.next() {
                    Some(h) if h.to_lowercase().eq(nc.to_lowercase()) => pos += h.len_utf8(),
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                break;
            }
        }
        if ok && !hay[pos..].chars().next().is_some_and(is_word_char) {
            best = Some((start, pos));
        }
    }
    best
}

/// Match the casing of `original` onto `replacement`: ALL CAPS stays all
/// caps, a capitalised word stays capitalised; otherwise the spoken casing.
pub fn adapt_case(original: &str, replacement: &str) -> String {
    let letters: Vec<char> = original.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() > 1 && letters.iter().all(|c| c.is_uppercase()) {
        return replacement.to_uppercase();
    }
    let orig_cap = original.chars().next().is_some_and(char::is_uppercase);
    let mut chars = replacement.chars();
    match chars.next() {
        Some(f) if orig_cap && f.is_lowercase() => f.to_uppercase().chain(chars).collect(),
        _ => replacement.to_string(),
    }
}

/// Apply a `Replace` to `text`. Returns (new_text, byte range replaced in
/// `text`, original matched slice, replacement). `utterance` (when known)
/// lets an ambiguous split ("change to to two") pick the one present.
pub fn replace_in(
    text: &str,
    cmd: &EditCommand,
    utterance: Option<&str>,
) -> Option<(String, (usize, usize), String, String)> {
    let EditCommand::Replace { from, to } = cmd else { return None };
    let mut cands = vec![(from.clone(), to.clone())];
    if let Some(u) = utterance {
        let core_norm = normalize(u);
        let core_cased = normalize_cased(u);
        let mut all = grammar::replace_candidates_cased(&core_norm, &core_cased);
        all.reverse();
        cands = all.into_iter().chain(cands).collect();
    }
    for (f, t) in cands {
        if let Some((s, e)) = find_word_ci(text, &f) {
            let orig = text[s..e].to_string();
            let rep = adapt_case(&orig, &t);
            let mut out = String::with_capacity(text.len() + rep.len());
            out.push_str(&text[..s]);
            out.push_str(&rep);
            out.push_str(&text[e..]);
            return Some((out, (s, e), orig, rep));
        }
    }
    None
}

/// Split `text` into (leading ws, core, trailing ws).
fn ws_split(text: &str) -> (&str, &str, &str) {
    let core = text.trim();
    let lead = &text[..text.len() - text.trim_start().len()];
    let trail = &text[text.trim_end().len()..];
    (lead, core, trail)
}

pub fn transform_case(text: &str, kind: &CaseKind) -> String {
    match kind {
        CaseKind::Upper => text.to_uppercase(),
        CaseKind::Lower => text.to_lowercase(),
        CaseKind::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut at_start = true;
            for c in text.chars() {
                if at_start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    at_start = false;
                } else {
                    out.push(c);
                    if c.is_whitespace() || c == '-' {
                        at_start = true;
                    } else if c.is_alphanumeric() {
                        at_start = false;
                    }
                }
            }
            out
        }
    }
}

/// "make that a list": one item per sentence; a single sentence is split
/// on commas / semicolons / " and ".
pub fn make_list(text: &str) -> String {
    let core = text.trim();
    let mut items: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in core.chars() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?' | '\n') {
            items.push(std::mem::take(&mut cur));
        }
    }
    items.push(cur);
    let mut items: Vec<String> = items
        .into_iter()
        .map(|s| s.trim().trim_end_matches(['.', '\n']).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if items.len() < 2 {
        let one = items.pop().unwrap_or_default();
        items = one
            .split([',', ';'])
            .flat_map(|p| p.split(" and "))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    items
        .iter()
        .map(|s| {
            let mut c = s.chars();
            match c.next() {
                Some(f) => format!("- {}{}", f.to_uppercase(), c.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Locate the anchor payload in the field: it must end at the caret (only
/// whitespace between). Returns the UTF-16 range to remove for "scratch
/// that" (including the payload's own leading space).
pub fn locate_anchor(value: &str, caret_u16: usize, payload: &str) -> Option<(usize, usize)> {
    let caret_b = u16_to_byte(value, caret_u16);
    let before = &value[..caret_b];
    let core = payload.trim();
    if core.is_empty() {
        return None;
    }
    let b_trim = before.trim_end();
    if !b_trim.ends_with(core) {
        return None;
    }
    let mut start = b_trim.len() - core.len();
    // Include the payload's leading whitespace when it is really there.
    let lead = &payload[..payload.len() - payload.trim_start().len()];
    if !lead.is_empty() && value[..start].ends_with(lead) {
        start -= lead.len();
    }
    // Trailing whitespace between payload and caret: ours only if the
    // payload had trailing whitespace.
    let end_b = if payload.ends_with(char::is_whitespace) { caret_b } else { b_trim.len() };
    Some((ax_edit::byte_to_u16(value, start), ax_edit::byte_to_u16(value, end_b)))
}

/// Range (UTF-16) of the last word before the caret plus the whitespace
/// after it up to the caret. `with_trailing=false` → the word only.
pub fn last_word_range(value: &str, caret_u16: usize, with_trailing: bool) -> Option<(usize, usize)> {
    let caret_b = u16_to_byte(value, caret_u16);
    let before = &value[..caret_b];
    let trimmed = before.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    let start = trimmed
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let end = if with_trailing { caret_b } else { trimmed.len() };
    Some((ax_edit::byte_to_u16(value, start), ax_edit::byte_to_u16(value, end)))
}

/// Range (UTF-16) of the last sentence before the caret. Sentence ends are
/// `.`, `!`, `?` or a newline; the sentence's own terminator is included.
pub fn last_sentence_range(value: &str, caret_u16: usize, with_trailing: bool) -> Option<(usize, usize)> {
    let caret_b = u16_to_byte(value, caret_u16);
    let before = &value[..caret_b];
    let trimmed = before.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    // Skip the sentence's own terminator(s).
    let body = trimmed.trim_end_matches(['.', '!', '?']);
    let start = body
        .char_indices()
        .rev()
        .find(|(_, c)| matches!(c, '.' | '!' | '?' | '\n'))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let start = start + (trimmed[start..].len() - trimmed[start..].trim_start().len());
    let end = if with_trailing { caret_b } else { trimmed.len() };
    Some((ax_edit::byte_to_u16(value, start), ax_edit::byte_to_u16(value, end)))
}

fn utf16_slice(value: &str, loc: usize, len: usize) -> &str {
    let s = u16_to_byte(value, loc);
    let e = u16_to_byte(value, loc + len);
    &value[s..e]
}

// ---------------------------------------------------------------------------
// Planning (pure given a field snapshot)
// ---------------------------------------------------------------------------

/// What the focused field looks like right now.
#[derive(Debug, Clone, Default)]
pub struct Field {
    pub value: Option<String>,
    pub sel: Option<(usize, usize)>,
    /// AXSelectedText is writable → AX edit path available.
    pub writable: bool,
    pub terminal: bool,
}

impl Field {
    fn ax(&self) -> Option<(&str, (usize, usize))> {
        match (&self.value, self.sel, self.writable) {
            (Some(v), Some(s), true) => Some((v.as_str(), s)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Replace UTF-16 range with `text`, then put the caret at `caret`.
    Replace { loc: usize, len: usize, text: String, caret: usize },
    Select { loc: usize, len: usize },
    Caret(usize),
    Keys(Vec<Stroke>),
    /// Rewrite: Iris does it (on-device model) through the
    /// `VoiceEditGetTarget` / `VoiceEditReplaceTarget` primitives.
    Handoff { style: String },
}

/// A word the user corrected that looks like a recognition error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Learn {
    pub wrong: String,
    pub right: String,
}

/// Effect on the anchor stack after a successful edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorEffect {
    Pop,
    /// Top anchor now reads this text.
    Rewrite(String),
    /// Field changed in a way we do not track: AX re-verifies later, but
    /// keystroke fallbacks must not trust the anchor any more.
    Untracked,
    None,
}

pub struct Planned {
    pub plan: Plan,
    pub learn: Option<Learn>,
    pub effect: AnchorEffect,
    pub method: &'static str,
}

fn key(code: u16, mods: Mods, repeat: usize) -> Stroke {
    Stroke::Key { code, mods, repeat }
}

fn learn_from(orig: &str, rep: &str) -> Option<Learn> {
    let o = orig.trim_matches(|c: char| !c.is_alphanumeric());
    let r = rep.trim_matches(|c: char| !c.is_alphanumeric());
    if o.is_empty() || r.is_empty() || o.eq_ignore_ascii_case(r) {
        return None;
    }
    let sounds_alike = crate::always::text_match::soundex(o) == crate::always::text_match::soundex(r);
    (crate::always::correction::looks_like_correction(&o.to_lowercase(), &r.to_lowercase())
        || sounds_alike)
        .then(|| Learn { wrong: o.to_string(), right: r.to_string() })
}

/// Decide how to execute `cmd`. `anchor` = the payload of the last paste
/// (None when forced by Iris with no anchor). `keys_exact` = the field is
/// provably untouched since that paste (keystroke math is exact).
/// `forced` = Iris was addressed explicitly: "change X to Y" may then
/// search the focused field, not just the last paste.
pub fn plan(
    cmd: &EditCommand,
    utterance: &str,
    anchor: Option<&str>,
    field: &Field,
    keys_exact: bool,
    forced: bool,
) -> Result<Planned, &'static str> {
    let ax = field.ax();
    // Anchor range in the field (AX mode). A readable, writable field
    // whose text no longer ends with our paste means the user edited it.
    let anchor_range = match (ax, anchor) {
        (Some((v, (loc, len))), Some(a)) if len == 0 => locate_anchor(v, loc, a),
        _ => None,
    };
    let caret_only = |p: Plan, effect: AnchorEffect, method| {
        Ok(Planned { plan: p, learn: None, effect, method })
    };
    match cmd {
        EditCommand::ScratchThat | EditCommand::Delete(Unit::That) => {
            let a = anchor.ok_or("no_anchor")?;
            if let Some((s, e)) = anchor_range {
                return caret_only(
                    Plan::Replace { loc: s, len: e - s, text: String::new(), caret: s },
                    AnchorEffect::Pop,
                    "ax",
                );
            }
            if ax.is_some() {
                return Err("anchor_not_in_field");
            }
            if !keys_exact {
                return Err("field_not_verifiable");
            }
            caret_only(
                Plan::Keys(vec![key(keys::DELETE, Mods::NONE, a.chars().count())]),
                AnchorEffect::Pop,
                "keys",
            )
        }
        EditCommand::Replace { .. } => {
            if let (Some((v, _)), Some((s, e))) = (ax, anchor_range) {
                let region = utf16_slice(v, s, e - s);
                if let Some((new_region, _, orig, rep)) = replace_in(region, cmd, Some(utterance)) {
                    let caret = s + len_u16(&new_region);
                    return Ok(Planned {
                        plan: Plan::Replace { loc: s, len: e - s, text: new_region.clone(), caret },
                        learn: learn_from(&orig, &rep),
                        effect: AnchorEffect::Rewrite(new_region),
                        method: "ax",
                    });
                }
            }
            if forced && let Some((v, (loc, len))) = ax {
                // Iris-addressed: last occurrence before the caret.
                let caret_b = u16_to_byte(v, loc + len);
                if let Some((_, (bs, be), orig, rep)) = replace_in(&v[..caret_b], cmd, Some(utterance)) {
                    let s = ax_edit::byte_to_u16(v, bs);
                    let e = ax_edit::byte_to_u16(v, be);
                    let caret = (loc + len) - (e - s) + len_u16(&rep);
                    return Ok(Planned {
                        plan: Plan::Replace { loc: s, len: e - s, text: rep.clone(), caret },
                        learn: learn_from(&orig, &rep),
                        effect: AnchorEffect::Untracked,
                        method: "ax",
                    });
                }
                return Err("replace_target_absent");
            }
            if ax.is_some() {
                return Err("anchor_not_in_field");
            }
            let a = anchor.ok_or("no_anchor")?;
            if !keys_exact {
                return Err("field_not_verifiable");
            }
            let (new_text, (bs, _), orig, rep) =
                replace_in(a, cmd, Some(utterance)).ok_or("replace_target_absent")?;
            // Erase from the match to the end, retype the new tail.
            let erase = a[bs..].chars().count();
            let tail = new_text[bs..].to_string();
            Ok(Planned {
                plan: Plan::Keys(vec![key(keys::DELETE, Mods::NONE, erase), Stroke::Text(tail)]),
                learn: learn_from(&orig, &rep),
                effect: AnchorEffect::Rewrite(new_text),
                method: "keys",
            })
        }
        EditCommand::Case(_) | EditCommand::MakeList => {
            let a = anchor.ok_or("no_anchor")?;
            let transform = |t: &str| -> String {
                let (lead, core, trail) = ws_split(t);
                let body = match cmd {
                    EditCommand::Case(k) => transform_case(core, k),
                    _ => make_list(core),
                };
                // A list starts on its own line.
                let lead = if matches!(cmd, EditCommand::MakeList) && !lead.is_empty() {
                    "\n"
                } else {
                    lead
                };
                format!("{lead}{body}{trail}")
            };
            if let (Some((v, _)), Some((s, e))) = (ax, anchor_range) {
                let new_region = transform(utf16_slice(v, s, e - s));
                let caret = s + len_u16(&new_region);
                return caret_only(
                    Plan::Replace { loc: s, len: e - s, text: new_region.clone(), caret },
                    AnchorEffect::Rewrite(new_region),
                    "ax",
                );
            }
            if ax.is_some() {
                return Err("anchor_not_in_field");
            }
            if !keys_exact {
                return Err("field_not_verifiable");
            }
            let new_text = transform(a);
            caret_only(
                Plan::Keys(vec![
                    key(keys::DELETE, Mods::NONE, a.chars().count()),
                    Stroke::Text(new_text.clone()),
                ]),
                AnchorEffect::Rewrite(new_text),
                "keys",
            )
        }
        EditCommand::NewLine | EditCommand::NewParagraph => {
            let nl = if matches!(cmd, EditCommand::NewLine) { "\n" } else { "\n\n" };
            if let Some((_, (loc, len))) = ax {
                return caret_only(
                    Plan::Replace { loc, len, text: nl.into(), caret: loc + nl.len() },
                    AnchorEffect::Untracked,
                    "ax",
                );
            }
            // Shift+Return = newline without sending in chat apps and
            // agent prompts (bare Return would submit).
            caret_only(
                Plan::Keys(vec![key(keys::RETURN, Mods::SHIFT, nl.len())]),
                AnchorEffect::Untracked,
                "keys",
            )
        }
        EditCommand::Undo => caret_only(
            Plan::Keys(vec![key(keys::Z, Mods::CMD, 1)]),
            AnchorEffect::Untracked,
            "keys",
        ),
        EditCommand::Redo => caret_only(
            Plan::Keys(vec![key(keys::Z, Mods::CMD_SHIFT, 1)]),
            AnchorEffect::Untracked,
            "keys",
        ),
        EditCommand::Select(Unit::All) => caret_only(
            Plan::Keys(vec![key(keys::A, Mods::CMD, 1)]),
            AnchorEffect::None,
            "keys",
        ),
        EditCommand::Select(Unit::That) => {
            if let Some((s, e)) = anchor_range {
                return caret_only(Plan::Select { loc: s, len: e - s }, AnchorEffect::None, "ax");
            }
            if ax.is_some() {
                return Err("anchor_not_in_field");
            }
            let a = anchor.ok_or("no_anchor")?;
            if !keys_exact || field.terminal {
                return Err("field_not_verifiable");
            }
            caret_only(
                Plan::Keys(vec![key(keys::LEFT, Mods::SHIFT, a.chars().count())]),
                AnchorEffect::None,
                "keys",
            )
        }
        EditCommand::Select(unit @ (Unit::LastWord | Unit::LastSentence)) => {
            if let Some((v, (loc, len))) = ax {
                let caret = loc + len;
                let r = if *unit == Unit::LastWord {
                    last_word_range(v, caret, false)
                } else {
                    last_sentence_range(v, caret, false)
                };
                let (s, e) = r.ok_or("nothing_before_caret")?;
                return caret_only(Plan::Select { loc: s, len: e - s }, AnchorEffect::None, "ax");
            }
            if *unit == Unit::LastWord && !field.terminal {
                return caret_only(
                    Plan::Keys(vec![key(keys::LEFT, Mods::ALT_SHIFT, 1)]),
                    AnchorEffect::None,
                    "keys",
                );
            }
            Err("needs_ax")
        }
        EditCommand::Delete(unit @ (Unit::LastWord | Unit::LastSentence | Unit::All)) => {
            if let Some((v, (loc, len))) = ax {
                let caret = loc + len;
                let r = match unit {
                    Unit::LastWord => last_word_range(v, caret, true),
                    Unit::LastSentence => last_sentence_range(v, caret, true),
                    _ => None,
                };
                let (s, e) = r.ok_or("nothing_before_caret")?;
                return caret_only(
                    Plan::Replace { loc: s, len: e - s, text: String::new(), caret: s },
                    AnchorEffect::Untracked,
                    "ax",
                );
            }
            if *unit == Unit::LastWord {
                // ⌥⌫ deletes the previous word in Cocoa fields and in
                // terminals with readline-style bindings.
                return caret_only(
                    Plan::Keys(vec![key(keys::DELETE, Mods::ALT, 1)]),
                    AnchorEffect::Untracked,
                    "keys",
                );
            }
            Err("needs_ax")
        }
        EditCommand::Rewrite { style } => {
            // Something to rewrite must exist: a selection, or our paste
            // still sitting where we left it.
            let has_selection = ax.is_some_and(|(_, (_, len))| len > 0);
            if !has_selection && anchor_range.is_none() {
                return Err(if ax.is_some() { "anchor_not_in_field" } else { "needs_ax" });
            }
            caret_only(Plan::Handoff { style: style.clone() }, AnchorEffect::None, "iris")
        }
        EditCommand::GoTo(place) => {
            if let Some((v, _)) = ax {
                let at = if *place == Place::End { len_u16(v) } else { 0 };
                return caret_only(Plan::Caret(at), AnchorEffect::None, "ax");
            }
            let stroke = match (place, field.terminal) {
                (Place::End, true) => key(keys::E, Mods::CTRL, 1),
                (Place::Start, true) => key(keys::A, Mods::CTRL, 1),
                (Place::End, false) => key(keys::DOWN, Mods::CMD, 1),
                (Place::Start, false) => key(keys::UP, Mods::CMD, 1),
            };
            caret_only(Plan::Keys(vec![stroke]), AnchorEffect::None, "keys")
        }
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

const TERMINALS: &[&str] = &[
    "com.mitchellh.ghostty",
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "net.kovidgoyal.kitty",
    "com.github.wez.wezterm",
    "org.alacritty",
    "com.cmuxterm.app",
];

pub fn is_terminal(bundle: Option<&str>) -> bool {
    bundle.is_some_and(|b| TERMINALS.contains(&b))
}

/// Snapshot the focused field for planning.
pub fn snapshot(elem: Option<&AxElem>, terminal: bool) -> Field {
    let Some(e) = elem else {
        return Field { terminal, ..Field::default() };
    };
    Field {
        value: e.value(),
        sel: e.selected_range(),
        writable: e.settable("AXSelectedText") || e.settable("AXValue"),
        terminal,
    }
}

/// Run a plan. On failure the caller's selection is restored. Returns the
/// method actually used.
pub fn execute(
    planned: &Planned,
    elem: Option<&AxElem>,
    target: KeyTarget,
) -> Result<&'static str, &'static str> {
    match &planned.plan {
        Plan::Keys(strokes) => {
            mark_self_post();
            ax_edit::post_strokes(target, strokes).map_err(|_| "keys_failed")?;
            mark_self_post();
            Ok("keys")
        }
        Plan::Handoff { style } => {
            if !rewriter_alive() {
                return Err("rewriter_offline");
            }
            crate::always::event::global_broadcaster().send(crate::always::event::DaemonEvent::VoiceEdit {
                req: String::new(),
                op: "rewrite_request".into(),
                ok: "true".into(),
                method: "iris".into(),
                ms: String::new(),
                detail: style.clone(),
                text: String::new(),
            });
            Ok("iris")
        }
        Plan::Select { loc, len } => {
            let e = elem.ok_or("no_element")?;
            e.set_selected_range(*loc, *len).then_some("ax").ok_or("ax_select_failed")
        }
        Plan::Caret(at) => {
            let e = elem.ok_or("no_element")?;
            e.set_selected_range(*at, 0).then_some("ax").ok_or("ax_caret_failed")
        }
        Plan::Replace { loc, len, text, caret } => {
            let e = elem.ok_or("no_element")?;
            let before_sel = e.selected_range();
            let before_val = e.value().unwrap_or_default();
            let restore = |e: &AxElem| {
                if let Some((l, n)) = before_sel {
                    e.set_selected_range(l, n);
                }
            };
            if !e.set_selected_range(*loc, *len) || e.selected_range() != Some((*loc, *len)) {
                restore(e);
                return Err("ax_select_failed");
            }
            let expected = {
                let s = u16_to_byte(&before_val, *loc);
                let en = u16_to_byte(&before_val, *loc + *len);
                format!("{}{}{}", &before_val[..s], text, &before_val[en..])
            };
            let mut method = "ax";
            let wrote = if text.is_empty() {
                e.set_selected_text("")
            } else {
                e.set_selected_text(text)
            };
            let landed = |e: &AxElem| e.value().as_deref() == Some(expected.as_str());
            if !(wrote && landed(e)) {
                if e.value().as_deref() != Some(before_val.as_str()) {
                    // Something changed but not what we expected: never
                    // stack a second write on top.
                    return Err("ax_write_mismatch");
                }
                // AX write unsupported: the range is selected, so typing
                // replaces it (Backspace when deleting).
                mark_self_post();
                let strokes = if text.is_empty() {
                    vec![key(keys::DELETE, Mods::NONE, 1)]
                } else {
                    vec![Stroke::Text(text.clone())]
                };
                if ax_edit::post_strokes(target, &strokes).is_err() {
                    restore(e);
                    return Err("keys_failed");
                }
                mark_self_post();
                method = "ax_select+keys";
                // Key events are asynchronous: give the app a moment, then
                // verify (bounded).
                let deadline = Instant::now() + Duration::from_millis(150);
                while !landed(e) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if !landed(e) {
                    return Err("keys_unverified");
                }
            }
            e.set_selected_range(*caret, 0);
            Ok(method)
        }
    }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Outcome {
    pub claimed: bool,
    pub op: &'static str,
    pub method: &'static str,
    pub reason: &'static str,
    pub ms: f64,
}

fn shadow_path() -> std::path::PathBuf {
    crate::config::config_dir().join("voice_edit_shadow.jsonl")
}

fn shadow_log(utterance: &str, source: &str, out: &Outcome, app: Option<&str>) {
    use std::io::Write;
    let text = crate::always::telemetry::should_log_transcripts().then_some(utterance);
    let row = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "source": source,
        "utterance": text,
        "chars": utterance.chars().count(),
        "op": out.op,
        "claimed": out.claimed,
        "method": out.method,
        "reason": out.reason,
        "ms": (out.ms * 100.0).round() / 100.0,
        "app": app,
    });
    let path = shadow_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{row}");
    }
}

fn learn_async(l: Learn) {
    std::thread::spawn(move || {
        let pair = crate::always::correction::CorrectionPair { wrong: l.wrong, right: l.right };
        let auto = crate::db::open()
            .ok()
            .and_then(|c| crate::db::get_preferences(&c).ok())
            .and_then(|p| p.auto_learn_corrections)
            .unwrap_or(true);
        if auto {
            match crate::always::correction::apply_pairs_to_glossary_with_provenance(
                std::slice::from_ref(&pair),
                crate::always::correction::Provenance::Manual,
            ) {
                Ok(n) => tracing::info!(wrong = %pair.wrong, right = %pair.right, applied = n, "voice_edit_correction_learned"),
                Err(e) => tracing::warn!(error = %e, "voice_edit_correction_learn_failed"),
            }
        } else if let Ok(q) = crate::always::correction_queue::global_queue() {
            let _ = q.add(pair.clone(), crate::always::correction_queue::CorrectionSource::Hotkey);
            tracing::info!(wrong = %pair.wrong, right = %pair.right, "voice_edit_correction_queued");
        }
        crate::always::event::global_broadcaster().correction_logged(&pair.wrong, &pair.right);
    });
}

fn apply_effect(effect: &AnchorEffect) {
    let mut st = STATE.lock();
    let now = Instant::now();
    match effect {
        AnchorEffect::Pop => {
            st.anchors.pop();
        }
        AnchorEffect::Rewrite(t) => {
            if let Some(a) = st.anchors.last_mut() {
                a.text = t.clone();
            }
        }
        AnchorEffect::Untracked | AnchorEffect::None => {}
    }
    // Chained edits ("new line", then "scratch that"…) keep the window
    // open from the last edit; keystroke math stays exact only for
    // tracked effects.
    if let Some(a) = st.anchors.last_mut() {
        a.at = now;
    }
    if matches!(effect, AnchorEffect::Untracked) {
        st.untracked_at = Some(Instant::now());
    }
}

/// Daemon entry: called with the cleaned utterance before it is pasted.
/// `true` = claimed and executed; the caller must NOT paste it.
pub fn try_claim(utterance: &str) -> bool {
    run(utterance, false, "dictation", KeyTarget::Hid, None).claimed
}

/// Shared runner. `forced` = Iris addressed the command explicitly (the
/// paste-window gate is skipped; secure fields and verification still
/// apply). `pid` = aim AX + keys at this app instead of the focused one.
pub fn run(
    utterance: &str,
    forced: bool,
    source: &str,
    target: KeyTarget,
    pid: Option<i32>,
) -> Outcome {
    let Some((cmd, addressed)) = grammar::parse_addressed(utterance) else {
        return Outcome { claimed: false, op: "none", method: "", reason: "not_a_command", ms: 0.0 };
    };
    // "Iris, scratch that": addressed by name, so the paste-window gate
    // does not apply (verification and secure-field refusal still do).
    let forced = forced || addressed;
    let original = utterance;
    // Replace candidates are re-derived from the text without the address.
    let utterance_core = if addressed {
        utterance.split_whitespace().skip_while(|w| normalize(w) != "iris").skip(1).collect::<Vec<_>>().join(" ")
    } else {
        utterance.to_string()
    };
    let utterance = utterance_core.as_str();
    let t0 = Instant::now();
    let app = crate::always::pause::current_app();
    let mut out = Outcome { claimed: false, op: cmd.name(), method: "", reason: "", ms: 0.0 };
    let result = run_inner(&cmd, utterance, forced, target, pid, app.as_deref());
    out.ms = t0.elapsed().as_secs_f64() * 1000.0;
    match result {
        Ok(method) => {
            out.claimed = true;
            out.method = method;
            out.reason = "ok";
        }
        Err(reason) => out.reason = reason,
    }
    tracing::info!(
        op = out.op,
        claimed = out.claimed,
        method = out.method,
        reason = out.reason,
        ms = out.ms,
        source,
        "voice_edit"
    );
    shadow_log(original, source, &out, app.as_deref());
    if out.claimed {
        crate::always::event::global_broadcaster().send(
            crate::always::event::DaemonEvent::VoiceEdit {
                req: String::new(),
                op: out.op.to_string(),
                ok: "true".into(),
                method: out.method.to_string(),
                ms: format!("{:.1}", out.ms),
                detail: String::new(),
                text: String::new(),
            },
        );
    }
    out
}

fn run_inner(
    cmd: &EditCommand,
    utterance: &str,
    forced: bool,
    target: KeyTarget,
    pid: Option<i32>,
    app: Option<&str>,
) -> Result<&'static str, &'static str> {
    let (anchor, committed, user_input, untracked) = {
        let st = STATE.lock();
        let a = st.anchors.last().cloned();
        let since = |t: Option<Instant>| match (&a, t) {
            (Some(a), Some(t)) => t > a.at,
            _ => false,
        };
        (a.clone(), since(st.committed_at), since(st.user_input_at), since(st.untracked_at))
    };
    let elem = match pid {
        Some(p) => ax_edit::focused_element_of(p),
        None => ax_edit::focused_element(),
    };
    if elem.as_ref().is_some_and(|e| e.is_secure()) {
        return Err("secure_field");
    }
    let same_field = match (anchor.as_ref().and_then(|a| a.elem.as_ref()), elem.as_ref()) {
        (Some(a), Some(b)) => Some(a.same_as(b)),
        _ => None,
    };
    if !forced {
        gate_verdict(&GateInput {
            cmd,
            anchor: anchor.as_ref(),
            same_app: pid.is_some() || anchor.as_ref().is_some_and(|a| a.app.as_deref() == app),
            same_field,
            committed_since_paste: committed,
            user_input_since_paste: user_input,
        })?;
    } else if anchor.as_ref().is_some_and(|a| a.secure) {
        return Err("secure_field");
    }
    // A forced command only trusts the anchor if it is for this field.
    let anchor_text = anchor
        .as_ref()
        .filter(|a| a.at.elapsed() <= EDIT_WINDOW && same_field != Some(false))
        .map(|a| a.text.clone());
    let field = snapshot(elem.as_ref(), is_terminal(app));
    let keys_exact = !committed && !user_input && !untracked && anchor_text.is_some();
    let planned = plan(cmd, utterance, anchor_text.as_deref(), &field, keys_exact, forced)?;
    // Serialise with the paste path: never edit while a paste is landing.
    if !crate::always::pause::try_begin_paste() {
        return Err("paste_in_flight");
    }
    let res = execute(&planned, elem.as_ref(), target);
    crate::always::pause::end_paste();
    let method = res?;
    apply_effect(&planned.effect);
    // The session / merge buffer described text that just changed; the
    // next utterance starts fresh. Keep ⌃⌥X's diff baseline honest.
    crate::always::pause::dictation_buffer_clear();
    match &planned.effect {
        AnchorEffect::Pop => crate::always::pause::set_last_pasted(String::new()),
        AnchorEffect::Rewrite(t) => crate::always::pause::set_last_pasted(t.trim().to_string()),
        _ => {}
    }
    if let Some(l) = planned.learn.clone() {
        learn_async(l);
    }
    Ok(method)
}

// ---------------------------------------------------------------------------
// Iris primitives (UDS): read the edit target, replace it in one edit
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Target {
    pub text: String,
    /// "selection" | "last_paste" | "none"
    pub source: &'static str,
    pub secure: bool,
    pub loc: usize,
    pub len: usize,
}

/// What "that" refers to right now: the selection when non-empty, else the
/// last paste (if still verifiable in the focused field).
pub fn current_target(pid: Option<i32>) -> Target {
    let elem = match pid {
        Some(p) => ax_edit::focused_element_of(p),
        None => ax_edit::focused_element(),
    };
    let Some(e) = elem else { return Target { source: "none", ..Target::default() } };
    if e.is_secure() {
        return Target { source: "none", secure: true, ..Target::default() };
    }
    let (Some(v), Some((loc, len))) = (e.value(), e.selected_range()) else {
        return Target { source: "none", ..Target::default() };
    };
    if len > 0 {
        return Target {
            text: utf16_slice(&v, loc, len).to_string(),
            source: "selection",
            secure: false,
            loc,
            len,
        };
    }
    let anchor = STATE.lock().anchors.last().cloned();
    if let Some(a) = anchor.filter(|a| a.at.elapsed() <= EDIT_WINDOW)
        && let Some((s, en)) = locate_anchor(&v, loc, &a.text)
    {
        let raw = utf16_slice(&v, s, en - s);
        // Rewrite the words, not the padding around them.
        let lead = len_u16(&raw[..raw.len() - raw.trim_start().len()]);
        let core = raw.trim();
        return Target { text: core.to_string(), source: "last_paste", secure: false, loc: s + lead, len: len_u16(core) };
    }
    Target { source: "none", ..Target::default() }
}

/// Replace the current target with `text` as ONE undoable edit (⌘Z
/// restores the original). Refuses when the target changed since Iris read
/// it (`expect` must equal the target text) or the field is secure.
pub fn replace_target(expect: &str, text: &str, pid: Option<i32>, target: KeyTarget) -> Result<(&'static str, f64), &'static str> {
    let t0 = Instant::now();
    let t = current_target(pid);
    if t.secure {
        return Err("secure_field");
    }
    if t.source == "none" {
        return Err("no_target");
    }
    if t.text != expect {
        return Err("target_changed");
    }
    let elem = match pid {
        Some(p) => ax_edit::focused_element_of(p),
        None => ax_edit::focused_element(),
    };
    let planned = Planned {
        plan: Plan::Replace { loc: t.loc, len: t.len, text: text.to_string(), caret: t.loc + len_u16(text) },
        learn: None,
        effect: if t.source == "last_paste" { AnchorEffect::Rewrite(text.to_string()) } else { AnchorEffect::Untracked },
        method: "ax",
    };
    if !crate::always::pause::try_begin_paste() {
        return Err("paste_in_flight");
    }
    let res = execute(&planned, elem.as_ref(), target);
    crate::always::pause::end_paste();
    let method = res?;
    if t.source == "last_paste" {
        // Keep the paste's own padding: rewrite the stored payload.
        let mut st = STATE.lock();
        if let Some(a) = st.anchors.last_mut() {
            let (lead, _, trail) = ws_split(&a.text);
            a.text = format!("{lead}{text}{trail}");
            a.at = Instant::now();
        }
    }
    crate::always::pause::dictation_buffer_clear();
    Ok((method, t0.elapsed().as_secs_f64() * 1000.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(v: &str) -> Field {
        Field { value: Some(v.into()), sel: Some((len_u16(v), 0)), writable: true, terminal: false }
    }

    fn cmd(u: &str) -> EditCommand {
        grammar::parse(u).unwrap()
    }

    #[test]
    fn gate_requires_fresh_uncommitted_same_field_paste() {
        let a = Anchor::for_replay("Hello world.", 1000);
        let c = cmd("scratch that");
        let base = GateInput {
            cmd: &c,
            anchor: Some(&a),
            same_app: true,
            same_field: Some(true),
            committed_since_paste: false,
            user_input_since_paste: false,
        };
        assert_eq!(gate_verdict(&base), Ok(()));
        assert_eq!(gate_verdict(&GateInput { anchor: None, ..base }), Err("no_recent_paste"));
        let old = Anchor::for_replay("Hello world.", 31_000);
        assert_eq!(gate_verdict(&GateInput { anchor: Some(&old), ..base }), Err("window_expired"));
        assert_eq!(gate_verdict(&GateInput { same_app: false, ..base }), Err("app_changed"));
        assert_eq!(gate_verdict(&GateInput { same_field: Some(false), ..base }), Err("field_changed"));
        assert_eq!(gate_verdict(&GateInput { committed_since_paste: true, ..base }), Err("committed"));
        assert_eq!(
            gate_verdict(&GateInput { user_input_since_paste: true, ..base }),
            Err("user_input_since_paste")
        );
        let r = cmd("change dog to cat");
        assert_eq!(gate_verdict(&GateInput { cmd: &r, ..base }), Err("replace_target_absent"));
        let r = cmd("change world to there");
        assert_eq!(gate_verdict(&GateInput { cmd: &r, ..base }), Ok(()));
    }

    #[test]
    fn scratch_plans_exact_ax_range() {
        let v = "Intro. Hello world. ";
        let p = plan(&cmd("scratch that"), "scratch that", Some("Hello world. "), &field(v), true, false).unwrap();
        assert_eq!(p.plan, Plan::Replace { loc: 7, len: 13, text: String::new(), caret: 7 });
        // Merge delta with a leading space.
        let v = "Hello world and more";
        let p = plan(&cmd("scratch that"), "", Some(" and more"), &field(v), true, false).unwrap();
        assert_eq!(p.plan, Plan::Replace { loc: 11, len: 9, text: String::new(), caret: 11 });
    }

    #[test]
    fn scratch_declines_when_user_edited_the_paste() {
        let v = "Hello wurld. ";
        let e = plan(&cmd("scratch that"), "", Some("Hello world. "), &field(v), true, false);
        assert_eq!(e.err(), Some("anchor_not_in_field"));
    }

    #[test]
    fn scratch_keys_fallback_only_when_exact() {
        let f = Field { terminal: true, ..Field::default() };
        let p = plan(&cmd("scratch that"), "", Some("ls -la "), &f, true, false).unwrap();
        assert_eq!(p.plan, Plan::Keys(vec![key(keys::DELETE, Mods::NONE, 7)]));
        assert!(plan(&cmd("scratch that"), "", Some("ls -la "), &f, false, false).is_err());
    }

    #[test]
    fn replace_in_paste_keeps_case_and_learns() {
        let v = "I use cloud code daily. ";
        let u = "Change cloud code to Claude Code.";
        let p = plan(&cmd(u), u, Some(v), &field(v), true, false).unwrap();
        let Plan::Replace { loc, len, text, .. } = &p.plan else { panic!() };
        assert_eq!((*loc, *len), (0, len_u16(v)));
        assert_eq!(text, "I use Claude Code daily. ");
        assert_eq!(p.learn, Some(Learn { wrong: "cloud code".into(), right: "Claude Code".into() }));
        // Capitalised original → capitalised replacement; unrelated word
        // swap is not learned as a recognition error.
        let v = "Meet on Tuesday. ";
        let u = "replace tuesday with friday";
        let p = plan(&cmd(u), u, Some(v), &field(v), true, false).unwrap();
        let Plan::Replace { text, .. } = &p.plan else { panic!() };
        assert_eq!(text, "Meet on Friday. ");
        assert_eq!(p.learn, None);
    }

    #[test]
    fn ambiguous_replace_uses_the_split_present() {
        let v = "I need to go. ";
        let u = "change to to two";
        let p = plan(&cmd(u), u, Some(v), &field(v), true, false).unwrap();
        let Plan::Replace { text, .. } = &p.plan else { panic!() };
        assert_eq!(text, "I need two go. ");
    }

    #[test]
    fn case_list_and_ranges() {
        assert_eq!(transform_case("the quick fox", &CaseKind::Capitalize), "The Quick Fox");
        assert_eq!(make_list("Buy milk. Call mom. Ship it."), "- Buy milk\n- Call mom\n- Ship it");
        assert_eq!(make_list("eggs, milk and bread"), "- Eggs\n- Milk\n- Bread");
        let v = "One. Two three. ";
        assert_eq!(last_sentence_range(v, len_u16(v), false), Some((5, 15)));
        assert_eq!(last_word_range(v, len_u16(v), true), Some((9, 16)));
        assert_eq!(find_word_ci("Cat catalog cat.", "cat"), Some((12, 15)));
        assert_eq!(adapt_case("NASA", "nasa"), "NASA");
    }

    #[test]
    fn newline_prefers_ax_and_falls_back_to_shift_return() {
        let p = plan(&cmd("new line"), "", Some("x "), &field("x "), true, false).unwrap();
        assert_eq!(p.plan, Plan::Replace { loc: 2, len: 0, text: "\n".into(), caret: 3 });
        let p = plan(&cmd("new paragraph"), "", Some("x "), &Field::default(), true, false).unwrap();
        assert_eq!(p.plan, Plan::Keys(vec![key(keys::RETURN, Mods::SHIFT, 2)]));
    }
}
