//! Forensic checks on a finished recording, using only the file itself.
//!
//! Learned the hard way (see the Python prototype):
//! - Windows' audio engine limits each stream near 0 dBFS *before* the loopback tap (ceiling ~-0.12 dBFS).
//!   A peak pile-up there is a hint, not proof - many masters are mastered to -0.1 dBFS. Proof is a 16-bit
//!   master that leaves the 16-bit grid exactly in its loud passages.
//! - A player underrun shows up as exact digital silence with loud audio on both sides.
//! - Sharp "jumps" are usually just kicks; real capture glitches sit on the 10 ms packet grid.

use crate::audio::{Audio, db};
use crate::timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Altered,
    Corrupt,
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub severity: Severity,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub verdict: Severity,
    pub master: Master,
    pub peak_dbfs: f64,
    pub duration: String,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Master {
    /// Every sample sits on the 16-bit grid: a 16-bit master, bit-exact.
    Bit16Exact,
    /// Mostly on the grid: a 16-bit master that was altered in places.
    Bit16Partly,
    /// More than 16 bits of content: a 24-bit master - or processed/lossy-decoded audio. Which one can't be
    /// proven from the file alone (see the lossy hint).
    Bit24,
}

const LIMITER_LO: f64 = 0.977_237_220_955_810_7; // -0.20 dBFS
const LIMITER_HI: f64 = 0.990_831_944_892_768_9; // -0.08 dBFS

pub fn analyze(a: &Audio) -> Report {
    analyze_with(a, false)
}

