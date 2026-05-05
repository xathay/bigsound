//! BigClarity — treble exciter / harmonic enhancer.
//!
//! Adds "air" and "presence" to recorded audio by synthesising odd-order
//! harmonics of the high-mid band (typically 3-6 kHz). The technique is
//! the same as classic hardware exciters (BBE Sonic Maximizer, Aphex
//! Aural Exciter): isolate a band, run it through a symmetric saturator
//! (`tanh`) so 3rd/5th/7th harmonics fall in 6-30 kHz, lowpass the
//! result so anything above the audible range is killed, mix back in.
//!
//! Result: cymbals shimmer, vocals get sibilance, recordings sound
//! "expensive" rather than "muffled". This is FxSound's "Fidelity" knob.

use big_bass::biquad::{Biquad, BiquadCoeffs};

/// Anti-denormal DC seed (see big-bass for rationale).
const DENORMAL_SEED: f32 = 1.0e-20;

/// Drive knob (0..=1) maps to a tanh pre-gain in [DRIVE_MIN..=DRIVE_MAX].
const DRIVE_MIN: f32 = 1.0;
const DRIVE_MAX: f32 = 10.0;

/// Anti-alias low-pass on the saturator output. Cap at ANTI_ALIAS_MAX_HZ
/// (16 kHz: above the brilliance band, below the most fragile region of
/// hearing) but always stay ANTI_ALIAS_NYQUIST_MARGIN_HZ below Nyquist so
/// higher-order tanh harmonics don't fold into the audible band.
const ANTI_ALIAS_MAX_HZ: f32 = 16000.0;
const ANTI_ALIAS_NYQUIST_MARGIN_HZ: f32 = 200.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClarityParams {
    /// Lower edge of the band that gets excited (Hz). Above this, the
    /// saturator generates harmonic content. Range 1500..=8000.
    pub target_freq: f32,
    /// Saturator drive — how hard the input is pushed into `tanh`.
    /// Higher = richer harmonics + more saturation. Range 0..=1.
    pub drive: f32,
    /// Mix amount of synthesised harmonics into the dry signal.
    /// Range 0..=1. Subtle defaults — too much sounds harsh.
    pub mix: f32,
    /// Pass-through.
    pub bypass: bool,
}

impl Default for ClarityParams {
    fn default() -> Self {
        Self {
            target_freq: 3500.0,
            drive: 0.4,
            mix: 0.3,
            bypass: false,
        }
    }
}

#[derive(Debug)]
pub struct ClarityChannel {
    /// Cascade of two HP biquads = 4th-order Butterworth.
    /// Isolates the high-mid band that will be saturated.
    sidechain_hp: [Biquad; 2],
    /// Anti-alias LP at ~16 kHz to clip harmonics that would otherwise
    /// fold around Nyquist into the audible range.
    output_lp: [Biquad; 2],
}

impl ClarityChannel {
    pub fn new() -> Self {
        Self {
            sidechain_hp: [Biquad::default(); 2],
            output_lp: [Biquad::default(); 2],
        }
    }

    pub fn reset(&mut self) {
        for f in &mut self.sidechain_hp {
            f.reset();
        }
        for f in &mut self.output_lp {
            f.reset();
        }
    }

    fn update_coeffs(&mut self, sample_rate: f32, params: &ClarityParams) {
        let hp = BiquadCoeffs::highpass(sample_rate, params.target_freq, 0.707);
        for f in &mut self.sidechain_hp {
            f.set_coeffs(hp);
        }

        let lp_freq = (sample_rate * 0.5 - ANTI_ALIAS_NYQUIST_MARGIN_HZ).min(ANTI_ALIAS_MAX_HZ);
        let lp = BiquadCoeffs::lowpass(sample_rate, lp_freq, 0.707);
        for f in &mut self.output_lp {
            f.set_coeffs(lp);
        }
    }

    #[inline]
    pub fn process(&mut self, input: f32, params: &ClarityParams) -> f32 {
        if params.bypass {
            return input;
        }

        let input_safe = input + DENORMAL_SEED;

        // Isolate the high-mid band.
        let mut hf = input_safe;
        for f in &mut self.sidechain_hp {
            hf = f.process(hf);
        }

        // Symmetric saturation. `tanh` produces ONLY odd-order harmonics
        // (3rd, 5th, 7th…) — the kind that sound "expensive". Even
        // harmonics would sound "fuzzy" / "tube-warm" — wrong vibe for
        // sparkle. Dividing by drive normalises the output level so the
        // drive knob doesn't double as a volume knob.
        let drive = DRIVE_MIN + params.drive * (DRIVE_MAX - DRIVE_MIN);
        let saturated = (drive * hf).tanh() / drive;

        // Bandlimit to remove inaudible/aliased content.
        let mut harmonics = saturated;
        for f in &mut self.output_lp {
            harmonics = f.process(harmonics);
        }

        input + params.mix * harmonics
    }
}

