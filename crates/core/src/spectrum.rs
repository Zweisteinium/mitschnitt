//! Is a lossless file made from MP3/AAC? Echolot's spectrum check (`library/audio.py`), ported so both judge a
//! recording the same way. Lossy encoders cut the spectrum with a steep low-pass (MP3 128k ~16 kHz, 192k ~19 kHz,
//! V0/256k ~19.5-20 kHz); real masters roll off gradually up to ~22 kHz. Three 15 s excerpts are averaged into a power
//! spectrum and the steepest drop between 12 kHz and Nyquist is located. An edge from [`TRACES_FROM`] up also needs
//! the encoder's traces below it ([`flicker`]): a steep mastering filter cuts there as well.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use crate::audio::Audio;

/// dB fall within ~1 kHz that counts as an encoder low-pass.
const DROP_DB: f64 = 30.0;
/// Edges above this are ordinary anti-alias filters of genuine masters.
const MAX_LOSSY_CUT: usize = 20_400;
const TRACES_FROM: usize = 19_000;
/// Genuine files with steep mastering filters (19.5 and 20 kHz) measured under 0.027.
const FLICKER: f64 = 0.03;

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub cut_hz: usize,
    pub drop_db: f64,
    /// The likely source when lossy, e.g. "~256 kbps / V0".
    pub lossy: Option<&'static str>,
}

/// `None`: too short (under 20 s) or too quiet to tell.
pub fn check(a: &Audio) -> Option<Edge> {
    let rate = a.rate as usize;
    let mono: Vec<f32> = (0..a.frames()).map(|i| a.frame(i).iter().map(|s| *s as f32).sum::<f32>() / (a.channels as f32 * (1u32 << (a.bits - 1)) as f32)).collect();
    if mono.len() < 20 * rate {
        return None;
    }
    let pieces: Vec<&[f32]> = [0.3, 0.5, 0.7]
        .iter()
        .map(|pos| {
            let start = (mono.len() as f64 * pos) as usize;
            &mono[start..(start + 15 * rate).min(mono.len())]
        })
        .collect();

    let (nfft, hop) = (8192, 4096);
    let mut acc = vec![0.0f64; nfft / 2 + 1];
    let mut frames = 0;
    for x in &pieces {
        for i in (0..x.len().saturating_sub(nfft)).step_by(hop) {
            for (sum, p) in acc.iter_mut().zip(power(&x[i..i + nfft])) {
                *sum += p;
            }
            frames += 1;
        }
    }
    if frames < 20 {
        return None;
    }
    let bin_hz = a.rate as f64 / nfft as f64;
    let db: Vec<f64> = acc.iter().map(|p| 10.0 * (p / frames as f64 + 1e-20).log10()).collect();
    let band = |lo: f64, hi: f64| mean(db.iter().enumerate().filter(|(k, _)| (lo..hi).contains(&(*k as f64 * bin_hz))).map(|(_, d)| *d));
    if band(1_000.0, 8_000.0) < -90.0 {
        return None;
    }

    let (mut cut_hz, mut drop_db) = (0, 0.0);
    for cut in (12_000..(rate / 2 - 1_600).min(23_000)).step_by(100) {
        let c = cut as f64;
        let drop = band(c - 1_500.0, c - 300.0) - band(c + 300.0, c + 1_500.0);
        if drop > drop_db {
            (cut_hz, drop_db) = (cut, drop);
        }
    }
    let steep = drop_db >= DROP_DB && cut_hz < MAX_LOSSY_CUT;
    let filter = || cut_hz >= TRACES_FROM && flicker(&pieces, a.rate, cut_hz).is_some_and(|f| f <= FLICKER);
    let source = if cut_hz < 16_800 {
        "~128 kbps"
    } else if cut_hz < 19_300 {
        "~160-192 kbps"
    } else {
        "~256 kbps / V0"
    };
    let lossy = (steep && !filter()).then_some(source);
    Some(Edge { cut_hz, drop_db, lossy })
}

