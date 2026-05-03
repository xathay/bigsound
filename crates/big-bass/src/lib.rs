//! BigBass — psychoacoustic bass enhancement.
//!
//! Restores the *perceived* low-frequency content of small speakers by
//! synthesising harmonics in a band the speaker can reproduce; the brain
//! reconstructs the missing fundamental from the harmonic series
//! (Aarts/Larsen "phantom bass" / "missing fundamental" effect).

pub mod biquad;

use biquad::{Biquad, BiquadCoeffs};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BassEnhancerParams {
    /// Lower edge of the band the speaker fails to reproduce (Hz).
    /// Typical: 80 Hz (large speaker) .. 180 Hz (laptop / phone).
    pub target_freq: f32,
    /// Waveshaper drive — controls harmonic richness. 0..=1
    pub drive: f32,
    /// Mix amount of the synthesised harmonics into the dry signal. 0..=1
    pub mix: f32,
    /// High-pass the dry signal at `target_freq` so cone excursion isn't
    /// wasted on un-reproducible content.
    pub cut_dry_lows: bool,
    /// Make-up gain in dB applied after the harmonic mix. Combined with
    /// the built-in peak limiter, this raises perceived loudness without
    /// clipping — the "FxSound-on" feel where flipping the effect on
    /// makes everything noticeably louder. Range -12..=+12 dB. 0 = unity.
    pub loudness_db: f32,
    /// Pass-through.
    pub bypass: bool,
}

impl Default for BassEnhancerParams {
    fn default() -> Self {
        Self {
            target_freq: 100.0,
            drive: 0.6,
            mix: 0.5,
            cut_dry_lows: false,
            loudness_db: 4.0,
            bypass: false,
        }
    }
}

/// Peak envelope follower with separate attack/release time constants.
#[derive(Clone, Copy, Debug, Default)]
struct PeakEnvelope {
    attack_coef: f32,
    release_coef: f32,
    env: f32,
}

/// Single-band peak limiter with instant attack and exponential release.
/// Holds the output peak below `threshold` by attenuating gain when an
/// over-threshold sample arrives, then recovering toward unity. Cheap
/// (no look-ahead, no oversampling) but sufficient as a safety net for
/// the make-up gain stage. A future BigLoud module will replace this
/// with a multiband stereo-linked limiter for proper loudness shaping.
#[derive(Clone, Copy, Debug, Default)]
struct PeakLimiter {
    threshold: f32,
    release_coef: f32,
    gain: f32,
}

impl PeakLimiter {
    fn new(sample_rate: f32, threshold: f32, release_ms: f32) -> Self {
        Self {
            threshold,
            release_coef: (-1.0 / (release_ms * 0.001 * sample_rate)).exp(),
            gain: 1.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let abs_with_gain = (x * self.gain).abs();
        if abs_with_gain > self.threshold {
            // Instant attack: clamp gain so the output stays below threshold.
            self.gain *= self.threshold / abs_with_gain;
        } else {
            // Smooth recovery toward unity.
            self.gain = 1.0 + self.release_coef * (self.gain - 1.0);
        }
        x * self.gain
    }

    fn reset(&mut self) {
        self.gain = 1.0;
    }
}

impl PeakEnvelope {
    fn new(sample_rate: f32, attack_ms: f32, release_ms: f32) -> Self {
        Self {
            attack_coef: (-1.0 / (attack_ms * 0.001 * sample_rate)).exp(),
            release_coef: (-1.0 / (release_ms * 0.001 * sample_rate)).exp(),
            env: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let abs = x.abs();
        let coef = if abs > self.env {
            self.attack_coef
        } else {
            self.release_coef
        };
        self.env = abs + coef * (self.env - abs);
        self.env
    }

    fn reset(&mut self) {
        self.env = 0.0;
    }
}

pub struct BassEnhancerChannel {
    sidechain_bp: [Biquad; 2],
    harmonics_bp: [Biquad; 2],
    dry_hp: [Biquad; 2],
    dc_blocker: Biquad,
    sidechain_env: PeakEnvelope,
    limiter: PeakLimiter,
}

impl BassEnhancerChannel {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sidechain_bp: [Biquad::default(); 2],
            harmonics_bp: [Biquad::default(); 2],
            dry_hp: [Biquad::default(); 2],
            dc_blocker: Biquad::default(),
            // Medium attack / long release. Faster envelope (e.g. 5ms
            // attack) sounds "scratchy" on percussion because the gate
            // transitions are too abrupt and generate audible AM sidebands
            // when the harmonic signal is multiplied by the gate.
            sidechain_env: PeakEnvelope::new(sample_rate, 12.0, 200.0),
            // Threshold 0.95 (-0.45 dBFS) leaves a sliver of headroom for
            // the LADSPA host's own clipping. 50ms release feels musical
            // — fast enough not to choke transients, slow enough not to
            // pump on sustained content.
            limiter: PeakLimiter::new(sample_rate, 0.95, 50.0),
        }
    }

    pub fn reset(&mut self) {
        for f in &mut self.sidechain_bp {
            f.reset();
        }
        for f in &mut self.harmonics_bp {
            f.reset();
        }
        for f in &mut self.dry_hp {
            f.reset();
        }
        self.dc_blocker.reset();
        self.sidechain_env.reset();
        self.limiter.reset();
    }

