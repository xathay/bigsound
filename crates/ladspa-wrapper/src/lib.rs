//! Shared LADSPA scaffolding for the BigSound plugin wrappers.
//!
//! Every BigSound LADSPA wrapper used to redeclare the LADSPA C ABI
//! (`Descriptor`, `PortRangeHint`, the bit-flag constants, the `Sync`
//! impls and the `ladspa_descriptor` entry point) verbatim. This crate
//! centralises those bits so each wrapper now only carries:
//!
//! - the per-plugin port layout and hints;
//! - the per-plugin `Instance` struct and the DSP it owns;
//! - the `run` body, where mono/stereo and clamp ranges genuinely differ.
//!
//! Reference: <https://www.ladspa.org/ladspa_sdk/ladspa.h.txt>.
//!
//! # Safety
//! The C-ABI types here are `Sync` because the static `Descriptor` and
//! the `*const c_char` arrays of port names are read-only after init —
//! LADSPA hosts only read them and never mutate. The wrappers must keep
//! the underlying `&[u8]` byte literals alive for the lifetime of the
//! library, which `static` storage already guarantees.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

pub use std::ffi::c_ulong;
pub use std::os::raw::{c_char, c_void};

// LADSPA flag constants -----------------------------------------------------

pub const LADSPA_PROPERTY_HARD_RT_CAPABLE: i32 = 0x4;

pub const LADSPA_PORT_INPUT: i32 = 0x1;
pub const LADSPA_PORT_OUTPUT: i32 = 0x2;
pub const LADSPA_PORT_CONTROL: i32 = 0x4;
pub const LADSPA_PORT_AUDIO: i32 = 0x8;

pub const LADSPA_HINT_BOUNDED_BELOW: i32 = 0x1;
pub const LADSPA_HINT_BOUNDED_ABOVE: i32 = 0x2;
pub const LADSPA_HINT_TOGGLED: i32 = 0x4;
pub const LADSPA_HINT_DEFAULT_0: i32 = 0x200;
pub const LADSPA_HINT_DEFAULT_LOW: i32 = 0x80;
pub const LADSPA_HINT_DEFAULT_MIDDLE: i32 = 0xC0;
pub const LADSPA_HINT_DEFAULT_HIGH: i32 = 0x100;
pub const LADSPA_HINT_DEFAULT_MAXIMUM: i32 = 0x140;

// LADSPA C structs ----------------------------------------------------------

#[repr(C)]
pub struct PortRangeHint {
    pub hint_descriptor: i32,
    pub lower_bound: f32,
    pub upper_bound: f32,
}

#[repr(C)]
pub struct Descriptor {
    pub unique_id: c_ulong,
    pub label: *const c_char,
    pub properties: i32,
    pub name: *const c_char,
    pub maker: *const c_char,
    pub copyright: *const c_char,
    pub port_count: c_ulong,
    pub port_descriptors: *const i32,
    pub port_names: *const *const c_char,
    pub port_range_hints: *const PortRangeHint,
    pub impl_data: *mut c_void,
    pub instantiate: Option<unsafe extern "C" fn(*const Descriptor, c_ulong) -> *mut c_void>,
    pub connect_port: Option<unsafe extern "C" fn(*mut c_void, c_ulong, *mut f32)>,
    pub activate: Option<unsafe extern "C" fn(*mut c_void)>,
    pub run: Option<unsafe extern "C" fn(*mut c_void, c_ulong)>,
    pub run_adding: Option<unsafe extern "C" fn(*mut c_void, c_ulong)>,
    pub set_run_adding_gain: Option<unsafe extern "C" fn(*mut c_void, f32)>,
    pub deactivate: Option<unsafe extern "C" fn(*mut c_void)>,
    pub cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
}

// SAFETY: LADSPA `Descriptor` is read-only after static init; hosts only
// read it. The pointers it carries reference `static` byte literals.
unsafe impl Sync for Descriptor {}

/// `[*const c_char; N]` wrapped so the array can live in a `static`
/// without each wrapper redeclaring its own `unsafe impl Sync`.
#[repr(transparent)]
pub struct CCharPtrArray<const N: usize>(pub [*const c_char; N]);

// SAFETY: the underlying byte literals live in `static` storage and are
// never mutated.
unsafe impl<const N: usize> Sync for CCharPtrArray<N> {}

/// Audio ports do not carry hint information — LADSPA hosts ignore the
/// range data on `LADSPA_PORT_AUDIO` ports. Use this for every audio
/// port hint slot.
pub const NO_HINT: PortRangeHint = PortRangeHint {
    hint_descriptor: 0,
    lower_bound: 0.0,
    upper_bound: 0.0,
};

// Helpers used inside `run` --------------------------------------------------

/// Read a control port, falling back to `default` if the host hasn't
/// connected it yet (the spec allows `connect_port` to set null).
///
/// # Safety
/// `ptr` must be either null or a pointer the host gave us through
/// `connect_port` and is keeping alive for the duration of `run`.
#[inline]
pub unsafe fn read_control(ptr: *const f32, default: f32) -> f32 {
    if ptr.is_null() {
        default
    } else {
        unsafe { *ptr }
    }
}

/// Helper for the `#[no_mangle] pub extern "C" fn ladspa_descriptor`
/// entry point. The wrapper still needs to declare the export itself
/// (cdylib symbols can't be re-exported from a dependency), but this
/// hides the index/null bookkeeping.
///
/// # Safety
/// `descriptor` must point to a `'static` `Descriptor` whose fields all
/// reference data that lives for the program's lifetime.
#[inline]
pub unsafe fn descriptor_or_null(index: c_ulong, descriptor: &'static Descriptor) -> *const c_void {
    if index == 0 {
        descriptor as *const Descriptor as *const c_void
    } else {
        std::ptr::null()
    }
}
