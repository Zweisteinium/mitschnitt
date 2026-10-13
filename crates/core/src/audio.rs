/// Decoded PCM: interleaved integer samples at `bits` depth (e.g. 24-bit values in an `i32`).
#[derive(Debug, Clone)]
pub struct Audio {
    pub rate: u32,
    pub channels: usize,
    pub bits: u32,
    pub samples: Vec<i32>,
}

impl Audio {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    pub fn frame(&self, i: usize) -> &[i32] {
        &self.samples[i * self.channels..(i + 1) * self.channels]
    }

    fn full_scale(&self) -> f64 {
        (1u64 << (self.bits - 1)) as f64
    }

    /// Fraction of full scale.
    pub fn frame_peak(&self, i: usize) -> f64 {
        self.frame(i).iter().map(|s| (*s as i64).abs()).max().unwrap_or(0) as f64 / self.full_scale()
    }

    pub fn peak(&self) -> f64 {
        self.samples.iter().map(|s| (*s as i64).abs()).max().unwrap_or(0) as f64 / self.full_scale()
    }

    /// Could come from a 16-bit master.
    pub fn frame_on_grid16(&self, i: usize) -> bool {
        if self.bits <= 16 {
            return true;
        }
        let step = 1i32 << (self.bits - 16);
        self.frame(i).iter().all(|s| s % step == 0)
    }
}

/// Amplitude (fraction of full scale) -> dBFS.
pub fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}
