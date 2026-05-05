//! BigBass — psychoacoustic bass enhancement.
//!
//! Restores the *perceived* low-frequency content of small speakers by
//! synthesising harmonics in a band the speaker can reproduce; the brain
//! reconstructs the missing fundamental from the harmonic series
//! (Aarts/Larsen "phantom bass" / "missing fundamental" effect).

pub mod biquad;

use biquad::{Biquad, BiquadCoeffs};

/// Anti-denormal DC seed added to inputs. Below ~1e-38 the FPU drops into
/// sub-normal mode and slows 10-50×, causing realtime underruns on quiet
/// passages. 1e-20 is well above that threshold yet inaudible.
const DENORMAL_SEED: f32 = 1.0e-20;

/// Drive knob (0..=1) maps linearly to a tanh pre-gain in [DRIVE_MIN..=DRIVE_MAX].
/// 10× max drive matches the harmonic richness of classic hardware exciters
/// without flattening the input into a square wave.
const DRIVE_MIN: f32 = 1.0;
const DRIVE_MAX: f32 = 10.0;

/// Smooths the corner of the half-wave rectifier `0.5*(x + sqrt(x² + ε))`
/// so the function is C∞ at x=0 — eliminates the aliasing the sharp
/// rectifier would generate.
const RECTIFIER_EPS: f32 = 1.0e-3;

/// Compensates the amplitude loss of half-wave rectification (mean ≈ 0.5×
/// the original peak).
const HALFWAVE_GAIN_COMP: f32 = 2.0;

/// Exponential rate of the harmonic gate `1 - exp(-k * env)`. Higher = the
/// gate opens faster as the sidechain envelope rises. 8.0 hits ~99% open
/// at env ≈ 0.6, soft enough to avoid AM sidebands on percussion.
const GATE_RATE: f32 = 8.0;

/// DC blocker placed on the rectifier output. 20 Hz is the conventional
/// edge — below the audible band but high enough to converge fast.
const DC_BLOCKER_HZ: f32 = 20.0;

/// Sidechain bandpass centre, expressed as a fraction of `target_freq`.
/// Slightly below target captures the bass content the speaker is rolling
/// off rather than the target itself.
const SIDECHAIN_BAND_RATIO: f32 = 0.75;
const SIDECHAIN_BAND_Q: f32 = 0.7;

/// Harmonic bandpass centre — covers ~2nd through ~5th harmonics of `target`.
const HARMONICS_BAND_RATIO: f32 = 2.5;
const HARMONICS_BAND_Q: f32 = 0.5;

/// Sidechain envelope follower constants. Faster (~5 ms) attack sounds
/// "scratchy" on percussion because the gate transitions are too abrupt
/// and produce audible AM sidebands when the harmonics get multiplied.
const SIDECHAIN_ATTACK_MS: f32 = 12.0;
const SIDECHAIN_RELEASE_MS: f32 = 200.0;

/// Peak limiter on the make-up gain stage. 0.95 (-0.45 dBFS) leaves a
/// sliver of headroom for the LADSPA host's own clipping; 50 ms release
/// is fast enough not to choke transients, slow enough not to pump on
/// sustained content.
const LIMITER_THRESHOLD: f32 = 0.95;
const LIMITER_RELEASE_MS: f32 = 50.0;

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

#[derive(Debug)]
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
            sidechain_env: PeakEnvelope::new(sample_rate, SIDECHAIN_ATTACK_MS, SIDECHAIN_RELEASE_MS),
            limiter: PeakLimiter::new(sample_rate, LIMITER_THRESHOLD, LIMITER_RELEASE_MS),
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

        let sc_coeffs = BiquadCoeffs::bandpass(
            sample_rate,
            target * SIDECHAIN_BAND_RATIO,
            SIDECHAIN_BAND_Q,
        );
        for f in &mut self.sidechain_bp {
            f.set_coeffs(sc_coeffs);
        }

        let hb_coeffs = BiquadCoeffs::bandpass(
            sample_rate,
            target * HARMONICS_BAND_RATIO,
            HARMONICS_BAND_Q,
        );
        for f in &mut self.harmonics_bp {
            f.set_coeffs(hb_coeffs);
        }

        // Optional dry HP — Linkwitz-Riley-ish pair of Butterworths.
        let dry_coeffs = BiquadCoeffs::highpass(sample_rate, target, 0.707);
        for f in &mut self.dry_hp {
            f.set_coeffs(dry_coeffs);
        }

        self.dc_blocker
            .set_coeffs(BiquadCoeffs::highpass(sample_rate, DC_BLOCKER_HZ, 0.707));
    }

    #[inline]
    pub fn process(&mut self, input: f32, params: &BassEnhancerParams) -> f32 {
        if params.bypass {
            return input;
        }

        let input_safe = input + DENORMAL_SEED;

        // 1. Isolate the band to enhance.
        let mut sc = input_safe;
        for f in &mut self.sidechain_bp {
            sc = f.process(sc);
        }

        // 2. Track sidechain envelope (drives the gate later).
        let env = self.sidechain_env.process(sc);

        // 3. Smooth half-wave rectifier (Aarts NLD, smoothed):
        //    f(x) = 0.5 * (x + sqrt(x² + ε)), ε rounds the corner at x=0.
        //    `tanh` after caps amplitude.
        let drive = DRIVE_MIN + params.drive * (DRIVE_MAX - DRIVE_MIN);
        let scaled = drive * sc;
        let smooth_abs = (scaled * scaled + RECTIFIER_EPS).sqrt();
        let smooth_halfwave = 0.5 * (scaled + smooth_abs);
        let rectified = smooth_halfwave.tanh();

        // 4. Block DC introduced by rectification.
        let no_dc = self.dc_blocker.process(rectified);

        // 5. Bandpass to keep only the harmonic series (2nd..5th).
        let mut harm = no_dc;
        for f in &mut self.harmonics_bp {
            harm = f.process(harm);
        }

        // 6. Gate harmonics by sidechain envelope: `1 - exp(-k*env)` rises
        //    smoothly from 0 to 1 with no plateau-clipping.
        let gate = 1.0 - (-env * GATE_RATE).exp();
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

        let enhanced = dry + params.mix * HALFWAVE_GAIN_COMP * harm_out;

        // Make-up gain: this is what gives BigSound the "feel louder"
        // perception that's expected from a FxSound-style enhancer.
        // The limiter immediately after catches any peaks that would
        // clip, so we can push hard without distortion.
        let makeup = 10.0_f32.powf(params.loudness_db / 20.0);
        self.limiter.process(enhanced * makeup)
    }
}

