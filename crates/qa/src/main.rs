use bigsound_qa::{bundled_profiles, stimulus, Chain};
use std::time::Instant;

fn main() {
    let seconds: usize = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "10".into())
        .parse()
        .expect("seconds as a positive integer");
    assert!((1..=3600).contains(&seconds));
    let profiles = bundled_profiles();
    println!(
        "sample_rate,block_frames,frames,wall_ms,realtime_factor,p99_block_us,max_block_us,peak"
    );
    for rate in [44100, 48000, 96000, 192000] {
        for block in [64, 256, 1024] {
            let mut chain = Chain::new(rate as f32);
            chain.apply(&profiles[0].1);
            let blocks = (rate * seconds).div_ceil(block);
            let mut timings = Vec::with_capacity(blocks);
            let mut seed = 42;
            let mut peak = 0.0_f32;
            let start = Instant::now();
            for b in 0..blocks {
                // Include profile switching and state reset in the workload.
                if b % 127 == 0 {
                    chain.apply(&profiles[(b / 127) % profiles.len()].1);
                }
                if b % 509 == 0 {
                    chain.reset();
                }
                let tick = Instant::now();
                for _ in 0..block {
                    let frame = [stimulus(&mut seed) * 2.0, stimulus(&mut seed) * 2.0];
                    for out in chain.process(frame) {
                        assert!(out.is_finite() && out.abs() <= 1.000002);
                        peak = peak.max(out.abs());
                    }
                }
                timings.push(tick.elapsed().as_secs_f64() * 1e6);
            }
            let elapsed = start.elapsed().as_secs_f64();
            timings.sort_by(f64::total_cmp);
            let frames = blocks * block;
            println!(
                "{rate},{block},{frames},{:.3},{:.3},{:.3},{:.3},{peak:.7}",
                elapsed * 1000.0,
                frames as f64 / rate as f64 / elapsed,
                timings[(blocks - 1) * 99 / 100],
                timings[blocks - 1]
            );
        }
    }
}
