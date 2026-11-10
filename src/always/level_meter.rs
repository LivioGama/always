//! Voice level for the notch: RMS energy + a coarse 5-band spectrum at
//! ~20 Hz while an utterance is being captured (`DaemonEvent::VoiceLevel`).
//!
//! Cost per tick: one 256-point radix-2 FFT (~10 µs) over the newest
//! 16 ms of audio; per frame only a sum of squares is accumulated.

use std::time::{Duration, Instant};

/// Minimum gap between ticks (≈20 Hz). Frames are 30 ms, so ticks land
/// every 2nd frame (60 ms) unless frames arrive in a burst.
pub const TICK: Duration = Duration::from_millis(50);
const N: usize = 256;
/// Band edges in Hz for the 5 bands.
const EDGES: [f32; 6] = [80.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0];

pub struct LevelMeter {
    sum_sq: f64,
    count: usize,
    window: Vec<i16>,
    last_tick: Option<Instant>,
}

impl Default for LevelMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl LevelMeter {
    pub fn new() -> Self {
        Self {
            sum_sq: 0.0,
            count: 0,
            window: Vec::with_capacity(N),
            last_tick: None,
        }
    }

    /// Accumulate one frame; returns `(energy, bands)` when a tick is due.
    ///
    /// `now` is the frame's CAPTURE time, not the time it is processed.
    /// Audio is delivered in bursts, so several frames are routinely
    /// processed within microseconds of each other; throttling on
    /// processing time collapsed such a burst into one tick carrying its
    /// OLDEST frame, leaving the level shown up to a burst behind the
    /// voice. In capture time the ticks stay evenly spaced and the last
    /// one of a burst is the newest audio.
    pub fn push(&mut self, samples: &[i16], now: Instant) -> Option<(f64, [f32; 5])> {
        for &s in samples {
            let f = s as f64 / 32768.0;
            self.sum_sq += f * f;
        }
        self.count += samples.len();
        let take = samples.len().min(N);
        self.window
            .extend_from_slice(&samples[samples.len() - take..]);
        if self.window.len() > N {
            let excess = self.window.len() - N;
            self.window.drain(..excess);
        }
        // `now >= t`: a capture clock that steps backwards (recorder
        // respawn) must not stall the meter until it catches up.
        if self
            .last_tick
            .is_some_and(|t| now >= t && now.duration_since(t) < TICK)
            || self.count == 0
        {
            return None;
        }
        self.last_tick = Some(now);
        let energy = (self.sum_sq / self.count as f64).sqrt();
        self.sum_sq = 0.0;
        self.count = 0;
        Some((energy, bands(&self.window)))
    }
}

/// What the overlay is told about one in-utterance frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LevelEvent {
    /// A level tick for a voiced frame.
    Level(f64, [f32; 5]),
    /// The voice paused: sent ONCE on the first non-voiced frame after
    /// level ticks, so the level-driven indicator comes to rest on an
    /// explicit signal instead of holding its last value until the
    /// utterance is cut.
    Rest,
}

/// The speaking-level stream for one utterance: level ticks while the
/// (verified) user is voiced, and a single [`LevelEvent::Rest`] at each
/// pause.
#[derive(Default)]
pub struct LiveLevel {
    meter: Option<LevelMeter>,
    live: bool,
}

impl LiveLevel {
    /// `active` is "voiced frame of a verified speaker (or My Voice off)".
    /// Anything else never moves the level; `captured_at` as in
    /// [`LevelMeter::push`].
    pub fn frame(
        &mut self,
        samples: &[i16],
        active: bool,
        captured_at: Instant,
    ) -> Option<LevelEvent> {
        if !active {
            return std::mem::take(&mut self.live).then_some(LevelEvent::Rest);
        }
        let meter = self.meter.get_or_insert_with(LevelMeter::new);
        let (energy, bands) = meter.push(samples, captured_at)?;
        self.live = true;
        Some(LevelEvent::Level(energy, bands))
    }

    /// True once any voiced frame was metered (a terminal zero is owed).
    pub fn started(&self) -> bool {
        self.meter.is_some()
    }
}

/// Relative RMS share of each band (sums to ~1; all zero for silence).
pub fn bands(window: &[i16]) -> [f32; 5] {
    let mut re = [0f32; N];
    let mut im = [0f32; N];
    let n = window.len().min(N);
    for i in 0..n {
        // Hann window over the samples present.
        let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n.max(2) - 1) as f32).cos();
        re[i] = window[window.len() - n + i] as f32 / 32768.0 * w;
    }
    fft(&mut re, &mut im);
    let bin_hz = 16_000.0 / N as f32;
    let mut power = [0f32; 5];
    for k in 1..N / 2 {
        let hz = k as f32 * bin_hz;
        if let Some(b) = (0..5).find(|&b| hz >= EDGES[b] && hz < EDGES[b + 1]) {
            power[b] += re[k] * re[k] + im[k] * im[k];
        }
    }
    let amp: Vec<f32> = power.iter().map(|p| p.sqrt()).collect();
    let total: f32 = amp.iter().sum();
    let mut out = [0f32; 5];
    if total > 1e-9 {
        for b in 0..5 {
            out[b] = amp[b] / total;
        }
    }
    out
}

