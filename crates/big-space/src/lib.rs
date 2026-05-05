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
//!
//! ## Phase-flip on hard-panned content at width > 1
//!
//! M/S widening with `width > 1` amplifies the (L − R) Side component.
//! For *hard-panned* input — say L = x, R = 0 — that gives Mid = x/2,
//! Side = x/2, and recomposed channels L' = x(1 + W)/2, R' = x(1 − W)/2.
//! At W > 1 the recovered R' carries the **opposite phase** of the
//! original input. Combined with BigCross (which adds in-phase delayed
//! copies of the opposite channel) the apparent inter-aural correlation
//! flips strongly negative — perceptually the brain reads this as
//! "anti-phase mush", not as the natural out-of-head image crossfeed
//! would produce on its own. This is intrinsic to all linear M/S
//! wideners; pro-audio plugins handle it by capping the slider.
//!
//! BigSound caps the GTK slider for `bigspace:width` at 1.5 for that
//! reason. Profiles that previously shipped wider values (Atmos/Cinema,
//! Gaming) were tuned down. The DSP itself accepts the full range —
//! the cap is policy at the UI layer.

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
#[derive(Debug)]
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

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn sine(freq: f32, n: usize, sr: f32, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin() * amp)
            .collect()
    }

    #[test]
    fn bypass_passes_input_through() {
        let mut p = SpaceProcessor::new(
            SR,
            SpaceParams {
                bypass: true,
                ..Default::default()
            },
        );
        let l = sine(440.0, 512, SR, 0.5);
        let r = sine(660.0, 512, SR, 0.5);
        for i in 0..l.len() {
            let (ol, or) = p.process_stereo(l[i], r[i]);
            assert_eq!(ol, l[i]);
            assert_eq!(or, r[i]);
        }
    }

    #[test]
    fn zero_mix_is_identity() {
        let mut p = SpaceProcessor::new(
            SR,
            SpaceParams {
                width: 2.0,
                mix: 0.0,
                ..Default::default()
            },
        );
        let l = sine(440.0, 512, SR, 0.5);
        let r = sine(660.0, 512, SR, 0.5);
        for i in 0..l.len() {
            let (ol, or) = p.process_stereo(l[i], r[i]);
            assert!((ol - l[i]).abs() < 1.0e-6);
            assert!((or - r[i]).abs() < 1.0e-6);
        }
    }

    #[test]
    fn unity_width_with_full_mix_preserves_signal() {
        let mut p = SpaceProcessor::new(
            SR,
            SpaceParams {
                width: 1.0,
                mix: 1.0,
                bass_keep_hz: 200.0,
                bypass: false,
            },
        );
        // Width=1 → no scaling on the side; mid+side reconstruction is identity.
        let l = sine(2000.0, 4096, SR, 0.4);
        let r = sine(2000.0, 4096, SR, 0.4);
        for i in 2048..l.len() {
            let (ol, or) = p.process_stereo(l[i], r[i]);
            assert!((ol - l[i]).abs() < 1.0e-3);
            assert!((or - r[i]).abs() < 1.0e-3);
        }
    }

    #[test]
    fn mono_input_stays_mono() {
        // L=R means side=0, so the widener can't make them differ.
        let mut p = SpaceProcessor::new(
            SR,
            SpaceParams {
                width: 2.0,
                mix: 1.0,
                ..Default::default()
            },
        );
        let signal = sine(1000.0, 1024, SR, 0.5);
        for &s in &signal {
            let (ol, or) = p.process_stereo(s, s);
            assert!((ol - or).abs() < 1.0e-5);
        }
    }

    #[test]
    fn output_is_finite_under_extreme_input() {
        let mut p = SpaceProcessor::new(SR, SpaceParams::default());
        for i in 0..4096 {
            let s = if i % 2 == 0 { 10.0 } else { -10.0 };
            let (ol, or) = p.process_stereo(s, -s);
            assert!(ol.is_finite() && or.is_finite());
        }
    }

    #[test]
    fn set_params_does_not_panic_under_sweep() {
        let mut p = SpaceProcessor::new(SR, SpaceParams::default());
        for w in [0.0_f32, 0.5, 1.0, 1.5, 2.0] {
            p.set_params(SpaceParams {
                width: w,
                bass_keep_hz: 150.0,
                mix: 1.0,
                bypass: false,
            });
            for _ in 0..256 {
                let _ = p.process_stereo(0.3, -0.3);
            }
        }
    }

    #[test]
    fn reset_clears_filter_state() {
        let mut p = SpaceProcessor::new(SR, SpaceParams::default());
        for _ in 0..1024 {
            let _ = p.process_stereo(1.0, -1.0);
        }
        p.reset();
        let mut tail = 0.0_f32;
        for _ in 0..2048 {
            let (ol, or) = p.process_stereo(0.0, 0.0);
            tail = tail.max(ol.abs()).max(or.abs());
        }
        assert!(tail < 1.0e-5);
    }
}
