//! RBJ biquad filters — Direct Form II Transposed.
//!
//! Reference: Robert Bristow-Johnson, "Cookbook formulae for audio EQ
//! biquad filter coefficients".

use std::f32::consts::PI;

#[derive(Clone, Copy, Debug)]
pub struct BiquadCoeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoeffs {
    pub const PASSTHROUGH: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    pub fn bandpass(sample_rate: f32, freq: f32, q: f32) -> Self {
        let omega = 2.0 * PI * freq / sample_rate;
        let (sin_o, cos_o) = omega.sin_cos();
        let alpha = sin_o / (2.0 * q);

        let b0 = q * alpha;
        let b1 = 0.0;
        let b2 = -q * alpha;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_o;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    pub fn highpass(sample_rate: f32, freq: f32, q: f32) -> Self {
        let omega = 2.0 * PI * freq / sample_rate;
        let (sin_o, cos_o) = omega.sin_cos();
        let alpha = sin_o / (2.0 * q);

        let b0 = (1.0 + cos_o) / 2.0;
        let b1 = -(1.0 + cos_o);
        let b2 = (1.0 + cos_o) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_o;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    pub fn lowpass(sample_rate: f32, freq: f32, q: f32) -> Self {
        let omega = 2.0 * PI * freq / sample_rate;
        let (sin_o, cos_o) = omega.sin_cos();
        let alpha = sin_o / (2.0 * q);

        let b0 = (1.0 - cos_o) / 2.0;
        let b1 = 1.0 - cos_o;
        let b2 = (1.0 - cos_o) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_o;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }
}

impl Default for BiquadCoeffs {
    fn default() -> Self {
        Self::PASSTHROUGH
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    coeffs: BiquadCoeffs,
    z1: f32,
    z2: f32,
}

impl Biquad {
    pub fn new(coeffs: BiquadCoeffs) -> Self {
        Self {
            coeffs,
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub fn set_coeffs(&mut self, coeffs: BiquadCoeffs) {
        self.coeffs = coeffs;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.coeffs.b0 * x + self.z1;
        self.z1 = self.coeffs.b1 * x - self.coeffs.a1 * y + self.z2;
        self.z2 = self.coeffs.b2 * x - self.coeffs.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn run(filter: &mut Biquad, input: &[f32]) -> Vec<f32> {
        input.iter().map(|&x| filter.process(x)).collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        let n = samples.len() as f32;
        (samples.iter().map(|x| x * x).sum::<f32>() / n).sqrt()
    }

    fn sine(freq: f32, n: usize, sr: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin())
            .collect()
    }

    #[test]
    fn passthrough_coeffs_preserve_input() {
        let mut f = Biquad::new(BiquadCoeffs::PASSTHROUGH);
        let input = sine(1000.0, 256, SR);
        let output = run(&mut f, &input);
        assert_eq!(input, output);
    }

    #[test]
    fn highpass_kills_dc() {
        let mut f = Biquad::new(BiquadCoeffs::highpass(SR, 80.0, 0.707));
        // Process enough samples for the filter to settle.
        let _ = run(&mut f, &vec![1.0; 4096]);
        let tail = run(&mut f, &vec![1.0; 1024]);
        let tail_rms = rms(&tail);
        assert!(tail_rms < 1.0e-3, "DC leaked through highpass: {tail_rms}");
    }

    #[test]
    fn lowpass_attenuates_high_frequencies() {
        let mut lp = Biquad::new(BiquadCoeffs::lowpass(SR, 1000.0, 0.707));
        // Discard transient, then measure RMS of a 8 kHz sine — well above cutoff.
        let signal = sine(8000.0, 8192, SR);
        let output = run(&mut lp, &signal);
        let in_rms = rms(&signal[4096..]);
        let out_rms = rms(&output[4096..]);
        let attenuation_db = 20.0 * (out_rms / in_rms).log10();
        assert!(
            attenuation_db < -10.0,
            "expected ≥10dB cut, got {attenuation_db} dB"
        );
    }

    #[test]
    fn bandpass_passes_centre_attenuates_far() {
        let mut centre_filter = Biquad::new(BiquadCoeffs::bandpass(SR, 1000.0, 1.0));
        let mut far_filter = Biquad::new(BiquadCoeffs::bandpass(SR, 1000.0, 1.0));
        let centre_in = sine(1000.0, 8192, SR);
        let far_in = sine(50.0, 8192, SR);
        let centre_out = run(&mut centre_filter, &centre_in);
        let far_out = run(&mut far_filter, &far_in);
        let centre_ratio = rms(&centre_out[4096..]) / rms(&centre_in[4096..]);
        let far_ratio = rms(&far_out[4096..]) / rms(&far_in[4096..]);
        assert!(
            centre_ratio > far_ratio * 5.0,
            "{centre_ratio} vs {far_ratio}"
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut f = Biquad::new(BiquadCoeffs::lowpass(SR, 1000.0, 0.707));
        let _ = run(&mut f, &sine(500.0, 4096, SR));
        f.reset();
        assert_eq!(f.z1, 0.0);
        assert_eq!(f.z2, 0.0);
    }

    #[test]
    fn output_is_finite_under_extreme_input() {
        let mut f = Biquad::new(BiquadCoeffs::highpass(SR, 200.0, 0.707));
        let input: Vec<f32> = (0..4096)
            .map(|i| if i % 2 == 0 { 1e6 } else { -1e6 })
            .collect();
        let output = run(&mut f, &input);
        assert!(output.iter().all(|x| x.is_finite()));
    }
}
