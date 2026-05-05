//! BigCross — Bauer-style stereo crossfeed.
//!
//! When you listen to headphones, each ear gets ONE channel — pure L on
//! left, pure R on right. That's not how human hearing works in real
//! life: with speakers, both ears receive both channels, but the
//! "wrong-ear" channel arrives slightly later (the time it takes sound
//! to travel around your head, ~250-300 µs) and slightly muffled (the
//! head shadows high frequencies). The brain uses these delay+filter
//! cues to build a 3D image.
//!
//! BigCross feeds a delayed, low-passed copy of each channel into the
//! opposite ear. The result on headphones: the soundstage moves out of
//! the centre of your skull and forward, music feels like it's coming
//! from speakers in front of you instead of inside your head. Less
//! fatigue, more natural stereo image. This is the closest free /
//! algorithmic equivalent to what Dolby Atmos / Apple Spatial Audio
//! achieve via licensed binaural HRIR convolution.
//!
//! Reference: Benjamin Bauer (1961), "Stereophonic Earphones and
//! Binaural Loudspeakers". HeadRoom and the AMB β22 use similar topology.
//!
//! On speakers (not headphones), set `amount = 0` — your room already
//! does the crossfeed physically.

use big_bass::biquad::{Biquad, BiquadCoeffs};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrossfeedParams {
    /// 0 = bypass, 1 = full crossfeed (-9 dB cross signal level).
    /// Typical sweet spot on headphones: 0.5..=0.8.
    pub amount: f32,
    /// Cutoff (Hz) of the low-pass on the cross signal. Mimics head
    /// shadowing: above this frequency the wrong-ear channel arrives
    /// strongly attenuated. Range 400..=1500. Default 700.
    pub cutoff_hz: f32,
    /// Delay (microseconds) of the cross signal. Approximates the time
    /// for sound to travel around the listener's head. Range 100..=500.
    /// Default 280 (corresponds to ~10 cm path difference, ear-to-ear).
    pub delay_us: f32,
    /// Pass-through.
    pub bypass: bool,
}

impl Default for CrossfeedParams {
    fn default() -> Self {
        Self {
            amount: 0.0,
            cutoff_hz: 700.0,
            delay_us: 280.0,
            bypass: false,
        }
    }
}

/// Fixed-capacity ring buffer for sample-accurate delay. Heap-allocated
/// once at construction, then reused without further allocation in the
/// audio process loop.
struct RingDelay {
    buf: Vec<f32>,
    write: usize,
    delay_samples: usize,
}

impl RingDelay {
    fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0.0; capacity.max(1)],
            write: 0,
            delay_samples: 0,
        }
    }

    fn set_delay_samples(&mut self, n: usize) {
        // Max usable delay is buf.len() - 1: with N slots and a delay of N
        // the read index aliases the just-written sample (delay 0). Use
        // saturating_sub so a degenerate capacity-1 buffer yields max=0
        // (true passthrough) instead of the previous off-by-one where
        // n=1 was requested but process() still returned the live sample.
        let max = self.buf.len().saturating_sub(1);
        self.delay_samples = n.min(max);
    }

    fn reset(&mut self) {
        self.buf.fill(0.0);
        self.write = 0;
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        // Write current sample, read N samples ago.
        let cap = self.buf.len();
        self.buf[self.write] = x;
        let read = (self.write + cap - self.delay_samples) % cap;
        let out = self.buf[read];
        self.write = (self.write + 1) % cap;
        out
    }
}

pub struct CrossfeedProcessor {
    /// 4th-order Butterworth low-pass on each cross channel — cascade
    /// of two biquads at the same cutoff so attenuation is steep enough
    /// (~24 dB/oct) that the high frequencies stay properly separated.
    lp_l: [Biquad; 2],
    lp_r: [Biquad; 2],
    delay_l: RingDelay,
    delay_r: RingDelay,
    sample_rate: f32,
    params: CrossfeedParams,
}

impl CrossfeedProcessor {
    pub fn new(sample_rate: f32, params: CrossfeedParams) -> Self {
        // 1 ms of headroom is plenty (delay knob caps at 500 µs).
        let cap = ((sample_rate * 0.001) as usize).max(32);
        let mut s = Self {
            lp_l: [Biquad::default(); 2],
            lp_r: [Biquad::default(); 2],
            delay_l: RingDelay::new(cap),
            delay_r: RingDelay::new(cap),
            sample_rate,
            params,
        };
        s.update_coeffs();
        s
    }

    fn update_coeffs(&mut self) {
        let lp = BiquadCoeffs::lowpass(self.sample_rate, self.params.cutoff_hz, 0.707);
        for f in &mut self.lp_l {
            f.set_coeffs(lp);
        }
        for f in &mut self.lp_r {
            f.set_coeffs(lp);
        }
        let samples = ((self.params.delay_us / 1_000_000.0) * self.sample_rate).round() as usize;
        self.delay_l.set_delay_samples(samples);
        self.delay_r.set_delay_samples(samples);
    }

    pub fn set_params(&mut self, p: CrossfeedParams) {
        let coeffs_changed = (self.params.cutoff_hz - p.cutoff_hz).abs() > 0.001
            || (self.params.delay_us - p.delay_us).abs() > 0.001;
        self.params = p;
        if coeffs_changed {
            self.update_coeffs();
        }
    }

    pub fn reset(&mut self) {
        for f in &mut self.lp_l {
            f.reset();
        }
        for f in &mut self.lp_r {
            f.reset();
        }
        self.delay_l.reset();
        self.delay_r.reset();
    }

    #[inline]
    pub fn process_stereo(&mut self, l: f32, r: f32) -> (f32, f32) {
        if self.params.bypass || self.params.amount <= 0.0 {
            return (l, r);
        }

        // Cross signal: low-passed copy of each channel, delayed by the
        // ear-to-ear time-of-flight, then attenuated by `amount`.
        let mut cross_l = l;
        for f in &mut self.lp_l {
            cross_l = f.process(cross_l);
        }
        let mut cross_r = r;
        for f in &mut self.lp_r {
            cross_r = f.process(cross_r);
        }
        let cross_l = self.delay_l.process(cross_l);
        let cross_r = self.delay_r.process(cross_r);

        // Bauer's classic level: -9 dB at full crossfeed (linear ≈ 0.355).
        // We scale linearly with `amount` so the knob is intuitive.
        let g = self.params.amount * 0.355;

        // Each ear gets its direct channel + the OPPOSITE ear's cross
        // signal — that's the wrong-ear path that didn't exist on
        // headphones until we added it back here.
        (l + cross_r * g, r + cross_l * g)
    }
}