/// In-place iterative radix-2 FFT, N = 256.
fn fft(re: &mut [f32; N], im: &mut [f32; N]) {
    let mut j = 0;
    for i in 1..N {
        let mut bit = N >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= N {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        for start in (0..N).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f32).sin_cos();
                let a = start + k;
                let b = a + len / 2;
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amp: f32, n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| {
                ((2.0 * std::f32::consts::PI * hz * i as f32 / 16_000.0).sin() * amp * 32767.0)
                    as i16
            })
            .collect()
    }

    #[test]
    fn tone_lands_in_its_band() {
        for (hz, band) in [(150.0, 0), (400.0, 1), (700.0, 2), (1500.0, 3), (3000.0, 4)] {
            let b = bands(&tone(hz, 0.5, 256));
            let max = (0..5).max_by(|&x, &y| b[x].total_cmp(&b[y])).unwrap();
            assert_eq!(max, band, "{hz} Hz -> {b:?}");
        }
        assert_eq!(bands(&[0i16; 256]), [0.0; 5]);
    }

    #[test]
    fn ticks_at_most_every_50ms_with_rms_energy() {
        let mut m = LevelMeter::new();
        let t0 = Instant::now();
        let frame = tone(440.0, 0.5, 480);
        let first = m.push(&frame, t0).expect("first frame ticks");
        assert!(
            (first.0 - 0.5 / 2f64.sqrt()).abs() < 0.01,
            "rms {}",
            first.0
        );
        assert!(m.push(&frame, t0 + Duration::from_millis(30)).is_none());
        assert!(m.push(&frame, t0 + Duration::from_millis(60)).is_some());
    }

    /// A burst: six 30 ms frames handed over at once, each stamped with its
    /// own capture time. Ticks must stay evenly spaced in audio time (every
    /// 2nd frame) rather than collapsing into one, and the last tick must
    /// carry the newest audio — here the loud half of the burst.
    #[test]
    fn burst_ticks_follow_capture_time_and_end_on_newest_audio() {
        let mut m = LevelMeter::new();
        let t0 = Instant::now();
        let quiet = tone(440.0, 0.01, 480);
        let loud = tone(440.0, 0.5, 480);
        let mut ticks = Vec::new();
        for i in 0..6u64 {
            let frame = if i < 3 { &quiet } else { &loud };
            if let Some((energy, _)) = m.push(frame, t0 + Duration::from_millis(30 * i)) {
                ticks.push((i, energy));
            }
        }
        let at: Vec<u64> = ticks.iter().map(|t| t.0).collect();
        assert_eq!(at, vec![0, 2, 4], "one tick per 60 ms of audio");
        assert!(ticks[0].1 < 0.02, "first tick is the quiet start");
        assert!(ticks[2].1 > 0.3, "last tick is the loud newest audio");
    }

    #[test]
    fn capture_clock_stepping_back_does_not_stall_the_meter() {
        let mut m = LevelMeter::new();
        let t0 = Instant::now() + Duration::from_secs(10);
        let frame = tone(440.0, 0.5, 480);
        assert!(m.push(&frame, t0).is_some());
        assert!(m.push(&frame, t0 - Duration::from_secs(5)).is_some());
    }

    #[test]
    fn live_level_rests_once_per_pause_and_resumes() {
        let mut l = LiveLevel::default();
        let t0 = Instant::now();
        let at = |i: u64| t0 + Duration::from_millis(30 * i);
        let voice = tone(440.0, 0.5, 480);
        let hush = [0i16; 480];

        assert!(!l.started());
        assert!(matches!(
            l.frame(&voice, true, at(0)),
            Some(LevelEvent::Level(..))
        ));
        assert!(l.started());
        // Pause: exactly one Rest, on the first non-voiced frame.
        assert_eq!(l.frame(&hush, false, at(1)), Some(LevelEvent::Rest));
        assert_eq!(l.frame(&hush, false, at(2)), None);
        assert_eq!(l.frame(&hush, false, at(3)), None);
        // Speech resumes, then a second pause rests again.
        assert!(matches!(
            l.frame(&voice, true, at(4)),
            Some(LevelEvent::Level(..))
        ));
        assert_eq!(l.frame(&hush, false, at(5)), Some(LevelEvent::Rest));
    }

    /// My Voice: frames of a speaker who is not (yet) verified are passed
    /// as inactive. They must never produce a level — nor a Rest, which
    /// would be a signal about a voice the indicator never showed.
    #[test]
    fn live_level_is_silent_for_an_unverified_speaker() {
        let mut l = LiveLevel::default();
        let t0 = Instant::now();
        let voice = tone(440.0, 0.5, 480);
        for i in 0..10u64 {
            assert_eq!(
                l.frame(&voice, false, t0 + Duration::from_millis(30 * i)),
                None
            );
        }
        assert!(!l.started());
    }
}
