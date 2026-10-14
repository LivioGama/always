//! Segment-level "My Voice": trim foreign speech off the edges of a
//! VERIFIED utterance, and a short-lived cohort of recent impostors.
//!
//! The whole-utterance gate answers "is the user speaking?". It cannot
//! answer "which PART of this recording is the user?" — and that is how
//! phone audio leaked (2026-10-14 14:56–14:57Z): an Instagram reel played
//! from the user's phone (an external source, so the echo canceller has
//! no reference for it) was correctly rejected on its own (0.05–0.24),
//! but as soon as the user spoke over it the utterance verified and EVERY
//! word in the buffer was transcribed — the reel's lead-in ("making these
//! videos for. …") pasted in front of his own words, and the reel's
//! continuation kept the VAD open after he stopped.
//!
//! A fixed per-window threshold cannot cut that audio safely: on the
//! owner's own recordings mixed with phone-filtered human speech, windows
//! holding >= 70 % of HIS speech still scored as low as 0.05–0.07 against
//! the profile (p01 0.08–0.25), so "score < t ⇒ foreign" cuts his words.
//! The cohort is what separates them: the reel that was just rejected IS
//! the voice now under his speech. A window is FOREIGN only when it is
//! far from the user's profile AND close to that recent impostor:
//! `score < 0.30 && cohort >= 0.60 && cohort - score >= 0.30`.
//!
//! Only the leading run before the first non-foreign window and the
//! trailing run after the last one are ever cut (each needs >= 2 foreign
//! windows), with a 450 ms guard kept next to the user's audio, and any
//! window that is not foreign — including every window the gate passed —
//! is kept whole. Measured offline (159 mixed utterances: 53 own-voice
//! recordings × reel at 12 / 6 / 0 dB below his voice, 18 phone-filtered
//! voices): 0 ms of his audible speech trimmed; reel speech trimmed
//! 28–52 % of the lead-in and 44–56 % of the tail. Inner runs (reel
//! between two of his phrases) are kept — no safe rule was found for them.
//!
//! When unsure, keep: no cohort ⇒ nothing is foreign ⇒ nothing is cut.

use std::collections::VecDeque;
use std::ops::Range;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::always::speaker_embed::{EMBEDDING_DIM, cosine};

/// Length of the windows the timeline judges (the ladder's 1.5 s
/// trailing window). Shorter, still-growing early windows are noisier
/// and were not part of the measured rule — they are ignored.
pub const WINDOW_SAMPLES: usize = 24_000;
/// A foreign window is far from the user's profile …
pub const FOREIGN_MAX_SCORE: f32 = 0.30;
/// … close to a recently rejected impostor …
pub const FOREIGN_MIN_COHORT: f32 = 0.60;
/// … and closer to the impostor than to the user by this margin.
/// (cohort >= 0.55 already trimmed 0.57 s of his speech in the offline
/// set; 0.60 trimmed none — the rule sits on the safe side of that edge.)
pub const FOREIGN_MARGIN: f32 = 0.30;
/// Consecutive foreign windows needed before an edge is cut.
pub const MIN_FOREIGN_RUN: usize = 2;
/// Audio kept next to the first/last non-foreign window (450 ms).
pub const GUARD_SAMPLES: usize = 7_200;
/// Cuts shorter than this are not worth the risk (600 ms).
pub const MIN_CUT_SAMPLES: usize = 9_600;

/// One scored ladder window, in buffer coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowVerdict {
    pub start: usize,
    pub end: usize,
    /// Best similarity to the user's profile (enrolled styles + recent own).
    pub score: f32,
    /// Best similarity to the recent-impostor cohort; `None` = no cohort.
    pub cohort: Option<f32>,
}

impl WindowVerdict {
    pub fn is_foreign(&self) -> bool {
        self.cohort.is_some_and(|c| {
            self.score < FOREIGN_MAX_SCORE
                && c >= FOREIGN_MIN_COHORT
                && c - self.score >= FOREIGN_MARGIN
        })
    }
}

/// What to cut from a buffer: `[0, lead)` and `[trail_from, len)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrimPlan {
    pub lead: usize,
    pub trail_from: Option<usize>,
}

impl TrimPlan {
    pub fn is_empty(&self) -> bool {
        self.lead == 0 && self.trail_from.is_none()
    }

    /// The kept range of a `len`-sample buffer.
    pub fn keep(&self, len: usize) -> Range<usize> {
        let end = self.trail_from.unwrap_or(len).min(len);
        self.lead.min(end)..end
    }

