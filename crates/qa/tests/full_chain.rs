use bigsound_qa::{bundled_profiles, stimulus, Chain};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
struct CountingAllocator;
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK.with(|track| {
            if track.get() {
                ALLOCATIONS.with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        TRACK.with(|track| {
            if track.get() {
                ALLOCATIONS.with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.realloc(p, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn all_profiles_stay_finite_and_obey_sample_peak_ceiling_at_all_rates() {
    let profiles = bundled_profiles();
    assert_eq!(profiles.len(), 15);
    for rate in [
        8000.0, 16000.0, 22050.0, 44100.0, 48000.0, 96000.0, 192000.0, 384000.0,
    ] {
        let mut chain = Chain::new(rate);
        for (name, params) in &profiles {
            chain.reset();
            chain.apply(params);
            let ceiling = 10.0_f32.powf(params["bigloud:ceiling_db"] / 20.0);
            let mut seed = 0x13579bdf;
            for i in 0..16384 {
                let frame = match i % 4096 {
                    0 => [f32::NAN, f32::INFINITY],
                    1 => [f32::MAX, -f32::MAX],
                    2 => [f32::NEG_INFINITY, f32::NAN],
                    3..=511 => [0.0, 0.0],
                    512..=1023 => [1.5, -1.5],
                    _ => [stimulus(&mut seed) * 2.0, stimulus(&mut seed) * 2.0],
                };
                for out in chain.process(frame) {
                    assert!(
                        out.is_finite() && out.abs() <= ceiling + 2e-6,
                        "{name} at {rate}: {out} ceiling {ceiling}, frame {i}"
                    );
                }
            }
        }
    }
}

#[test]
fn audio_processing_and_parameter_updates_do_not_allocate() {
    let mut chain = Chain::new(48000.0);
    let profiles = bundled_profiles();
    ALLOCATIONS.with(|n| n.set(0));
    TRACK.with(|n| n.set(true));
    let mut checksum = 0.0;
    for (_, params) in &profiles {
        chain.apply(params);
        for _ in 0..4096 {
            checksum += chain.process([0.2, -0.3])[0];
        }
        chain.reset();
    }
    TRACK.with(|n| n.set(false));
    let allocations = ALLOCATIONS.with(Cell::get);
    assert!(checksum.is_finite());
    assert_eq!(allocations, 0, "audio path allocated");
}

#[test]
fn malformed_controls_are_sanitised_and_recover_without_reset() {
    let mut chain = Chain::new(48000.0);
    for invalid in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        -f32::MAX,
    ] {
        let mut params = bundled_profiles()[0].1.clone();
        params.values_mut().for_each(|v| *v = invalid);
        chain.apply(&params);
        for _ in 0..4096 {
            assert!(chain.process([0.3, -0.2]).iter().all(|v| v.is_finite()));
        }
    }
}

#[test]
fn mono_content_remains_centred_with_every_profile() {
    let mut chain = Chain::new(48000.0);
    for (_, params) in bundled_profiles() {
        chain.reset();
        chain.apply(&params);
        let mut seed = 123;
        for _ in 0..8192 {
            let x = stimulus(&mut seed);
            let [l, r] = chain.process([x, x]);
            assert!((l - r).abs() < 1e-6);
        }
    }
}