#[derive(Debug)]
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

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn sine(freq: f32, n: usize, sr: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin() * 0.5)
            .collect()
    }

    fn process_mono(enh: &mut BassEnhancer, input: &[f32]) -> Vec<f32> {
        input
            .iter()
            .map(|&x| {
                let mut frame = [x];
                enh.process_frame(&mut frame);
                frame[0]
            })
            .collect()
    }

    #[test]
    fn bypass_passes_input_through() {
        let mut enh = BassEnhancer::new(
            1,
            SR,
            BassEnhancerParams {
                bypass: true,
                ..Default::default()
            },
        );
        let input = sine(440.0, 512, SR);
        let output = process_mono(&mut enh, &input);
        assert_eq!(input, output);
    }

    #[test]
    fn silence_in_silence_out_within_tolerance() {
        let mut enh = BassEnhancer::new(1, SR, BassEnhancerParams::default());
        let output = process_mono(&mut enh, &vec![0.0; 4096]);
        // Steady-state output on silence must be tiny — the only
        // contribution is the anti-denormal seed propagating through.
        let tail_max = output[2048..].iter().fold(0.0_f32, |a, &x| a.max(x.abs()));
        assert!(tail_max < 1.0e-3, "tail max = {tail_max}");
    }

    #[test]
    fn output_is_finite_for_sine_input() {
        let mut enh = BassEnhancer::new(1, SR, BassEnhancerParams::default());
        let output = process_mono(&mut enh, &sine(120.0, 4096, SR));
        assert!(output.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn limiter_caps_loud_makeup_below_unity() {
        let mut enh = BassEnhancer::new(
            1,
            SR,
            BassEnhancerParams {
                loudness_db: 12.0,
                drive: 1.0,
                mix: 1.0,
                ..Default::default()
            },
        );
        let input: Vec<f32> = sine(100.0, 4096, SR).iter().map(|x| x * 1.8).collect();
        let output = process_mono(&mut enh, &input);
        let peak = output[2048..].iter().fold(0.0_f32, |a, &x| a.max(x.abs()));
        assert!(peak <= 1.0, "limiter let peak through: {peak}");
    }

    #[test]
    fn output_is_finite_for_extreme_input() {
        let mut enh = BassEnhancer::new(1, SR, BassEnhancerParams::default());
        let input: Vec<f32> = (0..4096).map(|i| if i % 2 == 0 { 10.0 } else { -10.0 }).collect();
        let output = process_mono(&mut enh, &input);
        assert!(output.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn set_params_does_not_panic_under_param_sweep() {
        let mut enh = BassEnhancer::new(1, SR, BassEnhancerParams::default());
        for f in [60.0, 100.0, 180.0, 300.0] {
            enh.set_params(BassEnhancerParams {
                target_freq: f,
                drive: 0.5,
                mix: 0.7,
                loudness_db: 0.0,
                ..Default::default()
            });
            let _ = process_mono(&mut enh, &sine(f, 256, SR));
        }
    }

    #[test]
    fn reset_clears_filter_state() {
        let mut enh = BassEnhancer::new(1, SR, BassEnhancerParams::default());
        // Excite filters with a transient.
        let _ = process_mono(&mut enh, &vec![1.0; 1024]);
        enh.reset();
        // After reset, the response to silence must converge near 0 fast.
        let output = process_mono(&mut enh, &vec![0.0; 2048]);
        let early_max = output[..256].iter().fold(0.0_f32, |a, &x| a.max(x.abs()));
        let late_max = output[1024..].iter().fold(0.0_f32, |a, &x| a.max(x.abs()));
        assert!(late_max <= early_max + 1.0e-6);
    }
}
