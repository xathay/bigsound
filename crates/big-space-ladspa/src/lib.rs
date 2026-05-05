//! LADSPA wrapper for BigSpace — stereo (2 in / 2 out).

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use big_space::{SpaceParams, SpaceProcessor};
use ladspa_wrapper::{
    c_char, c_ulong, c_void, read_control, CCharPtrArray, Descriptor, PortRangeHint,
    LADSPA_HINT_BOUNDED_ABOVE, LADSPA_HINT_BOUNDED_BELOW, LADSPA_HINT_DEFAULT_LOW,
    LADSPA_HINT_DEFAULT_MAXIMUM, LADSPA_HINT_DEFAULT_MIDDLE, LADSPA_PORT_AUDIO,
    LADSPA_PORT_CONTROL, LADSPA_PORT_INPUT, LADSPA_PORT_OUTPUT, LADSPA_PROPERTY_HARD_RT_CAPABLE,
    NO_HINT,
};

const PORT_WIDTH: usize = 0;
const PORT_BASS_KEEP: usize = 1;
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

static PORT_NAME_WIDTH: &[u8] = b"width\0";
static PORT_NAME_BASS_KEEP: &[u8] = b"bass_keep_hz\0";
static PORT_NAME_MIX: &[u8] = b"mix\0";
static PORT_NAME_INPUT_L: &[u8] = b"Input_L\0";
static PORT_NAME_INPUT_R: &[u8] = b"Input_R\0";
static PORT_NAME_OUTPUT_L: &[u8] = b"Output_L\0";
static PORT_NAME_OUTPUT_R: &[u8] = b"Output_R\0";

static PORT_NAMES: CCharPtrArray<PORT_COUNT> = CCharPtrArray([
    PORT_NAME_WIDTH.as_ptr() as *const c_char,
    PORT_NAME_BASS_KEEP.as_ptr() as *const c_char,
    PORT_NAME_MIX.as_ptr() as *const c_char,
    PORT_NAME_INPUT_L.as_ptr() as *const c_char,
    PORT_NAME_INPUT_R.as_ptr() as *const c_char,
    PORT_NAME_OUTPUT_L.as_ptr() as *const c_char,
    PORT_NAME_OUTPUT_R.as_ptr() as *const c_char,
]);

static PORT_HINTS: [PortRangeHint; PORT_COUNT] = [
    // width: 0..=2, default ~1.0 from MIDDLE
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_MIDDLE,
        lower_bound: 0.0,
        upper_bound: 2.0,
    },
    // bass_keep_hz: 40..=400, default low (200)
    PortRangeHint {
        hint_descriptor: LADSPA_HINT_BOUNDED_BELOW
            | LADSPA_HINT_BOUNDED_ABOVE
            | LADSPA_HINT_DEFAULT_LOW,
        lower_bound: 40.0,
        upper_bound: 400.0,
    },
    // mix: 0..=1, default 1
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

static LABEL: &[u8] = b"big_space\0";
static NAME: &[u8] = b"BigSpace - stereo widening (mid/side)\0";
static MAKER: &[u8] = b"BigCommunity / Leonardo Athayde\0";
static COPYRIGHT: &[u8] = b"GPL-3.0-or-later\0";
const UNIQUE_ID: c_ulong = 7780;

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
    processor: SpaceProcessor,
    last_params: SpaceParams,
    port_width: *const f32,
    port_bass_keep: *const f32,
    port_mix: *const f32,
    port_audio_in_l: *const f32,
    port_audio_in_r: *const f32,
    port_audio_out_l: *mut f32,
    port_audio_out_r: *mut f32,
}

unsafe extern "C" fn instantiate(_d: *const Descriptor, sample_rate: c_ulong) -> *mut c_void {
    let sr = sample_rate as f32;
    let params = SpaceParams::default();
    let inst = Box::new(Instance {
        processor: SpaceProcessor::new(sr, params),
        last_params: params,
        port_width: std::ptr::null(),
        port_bass_keep: std::ptr::null(),
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
        PORT_WIDTH => inst.port_width = data,
        PORT_BASS_KEEP => inst.port_bass_keep = data,
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

    let width = unsafe { read_control(inst.port_width, 1.3) };
    let bass_keep = unsafe { read_control(inst.port_bass_keep, 200.0) };
    let mix = unsafe { read_control(inst.port_mix, 1.0) };

    let params = SpaceParams {
        width: width.clamp(0.0, 2.0),
        bass_keep_hz: bass_keep.clamp(40.0, 400.0),
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
