//! Forensic checks on a finished file. Measured: Windows limits each stream just below 0 dBFS before the loopback tap
//! (ceiling ~-0.12 dBFS), so a pile-up there is only a hint (masters are often limited to -0.1 dBFS); a player
//! underrun is exact digital silence between loud audio.

use crate::audio::{Audio, db};
use crate::timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Master {
    Bit16Exact,
    /// Mostly on the 16-bit grid: a 16-bit master altered in places.
    Bit16Partly,
    /// A 24-bit master, or processed audio: the file alone can't tell.
    Bit24,
}

const LIMITER_LO: f64 = 0.977_237_220_955_810_7; // -0.20 dBFS
const LIMITER_HI: f64 = 0.990_831_944_892_768_9; // -0.08 dBFS
/// Nothing above this came through Windows' limiter.
const WINDOWS_CEILING: f64 = 0.986_848_891_640_427_5; // -0.115 dBFS

pub fn analyze(a: &Audio) -> Report {
    analyze_with(a, false)
}

/// `exact_source`: the capture proved the audio untouched, so whatever the file shows is in the master itself.
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

    // Dropouts: >= 1 ms of digital silence between loud audio.
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

    match (0..n).find(|&i| a.frame(i).iter().any(|s| *s != 0)) {
        None => issues.push(issue(Severity::Corrupt, "no audio at all".into())),
        Some(i) if i > a.rate as usize / 1000 => {
            issues.push(issue(Severity::Info, format!("begins with {} of digital silence (part of the track)", ts(i))))
        }
        Some(_) => {}
    }

    // Lossy codecs cut everything above ~20 kHz.
    if !exact_source && let Some(ratio) = high_band_ratio(a) && ratio < 1e-10 {
        issues.push(issue(Severity::Info, format!("no content above 20 kHz ({:.0} dB) - possibly from a lossy source", 10.0 * ratio.max(1e-30).log10())));
    }

    let off: Vec<usize> = (0..n).filter(|&i| !a.frame_on_grid16(i)).collect();
    let on_fraction = 1.0 - off.len() as f64 / n.max(1) as f64;
    let master = if off.is_empty() {
        Master::Bit16Exact
    } else if on_fraction > 0.5 {
        Master::Bit16Partly
    } else {
        Master::Bit24
    };
    if master == Master::Bit16Partly && !exact_source {
        let stretches = group(&off, a.rate as usize / 20);
        let text = match stretches[..] {
            // Spotify fades a started track in over exactly 10 ms.
            [(_, last)] if last < a.rate as usize / 100 => format!("fade-in at the start: first {:.1} ms not original (rest bit-exact 16-bit)", (last + 1) as f64 * 1000.0 / a.rate as f64),
            _ => format!("not original in {} stretch(es) of an otherwise bit-exact 16-bit file: {}", stretches.len(), list(&stretches, a.rate, 8)),
        };
        issues.push(issue(Severity::Altered, text));
    }

    // Samples piling up just below the limiter's ceiling, none above it.
    let peak = a.peak();
    let near: Vec<usize> = (0..n).filter(|&i| a.frame_peak(i) >= LIMITER_LO).collect();
    let above_ceiling = near.iter().any(|&i| a.frame_peak(i) > WINDOWS_CEILING);
    let possible = master != Master::Bit16Exact && !exact_source && !above_ceiling;
    if possible && (LIMITER_LO..=LIMITER_HI).contains(&peak) && near.len() >= 20 {
        let passages = group(&near, a.rate as usize / 2);
        let proven = master == Master::Bit16Partly;
        issues.push(issue(
            if proven { Severity::Altered } else { Severity::Info },
            format!(
                "{}Windows peak limiter: {} samples under {:.2} dBFS in {} passage(s): {}",
                if proven { "" } else { "possible (or a master limited to that level) " },
                near.len(),
                db(peak),
                passages.len(),
                list(&passages, a.rate, 6)
            ),
        ));
    }

    // Clipping: >= 3 consecutive samples at exactly the peak.
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

/// Energy above 20 kHz relative to the total (4th-order Butterworth high-pass); `None` for (nearly) silent files.
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

/// Half-open runs of frames where `pred` holds.
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
    fn a_proven_untouched_capture_is_never_altered_by_the_file_check() {
        // Off the 16-bit grid in places and clipped: in the master, as the capture proved.
        let mut a = music(4.0, 0.5);
        for v in &mut a.samples[2 * RATE as usize * 2..(2 * RATE as usize + 4410) * 2] {
            scale(v, 0.9);
        }
        for i in 0..441 {
            scale(&mut a.samples[2 * i], i as f64 / 441.0);
        }
        for v in &mut a.samples[RATE as usize * 2..RATE as usize * 2 + 10] {
            *v = 32_767 << 8;
        }
        assert_eq!(analyze(&a).verdict, Severity::Altered);
        let r = analyze_with(&a, true);
        assert_eq!(r.verdict, Severity::Info, "{:?}", r.issues);
        assert!(r.issues.iter().any(|i| i.text.starts_with("clipped in the master")), "{:?}", r.issues);
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

    /// 24-bit "music" (off the 16-bit grid) with every sample above `ceiling` clipped to it.
    fn limited(ceiling_db: f64) -> Audio {
        let mut a = music(2.0, 1.2);
        let top = ((1i64 << 23) as f64 * 10f64.powf(ceiling_db / 20.0)) as i32;
        for v in &mut a.samples {
            *v = (*v + 1).clamp(-top, top);
        }
        a
    }

    #[test]
    fn master_limited_to_minus_0_1_db_is_not_windows() {
        let r = analyze(&limited(-0.10));
        assert_eq!(r.master, Master::Bit24);
        assert!(!r.issues.iter().any(|i| i.text.contains("limiter")), "{:?}", r.issues);
    }

    #[test]
    fn pile_up_at_the_windows_ceiling_is_hinted() {
        let r = analyze(&limited(-0.125));
        assert!(r.issues.iter().any(|i| i.text.contains("Windows peak limiter")), "{:?}", r.issues);
        assert!(!analyze_with(&limited(-0.125), true).issues.iter().any(|i| i.text.contains("limiter")), "exact source rules it out");
    }

    #[test]
    fn early_stop_breaks_whole_millisecond_length() {
        let mut a = music(1.0, 0.5);
        a.samples.truncate((RATE as usize + 7) * 2 - 14 * 2); // 44093 frames = 999.84 ms
        let r = analyze(&a);
        assert!(r.issues.iter().any(|i| i.text.contains("whole-millisecond")), "{:?}", r.issues);
    }
}