    /// Apply in place. Returns (lead samples removed, trail samples removed).
    pub fn apply(&self, buf: &mut Vec<i16>) -> (usize, usize) {
        let keep = self.keep(buf.len());
        let trail = buf.len() - keep.end;
        buf.truncate(keep.end);
        buf.drain(..keep.start);
        (keep.start, trail)
    }

    /// A trimmed copy (speculation snapshot).
    pub fn applied(&self, buf: &[i16]) -> Vec<i16> {
        buf[self.keep(buf.len())].to_vec()
    }
}

fn judged(windows: &[WindowVerdict], len: usize) -> Vec<&WindowVerdict> {
    let mut ws: Vec<&WindowVerdict> = windows
        .iter()
        .filter(|w| w.end <= len && w.end - w.start >= WINDOW_SAMPLES)
        .collect();
    ws.sort_by_key(|w| w.end);
    ws
}

/// Foreign windows after the last non-foreign one (all of them when
/// none is non-foreign).
pub fn trailing_foreign(windows: &[WindowVerdict], len: usize) -> usize {
    judged(windows, len)
        .iter()
        .rev()
        .take_while(|w| w.is_foreign())
        .count()
}

/// Plan the edge trim of a `len`-sample buffer from its scored windows.
/// `user_before`: the previous buffer of this utterance (before a chunk
/// flush) ended on the user's voice — so a buffer holding ONLY foreign
/// windows is the media continuing after he stopped, not his speech.
pub fn plan(windows: &[WindowVerdict], len: usize, user_before: bool) -> TrimPlan {
    let ws = judged(windows, len);
    let non_foreign: Vec<usize> = (0..ws.len()).filter(|&i| !ws[i].is_foreign()).collect();
    if non_foreign.is_empty() && user_before && ws.len() >= MIN_FOREIGN_RUN {
        // His last words sit at the end of the committed chunk: keep the
        // guard at the start of this buffer, cut the media after it.
        let cut = GUARD_SAMPLES;
        return TrimPlan {
            lead: 0,
            trail_from: (len.saturating_sub(cut) >= MIN_CUT_SAMPLES).then_some(cut),
        };
    }
    // No window looks like the user at all: the gate verified this
    // utterance on other evidence (whole-utterance score) — never cut it.
    let (Some(&first), Some(&last)) = (non_foreign.first(), non_foreign.last()) else {
        return TrimPlan::default();
    };
    let mut out = TrimPlan::default();
    if first >= MIN_FOREIGN_RUN {
        let cut = ws[first].start.saturating_sub(GUARD_SAMPLES);
        if cut >= MIN_CUT_SAMPLES {
            out.lead = cut;
        }
    }
    if ws.len() - 1 - last >= MIN_FOREIGN_RUN {
        let cut = ws[last].end + GUARD_SAMPLES;
        if len.saturating_sub(cut) >= MIN_CUT_SAMPLES {
            out.trail_from = Some(cut);
        }
    }
    out
}

/// Scored windows of the CURRENT buffer. Positions are only meaningful
/// for one buffer generation (`epoch` = committed samples): a chunk flush
/// starts a new generation and the old windows are dropped.
#[derive(Debug, Default)]
pub struct Timeline {
    epoch: usize,
    windows: Vec<WindowVerdict>,
    /// The previous buffer generation's last judged window was the user.
    user_before: bool,
}

impl Timeline {
    pub fn push(&mut self, epoch: usize, w: WindowVerdict) {
        if epoch != self.epoch {
            self.epoch = epoch;
            if let Some(last) = judged(&self.windows, usize::MAX).last() {
                self.user_before = !last.is_foreign();
            }
            self.windows.clear();
        }
        self.windows.push(w);
    }

    pub fn plan(&self, epoch: usize, len: usize) -> TrimPlan {
        if epoch != self.epoch {
            return TrimPlan::default();
        }
        plan(&self.windows, len, self.user_before)
    }

    pub fn trailing_foreign(&self, epoch: usize, len: usize) -> usize {
        if epoch != self.epoch {
            return 0;
        }
        trailing_foreign(&self.windows, len)
    }

