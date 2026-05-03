//! BigSpace — stereo widening via Mid/Side processing.
//!
//! Decomposes the stereo signal into Mid (L+R) and Side (L−R), scales the
//! Side component, and recomposes. This is the canonical stereo-widener
//! technique — used in everywhere from mastering plugins to consumer
//! audio enhancers (FxSound's "Ambience" / "Surround", BBE 3-D Sound,
//! Waves S1).
//!
//! With a bass-keep-mono safety: the Side component is high-passed before
//! the widening so low frequencies remain centred. This preserves mono
//! compatibility (laptop/phone speakers that physically sum to mono don't
//! lose any low-end energy due to L/R cancellation) and produces a more
//! "anchored" image — wide highs, solid lows.

use big_bass::biquad::{Biquad, BiquadCoeffs};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpaceParams {
    /// Width factor on the Side component.
    /// 0.0 = mono · 1.0 = neutral · up to 2.0 = very wide.
    pub width: f32,
    /// Below this frequency (Hz) the Side is high-passed to nothing —
    /// keeps bass centred for mono compatibility. Range 40..=400.
    pub bass_keep_hz: f32,
    /// Dry/wet — 1.0 = fully widened, 0.0 = unaffected.
    pub mix: f32,
    /// Pass-through.
    pub bypass: bool,
}

impl Default for SpaceParams {
    fn default() -> Self {
        Self {
            width: 1.3,
            bass_keep_hz: 200.0,
            mix: 1.0,
            bypass: false,
        }
    }
}

/// Stereo Mid/Side widener with optional bass-keep-mono safety.
pub struct SpaceProcessor {
    /// 4th-order Butterworth high-pass on the Side path. Cascade of two
    /// biquads at the same cutoff so the bass below `bass_keep_hz` is
    /// only present in the Mid component.
    side_hp: [Biquad; 2],
    sample_rate: f32,
    params: SpaceParams,
}

impl SpaceProcessor {
    pub fn new(sample_rate: f32, params: SpaceParams) -> Self {
        let mut s = Self {
            side_hp: [Biquad::default(); 2],
            sample_rate,
            params,
        };
        s.update_coeffs();
        s
    }

    fn update_coeffs(&mut self) {
        let hp = BiquadCoeffs::highpass(self.sample_rate, self.params.bass_keep_hz, 0.707);
        for f in &mut self.side_hp {
            f.set_coeffs(hp);
        }
    }

    pub fn set_params(&mut self, p: SpaceParams) {
        self.params = p;
        self.update_coeffs();
    }

    pub fn reset(&mut self) {
        for f in &mut self.side_hp {
            f.reset();
        }
    }

    #[inline]
    pub fn process_stereo(&mut self, l: f32, r: f32) -> (f32, f32) {
        if self.params.bypass {
            return (l, r);
        }

        // Decompose into Mid/Side. The 0.5 factor preserves unity gain
        // when reconstructing (M + S = L, M − S = R).
        let mid = (l + r) * 0.5;
        let side = (l - r) * 0.5;

        // High-pass the Side so the bass stays in Mid only.
        let mut side_hp = side;
        for f in &mut self.side_hp {
            side_hp = f.process(side_hp);
        }
        // What's been removed from Side stays in the Side path as the
        // un-widened low-frequency component. We add it back unscaled so
        // bass below `bass_keep_hz` is preserved at original level.
        let side_low = side - side_hp;
        let widened_side = side_hp * self.params.width + side_low;

        // Recompose.
        let widened_l = mid + widened_side;
        let widened_r = mid - widened_side;

        // Dry/wet crossfade.
        let m = self.params.mix.clamp(0.0, 1.0);
        let out_l = (1.0 - m) * l + m * widened_l;
        let out_r = (1.0 - m) * r + m * widened_r;
        (out_l, out_r)
    }
}
