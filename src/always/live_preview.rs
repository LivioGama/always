//! Live preview for one utterance: merges two engines, gates on My Voice.
//!
//! - Parakeet (preview_asr.rs) answers first — ~100-200 ms word→screen —
//!   but is preview-only and never pasted.
//! - The Apple recognizer (the paste source) answers a little later and is
//!   more accurate. Wherever Apple already has text, Apple's words win;
//!   Parakeet only supplies the words beyond Apple's last one.
//!
//! Both engines PUSH into this object from their own callback threads.
//!
//! **My Voice (decision 2026-10-14).** Before the speaker is verified the
//! preview is still shown — immediately — but as
//! `TranscriptionInterimPending` (rendered dimmed by the GUI). That event
//! name is unknown to every other consumer, so unverified words never
//! reach Iris; only `TranscriptionInterim` (verified) does. On
//! verification the current text is re-sent at once as verified.

use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;

/// Where previews go. Production: the global broadcaster.
pub type Emit = Arc<dyn Fn(&str, bool) + Send + Sync>;

struct State {
    /// Text of already-committed chunks (both engines restart per chunk).
    prefix: String,
    parakeet: String,
    apple: String,
    verified: bool,
    closed: bool,
    last_sent: String,
    last_sent_verified: bool,
    first_sent_logged: bool,
}

pub struct LivePreview {
    state: Mutex<State>,
    emit: Emit,
    onset: Instant,
}