    /// The buffer lost its first `n` samples: shift, dropping windows
    /// that started inside the removed audio.
    pub fn drain_front(&mut self, n: usize) {
        self.windows.retain(|w| w.start >= n);
        for w in &mut self.windows {
            w.start -= n;
            w.end -= n;
        }
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// Compact per-utterance log line: `end_ms:score:cohort,…`
    /// (`-` = no cohort). Scores only — never embeddings or audio.
    pub fn summary(&self) -> String {
        self.windows
            .iter()
            .map(|w| match w.cohort {
                Some(c) => format!("{}:{:.2}:{:.2}", w.end / 16, w.score, c),
                None => format!("{}:{:.2}:-", w.end / 16, w.score),
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

// ---------------------------------------------------------------------
// Recent-impostor cohort
// ---------------------------------------------------------------------

/// Only clearly-not-the-user rejects become cohort members (reel-only
/// utterances scored 0.05–0.24 on 2026-10-14).
pub const COHORT_ADMIT_MAX_SCORE: f32 = 0.30;
/// A candidate this close to the user's own recent accepted audio is
/// never admitted, whatever its profile score (poisoning guard).
pub const COHORT_OWN_MAX: f32 = 0.30;
/// Media keeps playing for minutes; a different video later is a
/// different voice and simply never matches.
pub const COHORT_TTL: Duration = Duration::from_secs(180);
pub const COHORT_MAX: usize = 8;

/// A passing WINDOW verdict this close to the threshold …
pub const VETO_BAND: f32 = 0.10;
/// … that is at least this much closer to the impostor is not a pass.
/// (On 747 passing windows of the user's own speech mixed with phone
/// voices, the cohort never came within 0.08 of the profile score.)
pub const VETO_MARGIN: f32 = 0.05;

static COHORT: Mutex<VecDeque<(Instant, Vec<f32>)>> = Mutex::new(VecDeque::new());

fn admit(
    cohort: &mut VecDeque<(Instant, Vec<f32>)>,
    now: Instant,
    embedding: Vec<f32>,
    score: f32,
    own_recent: &[Vec<f32>],
) -> bool {
    if embedding.len() != EMBEDDING_DIM || score >= COHORT_ADMIT_MAX_SCORE {
        return false;
    }
    let own = own_recent
        .iter()
        .filter(|r| r.len() == embedding.len())
        .map(|r| cosine(&embedding, r))
        .fold(f32::MIN, f32::max);
    if own >= COHORT_OWN_MAX {
        return false;
    }
    prune(cohort, now);
    cohort.push_back((now, embedding));
    while cohort.len() > COHORT_MAX {
        cohort.pop_front();
    }
    true
}

fn prune(cohort: &mut VecDeque<(Instant, Vec<f32>)>, now: Instant) {
    while cohort
        .front()
        .is_some_and(|(at, _)| now.saturating_duration_since(*at) > COHORT_TTL)
    {
        cohort.pop_front();
    }
}

fn similarity(cohort: &mut VecDeque<(Instant, Vec<f32>)>, now: Instant, e: &[f32]) -> Option<f32> {
    prune(cohort, now);
    cohort
        .iter()
        .filter(|(_, c)| c.len() == e.len())
        .map(|(_, c)| cosine(e, c))
        .reduce(f32::max)
}

/// Remember a rejected utterance's embedding as a recent impostor.
/// Returns whether it was admitted.
pub fn remember_foreign(embedding: Vec<f32>, score: f32) -> bool {
    let own = crate::always::voiceprint::recent_accepted();
    admit(&mut COHORT.lock(), Instant::now(), embedding, score, &own)
}

/// Best similarity of `e` to the live cohort; `None` when it is empty.
pub fn cohort_similarity(e: &[f32]) -> Option<f32> {
    similarity(&mut COHORT.lock(), Instant::now(), e)
}

pub fn clear_cohort() {
    COHORT.lock().clear();
}

/// A window that clears the gate threshold only barely, while sounding
/// more like the impostor just rejected, is not a verification.
pub fn cohort_vetoes_pass(score: f32, cohort: Option<f32>, threshold: f32) -> bool {
    cohort.is_some_and(|c| score < threshold + VETO_BAND && c >= score + VETO_MARGIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = WINDOW_SAMPLES;
    const STEP: usize = 8_000;

    fn win(end: usize, score: f32, cohort: Option<f32>) -> WindowVerdict {
        WindowVerdict {
            start: end - W,
            end,
            score,
            cohort,
        }
    }
    /// Windows every 0.5 s; `kind(i)` → (score, cohort).
    fn windows(n: usize, kind: impl Fn(usize) -> (f32, Option<f32>)) -> Vec<WindowVerdict> {
        (0..n)
            .map(|i| {
                let (s, c) = kind(i);
                win(W + i * STEP, s, c)
            })
            .collect()
    }
    const REEL: (f32, Option<f32>) = (0.08, Some(0.72));
    const USER: (f32, Option<f32>) = (0.55, Some(0.05));

    #[test]
    fn foreign_needs_low_score_high_cohort_and_margin() {
        assert!(win(W, 0.08, Some(0.72)).is_foreign());
        assert!(!win(W, 0.08, None).is_foreign(), "no cohort: never foreign");
        assert!(
            !win(W, 0.31, Some(0.95)).is_foreign(),
            "user-ish score is kept"
        );
        assert!(!win(W, 0.10, Some(0.59)).is_foreign(), "cohort too weak");
        assert!(!win(W, 0.29, Some(0.58)).is_foreign(), "margin too small");
        // Mostly-user window that scored low against the profile (seen
        // offline at 0.05) but is NOT near the impostor: kept.
        assert!(!win(W, 0.05, Some(0.20)).is_foreign());
    }

    #[test]
    fn reel_lead_in_and_tail_are_cut_with_guard() {
        // 6 reel windows, 8 user windows, 6 reel windows.
        let ws = windows(20, |i| if (6..14).contains(&i) { USER } else { REEL });
        let len = ws.last().unwrap().end + 4_000;
        let p = plan(&ws, len, false);
        let first_user = ws[6];
        let last_user = ws[13];
        assert_eq!(p.lead, first_user.start - GUARD_SAMPLES);
        assert_eq!(p.trail_from, Some(last_user.end + GUARD_SAMPLES));
        // Every window that is not foreign is kept whole.
        let keep = p.keep(len);
        for w in ws.iter().filter(|w| !w.is_foreign()) {
            assert!(
                keep.start <= w.start && w.end <= keep.end,
                "{w:?} cut by {keep:?}"
            );
        }
    }

    #[test]
    fn nothing_is_cut_without_a_cohort() {
        // The same audio scored with no recent impostor on record.
        let ws = windows(20, |i| {
            if (6..14).contains(&i) {
                USER
            } else {
                (0.08, None)
            }
        });
        assert!(plan(&ws, ws.last().unwrap().end, false).is_empty());
    }

    #[test]
    fn single_foreign_window_is_not_enough() {
        let ws = windows(10, |i| if i == 0 || i == 9 { REEL } else { USER });
        assert!(plan(&ws, ws.last().unwrap().end + 32_000, false).is_empty());
    }

    #[test]
    fn all_foreign_utterance_is_never_emptied() {
        // Verified on whole-utterance evidence while every window looked
        // foreign: keep everything rather than paste nothing.
        let ws = windows(12, |_| REEL);
        assert!(plan(&ws, ws.last().unwrap().end, false).is_empty());
    }

    #[test]
    fn trailing_run_counts_foreign_after_the_user() {
        let ws = windows(10, |i| if i < 7 { USER } else { REEL });
        assert_eq!(trailing_foreign(&ws, ws.last().unwrap().end), 3);
        let ws = windows(4, |_| USER);
        assert_eq!(trailing_foreign(&ws, ws.last().unwrap().end), 0);
    }

    #[test]
    fn media_only_buffer_after_a_flush_is_cut_only_after_his_voice() {
        // Chunk flushed on his words, then the reel kept talking: the new
        // buffer holds only foreign windows.
        let mut t = Timeline::default();
        for w in windows(6, |_| USER) {
            t.push(0, w);
        }
        let reel = windows(4, |_| REEL);
        for w in &reel {
            t.push(96_000, *w);
        }
        let len = reel.last().unwrap().end;
        assert_eq!(t.plan(96_000, len).trail_from, Some(GUARD_SAMPLES));
        // Same media buffer when the previous chunk did NOT end on him:
        // nothing user-like anywhere → never cut.
        let mut t2 = Timeline::default();
        for w in windows(6, |_| REEL) {
            t2.push(0, w);
        }
        for w in &reel {
            t2.push(96_000, *w);
        }
        assert!(t2.plan(96_000, len).is_empty());
        // One foreign window is not enough.
        let mut t3 = Timeline::default();
        t3.push(0, win(W, 0.6, None));
        t3.push(96_000, reel[0]);
        assert!(t3.plan(96_000, len).is_empty());
    }

    #[test]
    fn short_cuts_are_skipped() {
        // Two reel windows then user: first user window starts at 1.0 s →
        // cut point 1.0 s - 0.45 s = 0.55 s < 0.6 s minimum.
        let ws = windows(8, |i| if i < 2 { REEL } else { USER });
        assert_eq!(plan(&ws, ws.last().unwrap().end, false).lead, 0);
    }

    #[test]
    fn short_and_unfinished_windows_are_ignored() {
        let mut ws = windows(10, |i| if (4..10).contains(&i) { USER } else { REEL });
        // A short early window that looks foreign does not count, and a
        // window ending past the buffer (truncated by a tail cut) neither.
        ws.push(WindowVerdict {
            start: 0,
            end: 8_000,
            score: 0.0,
            cohort: Some(0.9),
        });
        let len = ws[9].end;
        ws.push(win(len + 8_000, 0.0, Some(0.9)));
        let p = plan(&ws, len, false);
        assert_eq!(p.lead, ws[4].start - GUARD_SAMPLES);
        assert_eq!(p.trail_from, None);
    }

    #[test]
    fn apply_removes_exactly_the_planned_edges() {
        let mut buf: Vec<i16> = (0..100).collect();
        let p = TrimPlan {
            lead: 10,
            trail_from: Some(90),
        };
        assert_eq!(p.applied(&buf), (10..90).collect::<Vec<i16>>());
        assert_eq!(p.apply(&mut buf), (10, 10));
        assert_eq!(buf, (10..90).collect::<Vec<i16>>());
        let mut b2: Vec<i16> = (0..5).collect();
        TrimPlan::default().apply(&mut b2);
        assert_eq!(b2.len(), 5);
    }

    #[test]
    fn timeline_is_per_buffer_generation_and_shifts_on_drain() {
        let mut t = Timeline::default();
        t.push(0, win(W, 0.1, Some(0.8)));
        t.push(0, win(W + STEP, 0.1, Some(0.8)));
        assert_eq!(t.len(), 2);
        assert!(
            t.plan(96_000, 50_000).is_empty(),
            "stale generation plans nothing"
        );
        t.drain_front(STEP);
        assert_eq!(t.len(), 1);
        assert_eq!(t.windows[0].start, W + STEP - W - STEP);
        t.push(96_000, win(W, 0.5, None));
        assert_eq!(t.len(), 1, "new generation drops old windows");
        assert_eq!(t.summary(), "1500:0.50:-");
    }

    fn unit(seed: f32) -> Vec<f32> {
        let mut v: Vec<f32> = (0..EMBEDDING_DIM)
            .map(|i| ((i as f32) * seed).sin())
            .collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= n);
        v
    }

    #[test]
    fn cohort_admission_guards_and_ttl() {
        let mut c = VecDeque::new();
        let t0 = Instant::now();
        let reel = unit(0.37);
        assert!(
            !admit(&mut c, t0, reel.clone(), 0.45, &[]),
            "borderline rejects stay out"
        );
        assert!(
            !admit(&mut c, t0, vec![0.0; 3], 0.1, &[]),
            "wrong dimension"
        );
        // Poisoning guard: a 'reject' that matches the user's own recent
        // audio is never an impostor.
        assert!(!admit(
            &mut c,
            t0,
            reel.clone(),
            0.1,
            std::slice::from_ref(&reel)
        ));
        assert!(admit(&mut c, t0, reel.clone(), 0.1, &[unit(1.91)]));
        assert!(similarity(&mut c, t0, &reel).unwrap() > 0.99);
        assert_eq!(
            similarity(&mut c, t0 + COHORT_TTL + Duration::from_secs(1), &reel),
            None
        );
        for i in 0..(COHORT_MAX + 3) {
            admit(&mut c, t0, unit(0.1 + i as f32), 0.1, &[]);
        }
        assert_eq!(c.len(), COHORT_MAX);
    }

    #[test]
    fn veto_only_touches_borderline_passes_near_the_impostor() {
        // 2026-10-14 14:57:20Z shape: 0.559 pass while a reel was being rejected.
        assert!(cohort_vetoes_pass(0.559, Some(0.70), 0.5));
        assert!(!cohort_vetoes_pass(0.559, None, 0.5), "no cohort: no veto");
        assert!(
            !cohort_vetoes_pass(0.559, Some(0.45), 0.5),
            "user-like window passes"
        );
        assert!(
            !cohort_vetoes_pass(0.65, Some(0.80), 0.5),
            "clear pass is never vetoed"
        );
    }
}
