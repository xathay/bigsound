//! LADSPA wrapper for BigLoud — stereo plugin (2 in / 2 out) so the
//! compressor can detect stereo-linked peaks and preserve the image.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use big_loud::{LoudnessParams, LoudnessProcessor};
use ladspa_wrapper::{
    c_char, c_ulong, c_void, read_control, CCharPtrArray, Descriptor, PortRangeHint,
    LADSPA_HINT_BOUNDED_ABOVE, LADSPA_HINT_BOUNDED_BELOW, LADSPA_HINT_DEFAULT_HIGH,
    LADSPA_HINT_DEFAULT_MAXIMUM, LADSPA_HINT_DEFAULT_MIDDLE, LADSPA_PORT_AUDIO,
    LADSPA_PORT_CONTROL, LADSPA_PORT_INPUT, LADSPA_PORT_OUTPUT, LADSPA_PROPERTY_HARD_RT_CAPABLE,
    NO_HINT,
};

// Port layout ---------------------------------------------------------------

const PORT_AMOUNT: usize = 0;
const PORT_CEILING_DB: usize = 1;
const PORT_MIX: usize = 2;
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
static PORT_NAME_CEILING: &[u8] = b"ceiling_db\0";
static PORT_NAME_MIX: &[u8] = b"mix\0";
static PORT_NAME_INPUT_L: &[u8] = b"Input_L\0";
static PORT_NAME_INPUT_R: &[u8] = b"Input_R\0";
static PORT_NAME_OUTPUT_L: &[u8] = b"Output_L\0";
static PORT_NAME_OUTPUT_R: &[u8] = b"Output_R\0";

static PORT_NAMES: CCharPtrArray<PORT_COUNT> = CCharPtrArray([
    PORT_NAME_AMOUNT.as_ptr() as *const c_char,
    PORT_NAME_CEILING.as_ptr() as *const c_char,
    PORT_NAME_MIX.as_ptr() as *const c_char,
    PORT_NAME_INPUT_L.as_ptr() as *const c_char,
    PORT_NAME_INPUT_R.as_ptr() as *const c_char,
    PORT_NAME_OUTPUT_L.as_ptr() as *const c_char,
    PORT_NAME_OUTPUT_R.as_ptr() as *const c_char,
]);

static PORT_HINTS: [PortRangeHint; PORT_COUNT] = [
    // amount: 0..=1, default 0.6 (a bit above middle, FxSound-medium)
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_HIGH,
        lower_bound: 0.0,
        upper_bound: 1.0,
    },
    // ceiling_db: -3..=0, middle of range used as default; filter-chain
    // config overrides anyway.
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_MIDDLE,
        lower_bound: -3.0,
        upper_bound: 0.0,
    },
    // mix: 0..=1, default 1 (fully wet)
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_MAXIMUM,
        lower_bound: 0.0,
        upper_bound: 1.0,
    },
    NO_HINT,
    NO_HINT,
    NO_HINT,
    NO_HINT,
];

static LABEL: &[u8] = b"big_loud\0";
static NAME: &[u8] = b"BigLoud - loudness shaping (compressor + limiter)\0";
static MAKER: &[u8] = b"BigCommunity / Leonardo Athayde\0";
static COPYRIGHT: &[u8] = b"GPL-3.0-or-later\0";
const UNIQUE_ID: c_ulong = 7778;

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

// Per-instance state -------------------------------------------------------

struct Instance {
    processor: LoudnessProcessor,
    last_params: LoudnessParams,
    port_amount: *const f32,
    port_ceiling: *const f32,
    port_mix: *const f32,
    port_audio_in_l: *const f32,
    port_audio_in_r: *const f32,
    port_audio_out_l: *mut f32,
    port_audio_out_r: *mut f32,
}

unsafe extern "C" fn instantiate(_d: *const Descriptor, sample_rate: c_ulong) -> *mut c_void {
    let sr = sample_rate as f32;
    let params = LoudnessParams::default();
    let inst = Box::new(Instance {
        processor: LoudnessProcessor::new(sr, params),
        last_params: params,
        port_amount: std::ptr::null(),
        port_ceiling: std::ptr::null(),
        port_mix: std::ptr::null(),
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
        PORT_CEILING_DB => inst.port_ceiling = data,
        PORT_MIX => inst.port_mix = data,
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

    let amount = unsafe { read_control(inst.port_amount, 0.6) };
    let ceiling = unsafe { read_control(inst.port_ceiling, -1.0) };
    let mix = unsafe { read_control(inst.port_mix, 1.0) };

    let params = LoudnessParams {
        amount: amount.clamp(0.0, 1.0),
        ceiling_db: ceiling.clamp(-12.0, 0.0),
        mix: mix.clamp(0.0, 1.0),
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
    unsafe { ladspa_wrapper::descriptor_or_null(index, &DESCRIPTOR) }
}