/// `exact_source`: the capture proved every sample arrived untouched (see `passive`), so whatever is in the
/// file - clipping included - is in the master itself.
pub fn analyze_with(a: &Audio, exact_source: bool) -> Report {
    let mut issues = Vec::new();
    let n = a.frames();
    let ts = |i: usize| timestamp(i, a.rate);

    if a.rate != 44_100 || a.channels != 2 {
        issues.push(issue(Severity::Corrupt, format!("format {} Hz / {} ch (expected 44100 Hz stereo)", a.rate, a.channels)));
    }
    let ms = n as f64 * 1000.0 / a.rate as f64;
    if (ms - ms.round()).abs() >= 0.012 {
        issues.push(issue(Severity::Corrupt, format!("length {} is not a whole-millisecond track length (stopped early?)", ts(n))));
    }

    // Dropouts: >= 1 ms exact digital silence (all channels) with audible signal right before and after.
    for (s, e) in runs(n, |i| a.frame(i).iter().all(|s| *s == 0)) {
        if e - s >= a.rate as usize / 1000 && s > 0 && e < n {
            let edge = a.rate as usize / 200;
            let pre = (s.saturating_sub(edge)..s).fold(0.0, |m: f64, i| m.max(a.frame_peak(i)));
            let post = (e..(e + edge).min(n)).fold(0.0, |m: f64, i| m.max(a.frame_peak(i)));
            if pre > 0.02 && post > 0.02 {
                let ms = (e - s) as f64 * 1000.0 / a.rate as f64;
                issues.push(issue(Severity::Corrupt, format!("dropout {} - {} ({ms:.1} ms of digital silence inside loud audio)", ts(s), ts(e))));
            }
        }
    }

    // Start: leading digital zeros are legitimate (masters may begin with silence, which a full recording keeps);
    // very quiet samples are real audio. Only a file without any audio is broken.
    match (0..n).find(|&i| a.frame(i).iter().any(|s| *s != 0)) {
        None => issues.push(issue(Severity::Corrupt, "no audio at all".into())),
        Some(i) if i > a.rate as usize / 1000 => {
            issues.push(issue(Severity::Info, format!("begins with {} of digital silence (part of the track)", ts(i))))
        }
        Some(_) => {}
    }

    // Lossy origin hint: perceptual codecs cut everything above ~19-20 kHz, lossless masters rarely do.
    if let Some(ratio) = high_band_ratio(a) {
        if ratio < 1e-10 {
            issues.push(issue(Severity::Info, format!("no content above 20 kHz ({:.0} dB) - possibly from a lossy source", 10.0 * ratio.max(1e-30).log10())));
        }
    }

    // 16-bit grid map: where (if anywhere) is a mostly-16-bit file off the grid?
    let off: Vec<usize> = (0..n).filter(|&i| !a.frame_on_grid16(i)).collect();
    let on_fraction = 1.0 - off.len() as f64 / n.max(1) as f64;
    let master = if off.is_empty() {
        Master::Bit16Exact
    } else if on_fraction > 0.5 {
        Master::Bit16Partly
    } else {
        Master::Bit24
    };
    if master == Master::Bit16Partly {
        let stretches = group(&off, a.rate as usize / 20);
        // A fade starts at (or within a few samples of) the first sample: its very first values may be exact zeros.
        if stretches.len() == 1 && stretches[0].0 < a.rate as usize / 200 && stretches[0].1 < 2 * a.rate as usize {
            let ms = stretches[0].1 as f64 * 1000.0 / a.rate as f64;
            issues.push(issue(Severity::Altered, format!("fade-in at the start: first {ms:.1} ms not original (rest bit-exact 16-bit)")));
        } else {
            issues.push(issue(Severity::Altered, format!("not original in {} stretch(es) of an otherwise bit-exact 16-bit file: {}", stretches.len(), list(&stretches, a.rate, 8))));
        }
    }

    // Windows limiter signature: peak in the ceiling region and samples piling up just below it.
    let peak = a.peak();
    let near: Vec<usize> = (0..n).filter(|&i| a.frame_peak(i) >= LIMITER_LO).collect();
    if (LIMITER_LO..=LIMITER_HI).contains(&peak) && near.len() >= 20 {
        let passages = group(&near, a.rate as usize / 2);
        // With a 16-bit master that leaves the grid in exactly these passages it's proven; otherwise a hint.
        let proven = master == Master::Bit16Partly;
        issues.push(issue(
            if proven { Severity::Altered } else { Severity::Info },
            format!(
                "{}Windows peak limiter: {} samples under {:.2} dBFS in {} passage(s): {}",
                if proven { "" } else { "possible " },
                near.len(),
                db(peak),
                passages.len(),
                list(&passages, a.rate, 6)
            ),
        ));
    }

    // Clipping: >= 3 consecutive samples at exactly the file's peak (flat tops).
    if peak > 0.9 {
        let peak_int = a.samples.iter().map(|s| (*s as i64).abs()).max().unwrap_or(0);
        let mut flats = 0;
        let mut first = None;
        for c in 0..a.channels {
            for (s, e) in runs(n, |i| (a.samples[i * a.channels + c] as i64).abs() == peak_int) {
                if e - s >= 3 {
                    flats += 1;
                    first = Some(first.map_or(s, |f: usize| f.min(s)));
                }
            }
        }
        if let Some(f) = first {
            // A bit-exact or proven-untouched file can't have been clipped on the way: the master is clipped.
            if master == Master::Bit16Exact || exact_source {
                issues.push(issue(Severity::Info, format!("clipped in the master: {flats} flat tops at {:.2} dBFS, first at {} (file is exact)", db(peak), ts(f))));
            } else {
                issues.push(issue(Severity::Altered, format!("clipping: {flats} flat tops at {:.2} dBFS, first at {}", db(peak), ts(f))));
            }
        }
    }

    let verdict = issues.iter().map(|i| i.severity).filter(|s| *s > Severity::Info).max().unwrap_or(Severity::Info);
    Report { verdict, master, peak_dbfs: db(peak), duration: ts(n), issues }
}

fn issue(severity: Severity, text: String) -> Issue {
    Issue { severity, text }
}

/// Energy above 20 kHz relative to the total (mono sum), via a 4th-order Butterworth high-pass.
/// `None` when the rate has no room above 20 kHz or the file is (nearly) silent.
fn high_band_ratio(a: &Audio) -> Option<f64> {
    if a.rate < 44_100 {
        return None;
    }
    let scale = (1u64 << (a.bits - 1)) as f64 * a.channels as f64;
    let mut stages = [Biquad::highpass(20_000.0, a.rate as f64, 0.541_196), Biquad::highpass(20_000.0, a.rate as f64, 1.306_563)];
    let (mut total, mut high) = (0.0, 0.0);
    for i in 0..a.frames() {
        let x = a.frame(i).iter().map(|s| *s as f64).sum::<f64>() / scale;
        let y = stages.iter_mut().fold(x, |v, s| s.process(v));
        total += x * x;
        high += y * y;
    }
    (total > 1e-6 * a.frames() as f64).then(|| high / total)
}

/// RBJ biquad (direct form I).
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn highpass(freq: f64, rate: f64, q: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * freq / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = [(1.0 + cos) / 2.0 / a0, -(1.0 + cos) / a0, (1.0 + cos) / 2.0 / a0];
        Biquad { b, a: [-2.0 * cos / a0, (1.0 - alpha) / a0], x: [0.0; 2], y: [0.0; 2] }
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// Half-open runs [start, end) of frames where `pred` holds.
fn runs(n: usize, pred: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for i in 0..n {
        match (pred(i), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, n));
    }
    out
}

