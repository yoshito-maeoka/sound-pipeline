//! Private helper: fractional delay line (linear interpolation).

pub struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    /// Line able to delay by up to `max_delay` samples (reads of `d` in `1.0..=max_delay`).
    pub fn new(max_delay: usize) -> Self {
        Self { buf: vec![0.0; max_delay + 2], pos: 0 }
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }

    pub fn max_delay(&self) -> f32 {
        (self.buf.len() - 2) as f32
    }

    /// Sample pushed `d` pushes ago (`d >= 1`; read before `push` of the current
    /// input to get an exact `d`-sample delay). Linear interpolation.
    #[inline]
    pub fn read(&self, d: f32) -> f32 {
        let d = d.clamp(1.0, self.max_delay());
        let di = d as usize;
        let frac = d - di as f32;
        let n = self.buf.len();
        let a = self.buf[(self.pos + n - di) % n];
        if frac == 0.0 {
            return a;
        }
        let b = self.buf[(self.pos + n - di - 1) % n];
        a + (b - a) * frac
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
    }
}

/// One-pole smoothing coefficient for a time constant in seconds.
pub fn smooth_coef(sample_rate: f32, seconds: f32) -> f32 {
    1.0 - (-1.0 / (seconds * sample_rate).max(1.0)).exp()
}
