//! LADSPA wrapper for BigCross — stereo plugin (2 in / 2 out).

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use big_cross::{CrossfeedParams, CrossfeedProcessor};
use std::ffi::c_ulong;
use std::os::raw::{c_char, c_void};

const LADSPA_PROPERTY_HARD_RT_CAPABLE: i32 = 0x4;

const LADSPA_PORT_INPUT: i32 = 0x1;
const LADSPA_PORT_OUTPUT: i32 = 0x2;
const LADSPA_PORT_CONTROL: i32 = 0x4;
const LADSPA_PORT_AUDIO: i32 = 0x8;

const LADSPA_HINT_BOUNDED_BELOW: i32 = 0x1;
const LADSPA_HINT_BOUNDED_ABOVE: i32 = 0x2;
const LADSPA_HINT_DEFAULT_LOW: i32 = 0x80;
const LADSPA_HINT_DEFAULT_MIDDLE: i32 = 0xC0;

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

const PORT_AMOUNT: usize = 0;
const PORT_CUTOFF: usize = 1;
const PORT_DELAY: usize = 2;
const PORT_AUDIO_IN_L: usize = 3;
const PORT_AUDIO_IN_R: usize = 4;
const PORT_AUDIO_OUT_L: usize = 5;
const PORT_AUDIO_OUT_R: usize = 6;
const PORT_COUNT: usize = 7;

static PORT_DESCRIPTORS: [i32; PORT_COUNT] = [
    LADSPA_PORT_INPUT | LADSPA_PORT_CONTROL,
    LADSPA_PORT_INPUT | LADSPA_PORT_CONTROL,
    LADSPA_PORT_INPUT | LADSPA_PORT_CONTROL,
    LADSPA_PORT_INPUT | LADSPA_PORT_AUDIO,
    LADSPA_PORT_INPUT | LADSPA_PORT_AUDIO,
    LADSPA_PORT_OUTPUT | LADSPA_PORT_AUDIO,
    LADSPA_PORT_OUTPUT | LADSPA_PORT_AUDIO,
];

static PORT_NAME_AMOUNT: &[u8] = b"amount\0";
static PORT_NAME_CUTOFF: &[u8] = b"cutoff_hz\0";
static PORT_NAME_DELAY: &[u8] = b"delay_us\0";
static PORT_NAME_INPUT_L: &[u8] = b"Input_L\0";
static PORT_NAME_INPUT_R: &[u8] = b"Input_R\0";
static PORT_NAME_OUTPUT_L: &[u8] = b"Output_L\0";
static PORT_NAME_OUTPUT_R: &[u8] = b"Output_R\0";

static PORT_NAMES: CCharPtrArray<PORT_COUNT> = CCharPtrArray([
    PORT_NAME_AMOUNT.as_ptr() as *const c_char,
    PORT_NAME_CUTOFF.as_ptr() as *const c_char,
    PORT_NAME_DELAY.as_ptr() as *const c_char,
    PORT_NAME_INPUT_L.as_ptr() as *const c_char,
    PORT_NAME_INPUT_R.as_ptr() as *const c_char,
    PORT_NAME_OUTPUT_L.as_ptr() as *const c_char,
    PORT_NAME_OUTPUT_R.as_ptr() as *const c_char,
]);

static PORT_HINTS: [PortRangeHint; PORT_COUNT] = [
    // amount: 0..=1, default 0 (off — speakers don't need it).
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_LOW,
        lower_bound: 0.0,
        upper_bound: 1.0,
    },
    // cutoff_hz: 400..=1500, default 700.
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_MIDDLE,
        lower_bound: 400.0,
        upper_bound: 1500.0,
    },
    // delay_us: 100..=500, default 280.
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_MIDDLE,
        lower_bound: 100.0,
        upper_bound: 500.0,
    },
    PortRangeHint { hint_descriptor: 0, lower_bound: 0.0, upper_bound: 0.0 },
    PortRangeHint { hint_descriptor: 0, lower_bound: 0.0, upper_bound: 0.0 },
    PortRangeHint { hint_descriptor: 0, lower_bound: 0.0, upper_bound: 0.0 },
    PortRangeHint { hint_descriptor: 0, lower_bound: 0.0, upper_bound: 0.0 },
];

