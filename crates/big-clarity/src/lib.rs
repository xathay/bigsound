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
