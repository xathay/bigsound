//! LADSPA wrapper around the BigBass DSP — produces a `cdylib` whose
//! `ladspa_descriptor` symbol PipeWire's filter-chain module loads to
//! place BigBass into the system-wide audio path.
//!
//! LADSPA is a minimal C ABI: see <https://www.ladspa.org/>.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use big_bass::{BassEnhancer, BassEnhancerParams};
use std::ffi::c_ulong;
use std::os::raw::{c_char, c_void};

// LADSPA constants ------------------------------------------------------------

const LADSPA_PROPERTY_HARD_RT_CAPABLE: i32 = 0x4;

const LADSPA_PORT_INPUT: i32 = 0x1;
const LADSPA_PORT_OUTPUT: i32 = 0x2;
const LADSPA_PORT_CONTROL: i32 = 0x4;
const LADSPA_PORT_AUDIO: i32 = 0x8;

const LADSPA_HINT_BOUNDED_BELOW: i32 = 0x1;
const LADSPA_HINT_BOUNDED_ABOVE: i32 = 0x2;
const LADSPA_HINT_TOGGLED: i32 = 0x4;
const LADSPA_HINT_DEFAULT_LOW: i32 = 0x80;
const LADSPA_HINT_DEFAULT_MIDDLE: i32 = 0xC0;
const LADSPA_HINT_DEFAULT_HIGH: i32 = 0x100;
const LADSPA_HINT_DEFAULT_0: i32 = 0x200;

// LADSPA structs --------------------------------------------------------------

#[repr(C)]
struct PortRangeHint {
    hint_descriptor: i32,
    lower_bound: f32,
    upper_bound: f32,
}

#[repr(C)]
struct Descriptor {
    unique_id: c_ulong,
    label: *const c_char,
    properties: i32,
    name: *const c_char,
    maker: *const c_char,
    copyright: *const c_char,
    port_count: c_ulong,
    port_descriptors: *const i32,
    port_names: *const *const c_char,
    port_range_hints: *const PortRangeHint,
    impl_data: *mut c_void,
    instantiate: Option<unsafe extern "C" fn(*const Descriptor, c_ulong) -> *mut c_void>,
    connect_port: Option<unsafe extern "C" fn(*mut c_void, c_ulong, *mut f32)>,
    activate: Option<unsafe extern "C" fn(*mut c_void)>,
    run: Option<unsafe extern "C" fn(*mut c_void, c_ulong)>,
    run_adding: Option<unsafe extern "C" fn(*mut c_void, c_ulong)>,
    set_run_adding_gain: Option<unsafe extern "C" fn(*mut c_void, f32)>,
    deactivate: Option<unsafe extern "C" fn(*mut c_void)>,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
}

unsafe impl Sync for Descriptor {}

#[repr(transparent)]
struct CCharPtrArray<const N: usize>([*const c_char; N]);
unsafe impl<const N: usize> Sync for CCharPtrArray<N> {}

// Port layout -----------------------------------------------------------------

const PORT_TARGET_FREQ: usize = 0;
const PORT_DRIVE: usize = 1;
const PORT_MIX: usize = 2;
const PORT_CUT_DRY: usize = 3;
const PORT_LOUDNESS: usize = 4;
const PORT_AUDIO_IN: usize = 5;
const PORT_AUDIO_OUT: usize = 6;
const PORT_COUNT: usize = 7;

static PORT_DESCRIPTORS: [i32; PORT_COUNT] = [
    LADSPA_PORT_INPUT | LADSPA_PORT_CONTROL,
    LADSPA_PORT_INPUT | LADSPA_PORT_CONTROL,
    LADSPA_PORT_INPUT | LADSPA_PORT_CONTROL,
    LADSPA_PORT_INPUT | LADSPA_PORT_CONTROL,
    LADSPA_PORT_INPUT | LADSPA_PORT_CONTROL,
    LADSPA_PORT_INPUT | LADSPA_PORT_AUDIO,
    LADSPA_PORT_OUTPUT | LADSPA_PORT_AUDIO,
];