/// Normalise a word for alignment: lowercase, alphanumerics only.
fn norm(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Apple text, extended by Parakeet's words past the point Apple reached.
///
/// Alignment anchors on Apple's last word (or last two words) inside
/// Parakeet's word list near Apple's word count. Parakeet regularly drops
/// the first word and makes small substitutions, so a pure word-count
/// split would lose or duplicate words; when no anchor is found, the
/// count is the fallback.
pub fn merge(apple: &str, parakeet: &str) -> String {
    let apple = apple.trim();
    let parakeet = parakeet.trim();
    if apple.is_empty() {
        return parakeet.to_string();
    }
    if parakeet.is_empty() {
        return apple.to_string();
    }
    let a: Vec<&str> = apple.split_whitespace().collect();
    let p: Vec<&str> = parakeet.split_whitespace().collect();
    let pn: Vec<String> = p.iter().map(|w| norm(w)).collect();
    let a_last = norm(a[a.len() - 1]);
    let a_prev = (a.len() >= 2).then(|| norm(a[a.len() - 2]));
    let lo = a.len().saturating_sub(4);
    let hi = (a.len() + 3).min(p.len());
    // Best anchor: rightmost index in the window whose word matches Apple's
    // last word, preferring a two-word match.
    let mut cut: Option<usize> = None;
    for strict in [true, false] {
        for i in (lo..hi).rev() {
            if pn[i] != a_last || a_last.is_empty() {
                continue;
            }
            let two = match (&a_prev, i.checked_sub(1)) {
                (Some(prev), Some(j)) => pn[j] == *prev,
                _ => false,
            };
            if !strict || two {
                cut = Some(i + 1);
                break;
            }
        }
        if cut.is_some() {
            break;
        }
    }
    // No anchor (Parakeet substituted Apple's last word): align by
    // spoken length instead — walk Parakeet's words until they cover as
    // many letters as Apple's text ("show every word" ≈ "show everywhere").
    let cut = cut.unwrap_or_else(|| {
        let target: usize = a.iter().map(|w| norm(w).chars().count()).sum();
        let mut acc = 0usize;
        for (i, w) in pn.iter().enumerate() {
            acc += w.chars().count();
            if acc >= target {
                return i + 1;
            }
        }
        p.len()
    });
    if cut >= p.len() {
        return apple.to_string();
    }
    format!("{} {}", apple, p[cut..].join(" "))
}

impl LivePreview {
    pub fn new(verified: bool, emit: Emit) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                prefix: String::new(),
                parakeet: String::new(),
                apple: String::new(),
                verified,
                closed: false,
                last_sent: String::new(),
                last_sent_verified: false,
                first_sent_logged: false,
            }),
            emit,
            onset: Instant::now(),
        })
    }

    /// Production emitter: verified → `TranscriptionInterim`,
    /// unverified → `TranscriptionInterimPending`.
    pub fn broadcaster() -> Emit {
        Arc::new(|text: &str, verified: bool| {
            let b = crate::always::event::global_broadcaster();
            if verified {
                b.transcription_interim(text.to_string());
            } else {
                b.transcription_interim_pending(text.to_string());
            }
        })
    }

    fn publish(&self, st: &mut State, source: &'static str) {
        if st.closed {
            return;
        }
        let tail = merge(&st.apple, &st.parakeet);
        let text = match (st.prefix.is_empty(), tail.is_empty()) {
            (true, _) => tail,
            (false, true) => st.prefix.clone(),
            (false, false) => format!("{} {}", st.prefix, tail),
        };
        if text.is_empty() || (text == st.last_sent && st.verified == st.last_sent_verified) {
            return;
        }
        if !st.first_sent_logged {
            st.first_sent_logged = true;
            tracing::info!(
                since_onset_ms = self.onset.elapsed().as_millis() as u64,
                source,
                verified = st.verified,
                "live_preview_first_text"
            );
        }
        st.last_sent = text.clone();
        st.last_sent_verified = st.verified;
        (self.emit)(&text, st.verified);
    }

    pub fn on_parakeet(&self, text: &str) {
        let mut st = self.state.lock();
        st.parakeet = text.trim().to_string();
        self.publish(&mut st, "parakeet");
    }

    pub fn on_apple(&self, text: &str) {
        let mut st = self.state.lock();
        st.apple = text.trim().to_string();
        self.publish(&mut st, "apple");
    }

    /// Speaker verified: from now on previews are routable; re-send the
    /// current text as verified straight away.
    pub fn set_verified(&self) {
        let mut st = self.state.lock();
        if st.verified {
            return;
        }
        st.verified = true;
        self.publish(&mut st, "verified");
    }

    pub fn is_verified(&self) -> bool {
        self.state.lock().verified
    }

    /// What the Parakeet engine has heard so far in the open chunk
    /// (empty when it heard nothing). Read once at end of capture by the
    /// foreign-speech gate.
    pub fn parakeet_text(&self) -> String {
        self.state.lock().parakeet.clone()
    }

    /// The merged live text right now (Apple words, extended by Parakeet),
    /// without the committed-chunk prefix. Read by the end-of-utterance
    /// decision to hear a trailing "uh" / "and" / "the" at a pause before
    /// any final transcript exists.
    pub fn open_text(&self) -> String {
        let st = self.state.lock();
        merge(&st.apple, &st.parakeet)
    }

    /// A chunk was committed and both engines restart on the next audio:
    /// freeze what is shown as the prefix, so the new sessions' text is
    /// appended after it rather than replacing it.
    pub fn commit_chunk(&self) {
        let mut st = self.state.lock();
        let tail = merge(&st.apple, &st.parakeet);
        if !tail.is_empty() {
            st.prefix = if st.prefix.is_empty() { tail } else { format!("{} {}", st.prefix, tail) };
        }
        st.apple.clear();
        st.parakeet.clear();
    }

    /// Utterance over: nothing more may be shown from either engine.
    pub fn close(&self) {
        self.state.lock().closed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder() -> (Emit, Arc<Mutex<Vec<(String, bool)>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = Arc::clone(&log);
        (
            Arc::new(move |t: &str, v: bool| l.lock().push((t.to_string(), v))),
            log,
        )
    }

    #[test]
    fn merge_prefers_apple_and_appends_parakeet_tail() {
        assert_eq!(merge("", "hello there"), "hello there");
        assert_eq!(merge("Hello there", ""), "Hello there");
        // Parakeet dropped the first word: anchor on "is", keep "a test".
        assert_eq!(
            merge("Hello, world, this is", "world this is a test"),
            "Hello, world, this is a test"
        );
        // Apple ahead of Parakeet: Apple alone.
        assert_eq!(merge("Open the notch settings", "open the"), "Open the notch settings");
        // Substitution inside Apple's span is overridden by Apple.
        assert_eq!(
            merge("show every word", "show everywhere while i"),
            "show every word while i"
        );
        // Repeated word: anchor on the two-word match, not the first hit.
        assert_eq!(merge("the cat and the", "the cat and the dog ran"), "the cat and the dog ran");
    }

    #[test]
    fn unverified_preview_is_pending_until_verified_then_resent() {
        let (emit, log) = recorder();
        let p = LivePreview::new(false, emit);
        p.on_parakeet("hello");
        p.on_parakeet("hello there");
        p.set_verified();
        p.on_apple("Hello there,");
        assert_eq!(
            *log.lock(),
            vec![
                ("hello".into(), false),
                ("hello there".into(), false),
                ("hello there".into(), true),
                ("Hello there,".into(), true),
            ]
        );
    }

    #[test]
    fn chunk_commit_keeps_prefix_and_appends_new_sessions() {
        let (emit, log) = recorder();
        let p = LivePreview::new(true, emit);
        p.on_apple("First part.");
        p.commit_chunk();
        p.on_parakeet("second");
        p.on_apple("Second part");
        let got: Vec<String> = log.lock().iter().map(|(t, _)| t.clone()).collect();
        assert_eq!(got, vec!["First part.", "First part. second", "First part. Second part"]);
    }

    #[test]
    fn closed_preview_emits_nothing_and_duplicates_are_suppressed() {
        let (emit, log) = recorder();
        let p = LivePreview::new(true, emit);
        p.on_parakeet("a b");
        p.on_parakeet("a b");
        p.close();
        p.on_parakeet("a b c");
        p.on_apple("A b c");
        assert_eq!(*log.lock(), vec![("a b".into(), true)]);
    }
}
