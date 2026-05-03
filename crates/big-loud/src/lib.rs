//! BigLoud — single-band loudness shaping: stereo-linked feed-forward
//! compressor with soft knee, followed by a stereo peak limiter.
//!
//! This is the module that gives BigSound the "FxSound feel" — when you
//! flip it on, music feels louder and more present without the master
//! volume changing. The compressor lifts the RMS by squashing peaks and
//! adding makeup gain; the limiter catches the remaining peaks so the
//! signal never clips.
//!
//! v0.1 is single-band (broadband). A future v0.2 will swap this for a
//! 2-3 band Linkwitz-Riley split with per-band compression — the canonical
//! multiband-loudness architecture used by FxSound, MaxxAudio and others.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoudnessParams {
    /// Compression amount 0..=1. Internally maps to threshold + ratio +
    /// makeup. 0 = no compression (pass through). 1 = aggressive squash.
    pub amount: f32,
    /// Output ceiling in dBFS — the limiter holds the peak under this.
    /// Default -1.0 (= 0.891 linear). Range -3..=0.
    pub ceiling_db: f32,
    /// Dry/wet mix. 1.0 = fully processed. 0.0 = pass through.
    pub mix: f32,
    /// Bypass the entire processor.
    pub bypass: bool,
}

impl Default for LoudnessParams {
    fn default() -> Self {
        Self {
            amount: 0.6,
            ceiling_db: -1.0,
            mix: 1.0,
            bypass: false,
        }
    }
}

/// Stereo-linked feed-forward compressor. Detection is on the maximum of
/// |L| and |R| so the gain reduction is applied identically to both
/// channels — the stereo image stays intact.
struct Compressor {
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    makeup_db: f32,
    attack_coef: f32,
    release_coef: f32,
    gain_db: f32, // current gain reduction state, in dB (≤ 0)
}

impl Compressor {
    fn new(sample_rate: f32, attack_ms: f32, release_ms: f32) -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 3.0,
            knee_db: 6.0,
            makeup_db: 0.0,
            attack_coef: (-1.0 / (attack_ms * 0.001 * sample_rate)).exp(),
            release_coef: (-1.0 / (release_ms * 0.001 * sample_rate)).exp(),
            gain_db: 0.0,
        }
    }

    /// Map a single 0..=1 "amount" knob to threshold, ratio, makeup. The
    /// makeup is *calibrated* — for a given `target_ceiling_db`, the math
    /// places the makeup so that a 0 dBFS input peak (the loudest possible
    /// sample) hits exactly `target_ceiling_db` after compression + makeup.
    /// The limiter then catches anything that overshoots.
    ///
    /// Why this matters: a fixed makeup like `+8·amount` undershoots the
    /// gain reduction at higher ratios — the compressor then *reduces*
    /// loudness instead of raising it. Calibrating to the ceiling extracts
    /// the maximum perceivable loudness for any chosen compression amount
    /// — the FxSound trick.
    fn set_amount(&mut self, amount: f32, target_ceiling_db: f32) {
        let a = amount.clamp(0.0, 1.0);
        self.threshold_db = -6.0 - 18.0 * a; // a=0 → -6 dB, a=1 → -24 dB
        self.ratio = 1.0 + 5.0 * a; // a=0 → 1:1, a=1 → 6:1
        self.knee_db = 6.0;

        // Expected gain reduction for a 0 dBFS input peak, in dB.
        let max_over = -self.threshold_db;
        let max_gr = max_over * (1.0 - 1.0 / self.ratio);
        // Add slope = 0 → 0 makeup, so amount=0 stays unity.
        self.makeup_db = (target_ceiling_db + max_gr).max(0.0);
    }

    #[inline]
    fn process_stereo(&mut self, l: f32, r: f32) -> (f32, f32) {
        // Stereo-linked peak detection.
        let peak = l.abs().max(r.abs()).max(1.0e-10);
        let peak_db = 20.0 * peak.log10();

        // Soft-knee gain reduction calculation.
        let over = peak_db - self.threshold_db;
        let half_knee = self.knee_db * 0.5;
        let target_gr_db = if over <= -half_knee {
            0.0
        } else if over >= half_knee {
            -over * (1.0 - 1.0 / self.ratio)
        } else {
            // Quadratic in the knee region — smooth onset.
            let x = over + half_knee;
            let factor = (x * x) / (2.0 * self.knee_db);
            -factor * (1.0 - 1.0 / self.ratio)
        };

        // Attack when getting more attenuated, release when lifting.
        let coef = if target_gr_db < self.gain_db {
            self.attack_coef
        } else {
            self.release_coef
        };
        self.gain_db = target_gr_db + coef * (self.gain_db - target_gr_db);

        let total_db = self.gain_db + self.makeup_db;
        let gain = 10.0_f32.powf(total_db / 20.0);
        (l * gain, r * gain)
    }

    fn reset(&mut self) {
        self.gain_db = 0.0;
    }
}

