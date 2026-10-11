//! Offline full-chain harness. Does not connect to the user's audio server.
use big_bass::{BassEnhancer, BassEnhancerParams};
use big_clarity::{ClarityParams, ClarityProcessor};
use big_cross::{CrossfeedParams, CrossfeedProcessor};
use big_loud::{LoudnessParams, LoudnessProcessor};
use big_space::{SpaceParams, SpaceProcessor};
use std::collections::HashMap;

pub struct Chain {
    bass: BassEnhancer,
    clarity: ClarityProcessor,
    space: SpaceProcessor,
    cross: CrossfeedProcessor,
    loud: LoudnessProcessor,
}

impl Chain {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            bass: BassEnhancer::new(2, sample_rate, BassEnhancerParams::default()),
            clarity: ClarityProcessor::new(2, sample_rate, ClarityParams::default()),
            space: SpaceProcessor::new(sample_rate, SpaceParams::default()),
            cross: CrossfeedProcessor::new(sample_rate, CrossfeedParams::default()),
            loud: LoudnessProcessor::new(sample_rate, LoudnessParams::default()),
        }
    }

    pub fn apply(&mut self, p: &HashMap<String, f32>) {
        let get = |key: &str, default| p.get(key).copied().unwrap_or(default);
        self.bass.set_params(BassEnhancerParams {
            target_freq: get("bigbass:target_freq", 90.0),
            drive: get("bigbass:drive", 0.45),
            mix: get("bigbass:mix", 0.35),
            cut_dry_lows: get("bigbass:cut_dry_lows", 0.0),
            loudness_db: get("bigbass:loudness_db", 2.5),
            bypass: false,
        });
        self.clarity.set_params(ClarityParams {
            target_freq: get("bigclarity:target_freq", 4000.0),
            drive: get("bigclarity:drive", 0.3),
            mix: get("bigclarity:mix", 0.2),
            bypass: false,
        });
        self.space.set_params(SpaceParams {
            width: get("bigspace:width", 1.2),
            bass_keep_hz: get("bigspace:bass_keep_hz", 150.0),
            mix: get("bigspace:mix", 1.0),
            bypass: false,
        });
        self.cross.set_params(CrossfeedParams {
            amount: get("bigcross:amount", 0.3),
            cutoff_hz: get("bigcross:cutoff_hz", 700.0),
            delay_us: get("bigcross:delay_us", 280.0),
            bypass: false,
        });
        self.loud.set_params(LoudnessParams {
            amount: get("bigloud:amount", 0.4),
            ceiling_db: get("bigloud:ceiling_db", -1.0),
            mix: get("bigloud:mix", 1.0),
            bypass: false,
        });
    }

    pub fn reset(&mut self) {
        self.bass.reset();
        self.clarity.reset();
        self.space.reset();
        self.cross.reset();
        self.loud.reset();
    }

    pub fn process(&mut self, mut frame: [f32; 2]) -> [f32; 2] {
        self.bass.process_frame(&mut frame);
        self.clarity.process_frame(&mut frame);
        let (l, r) = self.space.process_stereo(frame[0], frame[1]);
        let (l, r) = self.cross.process_stereo(l, r);
        let (l, r) = self.loud.process_stereo(l, r);
        [l, r]
    }
}

pub fn bundled_profiles() -> Vec<(String, HashMap<String, f32>)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../daemon/data/profiles");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .expect("profile directory")
        .map(|p| p.expect("profile path").path())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let profile: serde_json::Value =
                serde_json::from_slice(&std::fs::read(p).expect("read profile"))
                    .expect("valid JSON");
            let name = profile["name"].as_str().expect("profile name").to_owned();
            let params =
                serde_json::from_value(profile["params"].clone()).expect("numeric parameters");
            (name, params)
        })
        .collect()
}

/// Deterministic broadband stimulus: no RNG dependency or allocation.
pub fn stimulus(seed: &mut u32) -> f32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    (*seed as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
}