static PORT_NAME_TARGET_FREQ: &[u8] = b"target_freq\0";
static PORT_NAME_DRIVE: &[u8] = b"drive\0";
static PORT_NAME_MIX: &[u8] = b"mix\0";
static PORT_NAME_CUT_DRY: &[u8] = b"cut_dry_lows\0";
static PORT_NAME_LOUDNESS: &[u8] = b"loudness_db\0";
static PORT_NAME_INPUT: &[u8] = b"Input\0";
static PORT_NAME_OUTPUT: &[u8] = b"Output\0";

static PORT_NAMES: CCharPtrArray<PORT_COUNT> = CCharPtrArray([
    PORT_NAME_TARGET_FREQ.as_ptr() as *const c_char,
    PORT_NAME_DRIVE.as_ptr() as *const c_char,
    PORT_NAME_MIX.as_ptr() as *const c_char,
    PORT_NAME_CUT_DRY.as_ptr() as *const c_char,
    PORT_NAME_LOUDNESS.as_ptr() as *const c_char,
    PORT_NAME_INPUT.as_ptr() as *const c_char,
    PORT_NAME_OUTPUT.as_ptr() as *const c_char,
]);

static PORT_HINTS: [PortRangeHint; PORT_COUNT] = [
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_LOW,
        lower_bound: 40.0,
        upper_bound: 300.0,
    },
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_MIDDLE,
        lower_bound: 0.0,
        upper_bound: 1.0,
    },
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_MIDDLE,
        lower_bound: 0.0,
        upper_bound: 1.0,
    },
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_TOGGLED
            | LADSPA_HINT_DEFAULT_0,
        lower_bound: 0.0,
        upper_bound: 1.0,
    },
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_HIGH,
        lower_bound: -12.0,
        upper_bound: 12.0,
    },
    PortRangeHint {
        hint_descriptor: 0,
        lower_bound: 0.0,
        upper_bound: 0.0,
    },
    PortRangeHint {
        hint_descriptor: 0,
        lower_bound: 0.0,
        upper_bound: 0.0,
    },
];

// Plugin metadata. UniqueID 7000-9999 is the LADSPA "experimental/personal"
// range — collision-safe for now.
static LABEL: &[u8] = b"big_bass\0";
static NAME: &[u8] = b"BigBass - psychoacoustic bass enhancement\0";
static MAKER: &[u8] = b"BigCommunity / Leonardo Athayde\0";
static COPYRIGHT: &[u8] = b"GPL-3.0-or-later\0";
const UNIQUE_ID: c_ulong = 7777;

static DESCRIPTOR: Descriptor = Descriptor {
    unique_id: UNIQUE_ID,
    label: LABEL.as_ptr() as *const c_char,
    properties: LADSPA_PROPERTY_HARD_RT_CAPABLE,
    name: NAME.as_ptr() as *const c_char,
    maker: MAKER.as_ptr() as *const c_char,
    copyright: COPYRIGHT.as_ptr() as *const c_char,
    port_count: PORT_COUNT as c_ulong,
    port_descriptors: PORT_DESCRIPTORS.as_ptr(),
    port_names: PORT_NAMES.0.as_ptr(),
    port_range_hints: PORT_HINTS.as_ptr(),
    impl_data: std::ptr::null_mut(),
    instantiate: Some(instantiate),
    connect_port: Some(connect_port),
    activate: Some(activate),
    run: Some(run),
    run_adding: None,
    set_run_adding_gain: None,
    deactivate: None,
    cleanup: Some(cleanup),
};

// Per-instance state ----------------------------------------------------------