    fn update_coeffs(&mut self, sample_rate: f32, params: &BassEnhancerParams) {
        let target = params.target_freq;

        // Sidechain band centred slightly below target — captures the
        // bass content that is being rolled off by the speaker.
        let sc_coeffs = BiquadCoeffs::bandpass(sample_rate, target * 0.75, 0.7);
        for f in &mut self.sidechain_bp {
            f.set_coeffs(sc_coeffs);
        }

        // Harmonics band: 2nd through ~5th harmonics of `target`.
        let hb_coeffs = BiquadCoeffs::bandpass(sample_rate, target * 2.5, 0.5);
        for f in &mut self.harmonics_bp {
            f.set_coeffs(hb_coeffs);
        }

        // Optional dry HP — Linkwitz-Riley-ish pair of Butterworths.
        let dry_coeffs = BiquadCoeffs::highpass(sample_rate, target, 0.707);
        for f in &mut self.dry_hp {
            f.set_coeffs(dry_coeffs);
        }

        // DC blocker on the rectifier output.
        self.dc_blocker
            .set_coeffs(BiquadCoeffs::highpass(sample_rate, 20.0, 0.707));
    }

    #[inline]
    pub fn process(&mut self, input: f32, params: &BassEnhancerParams) -> f32 {
        if params.bypass {
            return input;
        }

        // Anti-denormal seed — keeps biquad state away from sub-normal
        // floats during silence; denormals slow the FPU 10-50× and can
        // cause realtime underruns on quiet passages.
        let input_safe = input + 1.0e-20;

        // 1. Isolate the band to enhance.
        let mut sc = input_safe;
        for f in &mut self.sidechain_bp {
            sc = f.process(sc);
        }

        // 2. Track sidechain envelope (drives the gate later).
        let env = self.sidechain_env.process(sc);

        // 3. Smooth half-wave rectifier (Aarts NLD, smoothed):
        //    f(x) = 0.5 * (x + sqrt(x² + ε))
        //    ε > 0 rounds the corner at x=0 so the function is C∞,
        //    eliminating the aliasing the sharp rectifier generates.
        //    `tanh` after caps amplitude.
        let drive = 1.0 + params.drive * 9.0;
        let scaled = drive * sc;
        let smooth_abs = (scaled * scaled + 1.0e-3).sqrt();
        let smooth_halfwave = 0.5 * (scaled + smooth_abs);
        let rectified = smooth_halfwave.tanh();

        // 4. Block DC introduced by rectification.
        let no_dc = self.dc_blocker.process(rectified);

        // 5. Bandpass to keep only the harmonic series (2nd..5th).
        let mut harm = no_dc;
        for f in &mut self.harmonics_bp {
            harm = f.process(harm);
        }

        // 6. Gate harmonics by sidechain envelope. The exponential
        //    `1 - exp(-k*env)` rises smoothly from 0 to 1 with no
        //    plateau-clipping, giving softer multiplicative behaviour
        //    than `min(k*env, 1)` (which has a sharp knee at 1).
        let gate = 1.0 - (-env * 8.0).exp();
        let harm_out = harm * gate;

        // 7. Optional cut of un-reproducible lows on the dry signal.
        let dry = if params.cut_dry_lows {
            let mut d = input_safe;
            for f in &mut self.dry_hp {
                d = f.process(d);
            }
            d
        } else {
            input
        };

        // The 2× compensates the amplitude loss of half-wave rectification
        // (rectifier output mean ≈ 0.5× the original peak).
        let enhanced = dry + params.mix * 2.0 * harm_out;

        // Make-up gain: this is what gives BigSound the "feel louder"
        // perception that's expected from a FxSound-style enhancer.
        // The limiter immediately after catches any peaks that would
        // clip, so we can push hard without distortion.
        let makeup = 10.0_f32.powf(params.loudness_db / 20.0);
        self.limiter.process(enhanced * makeup)
    }
}

pub struct BassEnhancer {
    channels: Vec<BassEnhancerChannel>,
    sample_rate: f32,
    params: BassEnhancerParams,
}

impl BassEnhancer {
    pub fn new(num_channels: usize, sample_rate: f32, params: BassEnhancerParams) -> Self {
        let mut s = Self {
            channels: (0..num_channels)
                .map(|_| BassEnhancerChannel::new(sample_rate))
                .collect(),
            sample_rate,
            params,
        };
        for ch in &mut s.channels {
            ch.update_coeffs(sample_rate, &s.params);
        }
        s
    }

    pub fn set_params(&mut self, params: BassEnhancerParams) {
        self.params = params;
        for ch in &mut self.channels {
            ch.update_coeffs(self.sample_rate, &self.params);
        }
    }

    pub fn reset(&mut self) {
        for ch in &mut self.channels {
            ch.reset();
        }
    }

    /// Process one frame (one sample per channel) in place.
    /// `frame.len()` must equal the channel count.
    #[inline]
    pub fn process_frame(&mut self, frame: &mut [f32]) {
        for (ch, sample) in self.channels.iter_mut().zip(frame.iter_mut()) {
            *sample = ch.process(*sample, &self.params);
        }
    }

    /// Process interleaved samples in place.
    pub fn process_interleaved(&mut self, samples: &mut [f32]) {
        let n = self.channels.len();
        for chunk in samples.chunks_exact_mut(n) {
            self.process_frame(chunk);
        }
    }
}
