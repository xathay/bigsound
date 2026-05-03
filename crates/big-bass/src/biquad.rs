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