struct Instance {
    enhancer: BassEnhancer,
    last_params: BassEnhancerParams,
    port_target_freq: *const f32,
    port_drive: *const f32,
    port_mix: *const f32,
    port_cut_dry: *const f32,
    port_loudness: *const f32,
    port_audio_in: *const f32,
    port_audio_out: *mut f32,
}

unsafe extern "C" fn instantiate(_d: *const Descriptor, sample_rate: c_ulong) -> *mut c_void {
    let sr = sample_rate as f32;
    let params = BassEnhancerParams::default();
    let inst = Box::new(Instance {
        enhancer: BassEnhancer::new(1, sr, params),
        last_params: params,
        port_target_freq: std::ptr::null(),
        port_drive: std::ptr::null(),
        port_mix: std::ptr::null(),
        port_cut_dry: std::ptr::null(),
        port_loudness: std::ptr::null(),
        port_audio_in: std::ptr::null(),
        port_audio_out: std::ptr::null_mut(),
    });
    Box::into_raw(inst) as *mut c_void
}

unsafe extern "C" fn connect_port(handle: *mut c_void, port: c_ulong, data: *mut f32) {
    let inst = unsafe { &mut *(handle as *mut Instance) };
    match port as usize {
        PORT_TARGET_FREQ => inst.port_target_freq = data,
        PORT_DRIVE => inst.port_drive = data,
        PORT_MIX => inst.port_mix = data,
        PORT_CUT_DRY => inst.port_cut_dry = data,
        PORT_LOUDNESS => inst.port_loudness = data,
        PORT_AUDIO_IN => inst.port_audio_in = data,
        PORT_AUDIO_OUT => inst.port_audio_out = data,
        _ => {}
    }
}

unsafe extern "C" fn activate(handle: *mut c_void) {
    let inst = unsafe { &mut *(handle as *mut Instance) };
    inst.enhancer.reset();
}

unsafe extern "C" fn run(handle: *mut c_void, sample_count: c_ulong) {
    let inst = unsafe { &mut *(handle as *mut Instance) };

    // Snapshot control values; PipeWire only updates them between blocks.
    let target = if inst.port_target_freq.is_null() {
        100.0
    } else {
        unsafe { *inst.port_target_freq }
    };
    let drive = if inst.port_drive.is_null() {
        0.6
    } else {
        unsafe { *inst.port_drive }
    };
    let mix = if inst.port_mix.is_null() {
        0.5
    } else {
        unsafe { *inst.port_mix }
    };
    let cut = if inst.port_cut_dry.is_null() {
        0.0
    } else {
        unsafe { *inst.port_cut_dry }
    };
    let loudness = if inst.port_loudness.is_null() {
        4.0
    } else {
        unsafe { *inst.port_loudness }
    };

    let params = BassEnhancerParams {
        target_freq: target.clamp(40.0, 300.0),
        drive: drive.clamp(0.0, 1.0),
        mix: mix.clamp(0.0, 1.0),
        cut_dry_lows: cut > 0.5,
        loudness_db: loudness.clamp(-12.0, 12.0),
        bypass: false,
    };

    if params != inst.last_params {
        inst.enhancer.set_params(params);
        inst.last_params = params;
    }

    let n = sample_count as usize;
    if inst.port_audio_in.is_null() || inst.port_audio_out.is_null() || n == 0 {
        return;
    }

    for i in 0..n {
        let mut frame = [unsafe { *inst.port_audio_in.add(i) }];
        inst.enhancer.process_frame(&mut frame);
        unsafe { *inst.port_audio_out.add(i) = frame[0] };
    }
}

unsafe extern "C" fn cleanup(handle: *mut c_void) {
    drop(unsafe { Box::from_raw(handle as *mut Instance) });
}

// Entry point ----------------------------------------------------------------

#[no_mangle]
pub extern "C" fn ladspa_descriptor(index: c_ulong) -> *const c_void {
    if index == 0 {
        &DESCRIPTOR as *const Descriptor as *const c_void
    } else {
        std::ptr::null()
    }
}