static LABEL: &[u8] = b"big_cross\0";
static NAME: &[u8] = b"BigCross - stereo crossfeed (Bauer)\0";
static MAKER: &[u8] = b"BigCommunity / Leonardo Athayde\0";
static COPYRIGHT: &[u8] = b"GPL-3.0-or-later\0";
const UNIQUE_ID: c_ulong = 7781;

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

struct Instance {
    processor: CrossfeedProcessor,
    last_params: CrossfeedParams,
    port_amount: *const f32,
    port_cutoff: *const f32,
    port_delay: *const f32,
    port_audio_in_l: *const f32,
    port_audio_in_r: *const f32,
    port_audio_out_l: *mut f32,
    port_audio_out_r: *mut f32,
}

unsafe extern "C" fn instantiate(_d: *const Descriptor, sample_rate: c_ulong) -> *mut c_void {
    let sr = sample_rate as f32;
    let params = CrossfeedParams::default();
    let inst = Box::new(Instance {
        processor: CrossfeedProcessor::new(sr, params),
        last_params: params,
        port_amount: std::ptr::null(),
        port_cutoff: std::ptr::null(),
        port_delay: std::ptr::null(),
        port_audio_in_l: std::ptr::null(),
        port_audio_in_r: std::ptr::null(),
        port_audio_out_l: std::ptr::null_mut(),
        port_audio_out_r: std::ptr::null_mut(),
    });
    Box::into_raw(inst) as *mut c_void
}

unsafe extern "C" fn connect_port(handle: *mut c_void, port: c_ulong, data: *mut f32) {
    let inst = unsafe { &mut *(handle as *mut Instance) };
    match port as usize {
        PORT_AMOUNT => inst.port_amount = data,
        PORT_CUTOFF => inst.port_cutoff = data,
        PORT_DELAY => inst.port_delay = data,
        PORT_AUDIO_IN_L => inst.port_audio_in_l = data,
        PORT_AUDIO_IN_R => inst.port_audio_in_r = data,
        PORT_AUDIO_OUT_L => inst.port_audio_out_l = data,
        PORT_AUDIO_OUT_R => inst.port_audio_out_r = data,
        _ => {}
    }
}

unsafe extern "C" fn activate(handle: *mut c_void) {
    let inst = unsafe { &mut *(handle as *mut Instance) };
    inst.processor.reset();
}

unsafe extern "C" fn run(handle: *mut c_void, sample_count: c_ulong) {
    let inst = unsafe { &mut *(handle as *mut Instance) };

    let amount = if inst.port_amount.is_null() {
        0.0
    } else {
        unsafe { *inst.port_amount }
    };
    let cutoff = if inst.port_cutoff.is_null() {
        700.0
    } else {
        unsafe { *inst.port_cutoff }
    };
    let delay = if inst.port_delay.is_null() {
        280.0
    } else {
        unsafe { *inst.port_delay }
    };

    let params = CrossfeedParams {
        amount: amount.clamp(0.0, 1.0),
        cutoff_hz: cutoff.clamp(400.0, 1500.0),
        delay_us: delay.clamp(100.0, 500.0),
        bypass: false,
    };

    if params != inst.last_params {
        inst.processor.set_params(params);
        inst.last_params = params;
    }

    let n = sample_count as usize;
    if inst.port_audio_in_l.is_null()
        || inst.port_audio_in_r.is_null()
        || inst.port_audio_out_l.is_null()
        || inst.port_audio_out_r.is_null()
        || n == 0
    {
        return;
    }

    for i in 0..n {
        let l = unsafe { *inst.port_audio_in_l.add(i) };
        let r = unsafe { *inst.port_audio_in_r.add(i) };
        let (out_l, out_r) = inst.processor.process_stereo(l, r);
        unsafe {
            *inst.port_audio_out_l.add(i) = out_l;
            *inst.port_audio_out_r.add(i) = out_r;
        }
    }
}

unsafe extern "C" fn cleanup(handle: *mut c_void) {
    drop(unsafe { Box::from_raw(handle as *mut Instance) });
}

#[no_mangle]
pub extern "C" fn ladspa_descriptor(index: c_ulong) -> *const c_void {
    if index == 0 {
        &DESCRIPTOR as *const Descriptor as *const c_void
    } else {
        std::ptr::null()
    }
}