impl Default for ClarityChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct ClarityProcessor {
    channels: Vec<ClarityChannel>,
    sample_rate: f32,
    params: ClarityParams,
}

impl ClarityProcessor {
    pub fn new(num_channels: usize, sample_rate: f32, params: ClarityParams) -> Self {
        let mut s = Self {
            channels: (0..num_channels).map(|_| ClarityChannel::new()).collect(),
            sample_rate,
            params,
        };
        for ch in &mut s.channels {
            ch.update_coeffs(sample_rate, &s.params);
        }
        s
    }

    pub fn set_params(&mut self, params: ClarityParams) {
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

    #[inline]
    pub fn process_frame(&mut self, frame: &mut [f32]) {
        for (ch, sample) in self.channels.iter_mut().zip(frame.iter_mut()) {
            *sample = ch.process(*sample, &self.params);
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

    fn process_mono(p: &mut ClarityProcessor, input: &[f32]) -> Vec<f32> {
        input
            .iter()
            .map(|&x| {
                let mut frame = [x];
                p.process_frame(&mut frame);
                frame[0]
            })
            .collect()
    }

    #[test]
    fn bypass_passes_input_through() {
        let mut p = ClarityProcessor::new(
            1,
            SR,
            ClarityParams {
                bypass: true,
                ..Default::default()
            },
        );
        let input = sine(440.0, 512, SR);
        let output = process_mono(&mut p, &input);
        assert_eq!(input, output);
    }

    #[test]
    fn zero_mix_is_identity() {
        let mut p = ClarityProcessor::new(
            1,
            SR,
            ClarityParams {
                mix: 0.0,
                ..Default::default()
            },
        );
        let input = sine(2000.0, 4096, SR);
        let output = process_mono(&mut p, &input);
        // input + 0 * harmonics ≈ input (within denormal seed).
        for (a, b) in input[2048..].iter().zip(output[2048..].iter()) {
            assert!((a - b).abs() < 1.0e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn silence_in_silence_out() {
        let mut p = ClarityProcessor::new(1, SR, ClarityParams::default());
        let output = process_mono(&mut p, &vec![0.0; 4096]);
        let tail_max = output[2048..].iter().fold(0.0_f32, |a, &x| a.max(x.abs()));
        assert!(tail_max < 1.0e-3, "tail max = {tail_max}");
    }

    #[test]
    fn output_is_finite_under_extreme_input() {
        let mut p = ClarityProcessor::new(1, SR, ClarityParams::default());
        let input: Vec<f32> = (0..4096)
            .map(|i| if i % 2 == 0 { 5.0 } else { -5.0 })
            .collect();
        let output = process_mono(&mut p, &input);
        assert!(output.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn set_params_works_under_sweep() {
        let mut p = ClarityProcessor::new(1, SR, ClarityParams::default());
        for f in [2000.0, 3500.0, 6000.0] {
            p.set_params(ClarityParams {
                target_freq: f,
                drive: 0.5,
                mix: 0.3,
                bypass: false,
            });
            let _ = process_mono(&mut p, &sine(f, 256, SR));
        }
    }

    #[test]
    fn reset_clears_filter_state() {
        let mut p = ClarityProcessor::new(1, SR, ClarityParams::default());
        let _ = process_mono(&mut p, &vec![1.0; 1024]);
        p.reset();
        let output = process_mono(&mut p, &vec![0.0; 2048]);
        let late_max = output[1024..].iter().fold(0.0_f32, |a, &x| a.max(x.abs()));
        assert!(late_max < 1.0e-3);
    }

    #[test]
    fn multi_channel_processes_independently() {
        let mut p = ClarityProcessor::new(2, SR, ClarityParams::default());
        let l = sine(440.0, 512, SR);
        let r = sine(880.0, 512, SR);
        let mut out_l = Vec::with_capacity(l.len());
        let mut out_r = Vec::with_capacity(r.len());
        for i in 0..l.len() {
            let mut frame = [l[i], r[i]];
            p.process_frame(&mut frame);
            out_l.push(frame[0]);
            out_r.push(frame[1]);
        }
        // The two outputs must differ (independent channel state).
        let diff: f32 = out_l
            .iter()
            .zip(out_r.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(diff > 1.0);
    }
}