/// Stereo-linked peak limiter. Same algorithm as `big_bass::PeakLimiter`
/// but the detector is the max of both channels so the stereo image
/// is preserved when the limiter pulls back gain.
struct StereoPeakLimiter {
    threshold: f32,
    release_coef: f32,
    gain: f32,
}

impl StereoPeakLimiter {
    fn new(sample_rate: f32, ceiling_db: f32, release_ms: f32) -> Self {
        Self {
            threshold: 10.0_f32.powf(ceiling_db / 20.0),
            release_coef: (-1.0 / (release_ms * 0.001 * sample_rate)).exp(),
            gain: 1.0,
        }
    }

    fn set_ceiling_db(&mut self, db: f32) {
        self.threshold = 10.0_f32.powf(db / 20.0);
    }

    #[inline]
    fn process_stereo(&mut self, l: f32, r: f32) -> (f32, f32) {
        let peak = (l * self.gain).abs().max((r * self.gain).abs());
        if peak > self.threshold {
            self.gain *= self.threshold / peak;
        } else {
            self.gain = 1.0 + self.release_coef * (self.gain - 1.0);
        }
        (l * self.gain, r * self.gain)
    }

    fn reset(&mut self) {
        self.gain = 1.0;
    }
}

pub struct LoudnessProcessor {
    compressor: Compressor,
    limiter: StereoPeakLimiter,
    params: LoudnessParams,
}

impl LoudnessProcessor {
    pub fn new(sample_rate: f32, params: LoudnessParams) -> Self {
        let mut s = Self {
            // Attack 10 ms (instead of fast 5 ms) lets transients punch
            // through before the compressor clamps — drums feel "alive"
            // rather than squashed. Release 80 ms is a musical default.
            compressor: Compressor::new(sample_rate, 10.0, 80.0),
            limiter: StereoPeakLimiter::new(sample_rate, params.ceiling_db, 50.0),
            params,
        };
        s.apply_params();
        s
    }

    fn apply_params(&mut self) {
        self.compressor
            .set_amount(self.params.amount, self.params.ceiling_db);
        self.limiter.set_ceiling_db(self.params.ceiling_db);
    }

    pub fn set_params(&mut self, p: LoudnessParams) {
        self.params = p;
        self.apply_params();
    }

    pub fn reset(&mut self) {
        self.compressor.reset();
        self.limiter.reset();
    }

    #[inline]
    pub fn process_stereo(&mut self, l: f32, r: f32) -> (f32, f32) {
        if self.params.bypass {
            return (l, r);
        }

        let (cl, cr) = self.compressor.process_stereo(l, r);

        // Linear dry/wet crossfade — useful for A/B and for a "subtle" preset.
        let m = self.params.mix.clamp(0.0, 1.0);
        let mixed_l = (1.0 - m) * l + m * cl;
        let mixed_r = (1.0 - m) * r + m * cr;

        self.limiter.process_stereo(mixed_l, mixed_r)
    }
}
