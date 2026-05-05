//! Documents the interaction between BigSpace (M/S widener) and BigCross
//! (Bauer crossfeed) in the chain laid out by
//! configs/pipewire/10-bigsound.conf.template.
//!
//! Background: a beta tester reported "crossfeed stops working when
//! BigSpace width ≥ 1.0". width=1.0 is mathematically identity for
//! BigSpace (verified by the unity_width_with_full_mix_preserves_signal
//! test in big-space), so the literal claim is false. The genuine
//! observation underneath is that for width >> 1.0 the *perceived*
//! out-of-head effect of crossfeed fades, because the widener and the
//! crossfeed pull in opposite directions:
//!
//!   * BigCross reduces inter-aural difference (mixes some L into R
//!     and vice versa, low-passed and delayed) — the brain reads the
//!     resulting high inter-aural correlation as "speakers in front
//!     of me", out of the head.
//!   * BigSpace at width > 1 amplifies the (L − R) side component —
//!     the brain reads the resulting low inter-aural correlation as
//!     "very wide stereo, hard inside the head".
//!
//! These are not bugs; they are conflicting psychoacoustic goals on
//! the same signal. The two operators are also linear and time-
//! invariant, so re-ordering the chain (BigCross before BigSpace vs
//! after) produces identical output — there is no algorithmic fix.
//! The remedy lives at the profile-design layer: when crossfeed is
//! intended to dominate (headphone profiles), keep width at or near
//! 1.0; when widening is intended to dominate (speaker profiles),
//! disable crossfeed.
//!
//! This integration test pins the documented behaviour as a
//! regression guard: chain order must not affect the right-channel
//! signature, and increasing width must visibly degrade the
//! crossfeed-as-correlation effect.

use big_cross::{CrossfeedParams, CrossfeedProcessor};
use big_space::{SpaceParams, SpaceProcessor};

const SR: f32 = 48000.0;
const N: usize = 8192;

fn pseudo_left_only_noise() -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(N);
    let mut rng = 0x12345678u32;
    for _ in 0..N {
        rng = rng.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let s = ((rng >> 16) as i32 as f32) / 32_768.0 * 0.3;
        out.push((s, 0.0));
    }
    out
}

#[derive(Clone, Copy)]
enum Order {
    SpaceFirst,
    CrossFirst,
}

fn run_chain(width: f32, cross_amount: f32, order: Order) -> (Vec<f32>, Vec<f32>) {
    let mut space = SpaceProcessor::new(
        SR,
        SpaceParams {
            width,
            bass_keep_hz: 150.0,
            mix: 1.0,
            bypass: false,
        },
    );
    let mut cross = CrossfeedProcessor::new(
        SR,
        CrossfeedParams {
            amount: cross_amount,
            cutoff_hz: 700.0,
            delay_us: 280.0,
            bypass: false,
        },
    );
    let (mut lo, mut ro) = (Vec::with_capacity(N), Vec::with_capacity(N));
    for (l, r) in pseudo_left_only_noise() {
        let (a, b) = match order {
            Order::SpaceFirst => {
                let (l1, r1) = space.process_stereo(l, r);
                cross.process_stereo(l1, r1)
            }
            Order::CrossFirst => {
                let (l1, r1) = cross.process_stereo(l, r);
                space.process_stereo(l1, r1)
            }
        };
        lo.push(a);
        ro.push(b);
    }
    (lo, ro)
}

/// Pearson correlation between two channels, on the steady-state tail.
/// 1.0 = identical (mono), 0.0 = uncorrelated, −1.0 = anti-phase.
/// Crossfeed raises this number; widening pushes it down.
fn pearson(l: &[f32], r: &[f32]) -> f32 {
    let n = l.len() as f32;
    let lm: f32 = l.iter().sum::<f32>() / n;
    let rm: f32 = r.iter().sum::<f32>() / n;
    let mut num = 0.0_f32;
    let mut dl = 0.0_f32;
    let mut dr = 0.0_f32;
    for (&a, &b) in l.iter().zip(r.iter()) {
        num += (a - lm) * (b - rm);
        dl += (a - lm).powi(2);
        dr += (b - rm).powi(2);
    }
    num / (dl * dr).sqrt().max(f32::EPSILON)
}

#[test]
fn chain_order_is_irrelevant() {
    // BigSpace and BigCross are LTI; they commute. Swapping order must
    // produce numerically identical output — this guards against any
    // future change that would silently make one of them stateful in
    // a way that breaks commutativity.
    for &(w, a) in &[(1.0, 0.6), (1.5, 0.6), (2.0, 0.6), (1.2, 0.3)] {
        let (l_s, r_s) = run_chain(w, a, Order::SpaceFirst);
        let (l_c, r_c) = run_chain(w, a, Order::CrossFirst);
        for (i, ((&ls, &rs), (&lc, &rc))) in
            l_s.iter().zip(r_s.iter()).zip(l_c.iter().zip(r_c.iter())).enumerate()
        {
            assert!(
                (ls - lc).abs() < 1e-5 && (rs - rc).abs() < 1e-5,
                "chain orders diverged at sample {i} for w={w} a={a}: \
                 space-first=({ls},{rs}) cross-first=({lc},{rc})"
            );
        }
    }
}

#[test]
fn widening_reduces_crossfeed_correlation() {
    // The brain reads inter-aural correlation as "out of head". This
    // pins that crossfeed alone produces high correlation, and that
    // increasing BigSpace width measurably erodes it — confirming the
    // perceptual report from the beta tester.
    let tail = 1024..N;

    let (l1, r1) = run_chain(1.0, 0.6, Order::SpaceFirst);
    let corr_w10 = pearson(&l1[tail.clone()], &r1[tail.clone()]);

    let (l2, r2) = run_chain(1.5, 0.6, Order::SpaceFirst);
    let corr_w15 = pearson(&l2[tail.clone()], &r2[tail.clone()]);

    let (l3, r3) = run_chain(2.0, 0.6, Order::SpaceFirst);
    let corr_w20 = pearson(&l3[tail.clone()], &r3[tail.clone()]);

    eprintln!("inter-aural correlation (higher = more out-of-head):");
    eprintln!("  width 1.0 + cross 0.6  →  {corr_w10:.3}");
    eprintln!("  width 1.5 + cross 0.6  →  {corr_w15:.3}");
    eprintln!("  width 2.0 + cross 0.6  →  {corr_w20:.3}");

    assert!(
        corr_w10 > corr_w15 && corr_w15 > corr_w20,
        "widening must monotonically erode the crossfeed-correlation effect"
    );
}