/// Group sorted indices into inclusive ranges, merging gaps up to `gap`.
fn group(idx: &[usize], gap: usize) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &i in idx {
        match out.last_mut() {
            Some((_, end)) if i - *end <= gap => *end = i,
            _ => out.push((i, i)),
        }
    }
    out
}

fn list(ranges: &[(usize, usize)], rate: u32, max: usize) -> String {
    let mut s = ranges.iter().take(max).map(|(a, b)| format!("{}-{}", timestamp(*a, rate), timestamp(*b, rate))).collect::<Vec<_>>().join(", ");
    if ranges.len() > max {
        s.push_str(" ...");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// Deterministic "music" from a 16-bit master, stored as 24-bit like a Mitschnitt recording.
    fn music(secs: f64, amp: f64) -> Audio {
        let n = (secs * RATE as f64) as usize;
        let mut samples = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f64 / RATE as f64;
            let x = amp * (0.6 * (2.0 * std::f64::consts::PI * 220.0 * t).sin() + 0.4 * (2.0 * std::f64::consts::PI * 331.0 * t).sin());
            let q = ((x * 32_768.0).round() as i32) << 8;
            samples.push(q);
            samples.push(q);
        }
        Audio { rate: RATE, channels: 2, bits: 24, samples }
    }

    fn scale(v: &mut i32, g: f64) {
        *v = (*v as f64 * g).round() as i32;
    }

    #[test]
    fn clean_16bit_master_is_clean() {
        let r = analyze(&music(2.0, 0.5));
        assert_eq!(r.master, Master::Bit16Exact);
        assert_eq!(r.verdict, Severity::Info, "{:?}", r.issues);
    }

    #[test]
    fn dropout_is_found_with_timestamps() {
        let mut a = music(3.0, 0.5);
        let (s, e) = (RATE as usize, RATE as usize + RATE as usize / 2); // 1.000 s - 1.500 s
        a.samples[s * 2..e * 2].fill(0);
        let r = analyze(&a);
        assert_eq!(r.verdict, Severity::Corrupt);
        assert!(r.issues.iter().any(|i| i.text.contains("dropout 0:01.000 - 0:01.500")), "{:?}", r.issues);
    }

    #[test]
    fn fade_in_at_start_is_found() {
        let mut a = music(2.0, 0.5);
        for i in 0..441 {
            // 10 ms linear fade-in: leaves the 16-bit grid
            let g = i as f64 / 441.0;
            scale(&mut a.samples[2 * i], g);
            scale(&mut a.samples[2 * i + 1], g);
        }
        let r = analyze(&a);
        assert!(r.issues.iter().any(|i| i.text.starts_with("fade-in at the start")), "{:?}", r.issues);
    }

    #[test]
    fn partial_processing_gets_timestamps() {
        let mut a = music(4.0, 0.5);
        for v in &mut a.samples[2 * RATE as usize * 2..(2 * RATE as usize + 4410) * 2] {
            scale(v, 0.9); // 2.000 s - 2.100 s attenuated: off the 16-bit grid
        }
        let r = analyze(&a);
        assert_eq!(r.master, Master::Bit16Partly);
        assert!(r.issues.iter().any(|i| i.text.contains("0:02.000-0:02.100")), "{:?}", r.issues);
    }

    #[test]
    fn clipping_in_a_bit_exact_master_is_not_an_alteration() {
        let mut a = music(1.0, 0.5);
        for v in &mut a.samples[2_000..2_010] {
            *v = 32_767 << 8; // flat top at full scale, still on the 16-bit grid
        }
        let r = analyze(&a);
        assert_eq!(r.master, Master::Bit16Exact);
        assert_eq!(r.verdict, Severity::Info, "{:?}", r.issues);
        assert!(r.issues.iter().any(|i| i.text.starts_with("clipped in the master")), "{:?}", r.issues);
    }

    #[test]
    fn early_stop_breaks_whole_millisecond_length() {
        let mut a = music(1.0, 0.5);
        a.samples.truncate((RATE as usize + 7) * 2 - 14 * 2); // 44093 frames = 999.84 ms
        let r = analyze(&a);
        assert!(r.issues.iter().any(|i| i.text.contains("whole-millisecond")), "{:?}", r.issues);
    }
}