/// An encoder's traces below its edge: it gives the bits of the highest bands to some frames (~23 ms) and none to
/// others, so there a bin switches between content and a hole (25 dB under the band) from one frame to the next; a
/// tone or noise through a filter stays as it is. The share of such switches in the 2.2 kHz below the edge, over loud
/// frames whose band is well above 16-bit rounding; `None` when too few.
fn flicker(pieces: &[&[f32]], rate: u32, cut: usize) -> Option<f64> {
    let nfft = 1024;
    let bin_hz = rate as f64 / nfft as f64;
    let bins = |lo: f64, hi: f64| (0..=nfft / 2).filter(move |k| (lo..hi).contains(&(*k as f64 * bin_hz)));
    let band: Vec<usize> = bins(cut as f64 - 2_500.0, cut as f64 - 300.0).collect();
    let mid: Vec<usize> = bins(1_000.0, 8_000.0).collect();
    let floor = 10.0 * (nfft as f64 / 4.0 * 1e-10).log10(); // 16-bit rounding noise per bin, about
    let (mut flips, mut frames) = (0, 0);
    for x in pieces {
        if x.len() < nfft * 4 {
            continue;
        }
        let spectra: Vec<Vec<f64>> = (0..=x.len() - nfft).step_by(nfft / 2).map(|i| power(&x[i..i + nfft]).into_iter().map(|p| p + 1e-20).collect()).collect();
        let mid_db: Vec<f64> = spectra.iter().map(|p| 10.0 * mean(mid.iter().map(|k| p[*k])).log10()).collect();
        let threshold = percentile(&mid_db, 90.0) - 15.0;
        let holes: Vec<Vec<bool>> = spectra
            .iter()
            .zip(&mid_db)
            .filter_map(|(p, m)| {
                let level = mean(band.iter().map(|k| p[*k]));
                (*m > threshold && 10.0 * level.log10() > floor + 30.0).then(|| band.iter().map(|k| p[*k] < level * 10f64.powf(-2.5)).collect())
            })
            .collect();
        if holes.len() < 2 {
            continue;
        }
        flips += holes.windows(2).map(|w| w[0].iter().zip(&w[1]).filter(|(a, b)| a != b).count()).sum::<usize>();
        frames += holes.len() - 1;
    }
    (frames >= 30).then(|| (flips as f64 / (frames * band.len()) as f64 * 10_000.0).round() / 10_000.0)
}

/// Power spectrum of one Hann-windowed frame (numpy's symmetric `hanning`, `rfft`).
fn power(frame: &[f32]) -> Vec<f64> {
    let n = frame.len();
    let mut buf: Vec<Complex<f64>> =
        frame.iter().enumerate().map(|(i, x)| Complex::new(*x as f64 * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()), 0.0)).collect();
    thread_local!(static PLANNER: std::cell::RefCell<FftPlanner<f64>> = std::cell::RefCell::new(FftPlanner::new()));
    PLANNER.with(|p| p.borrow_mut().plan_fft_forward(n).process(&mut buf));
    buf[..=n / 2].iter().map(|c| c.norm_sqr()).collect()
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = values.fold((0.0, 0), |(s, n), v| (s + v, n + 1));
    sum / n.max(1) as f64
}

/// numpy's default (linear) percentile.
fn percentile(values: &[f64], q: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let pos = q / 100.0 * (sorted.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 24 s of white noise, as it is and cut off above `cut` Hz like an encoder's low-pass.
    fn noise(cut: Option<f64>) -> Audio {
        let n = 1 << 20;
        let mut seed = 1u32;
        let mut buf: Vec<Complex<f64>> = (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                Complex::new((seed >> 8) as f64 / (1 << 24) as f64 - 0.5, 0.0)
            })
            .collect();
        if let Some(cut) = cut {
            let mut planner = FftPlanner::new();
            planner.plan_fft_forward(n).process(&mut buf);
            let edge = (cut / 44_100.0 * n as f64) as usize;
            buf[edge..n - edge].fill(Complex::new(0.0, 0.0));
            planner.plan_fft_inverse(n).process(&mut buf);
            buf.iter_mut().for_each(|c| *c /= n as f64);
        }
        let samples = buf.iter().flat_map(|c| [(c.re * 4_000_000.0) as i32; 2]).collect();
        Audio { rate: 44_100, channels: 2, bits: 24, samples }
    }

    #[test]
    fn an_encoder_low_pass_is_found_and_full_bandwidth_is_not() {
        assert_eq!(check(&noise(Some(16_000.0))).and_then(|e| e.lossy), Some("~128 kbps"));
        assert_eq!(check(&noise(None)).and_then(|e| e.lossy), None);
    }
}
