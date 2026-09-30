//! Thin, safe wrappers over the X-Plane plugin SDK (XPLM 4.x).
//!
//! All `unsafe` FFI lives in this crate. Every XPLM call must be made from
//! X-Plane's main thread; the handle types here are deliberately `!Send` so
//! they cannot leave it.

pub mod dataref;
pub mod flight_loop;
pub mod gl;
pub mod guard;
pub mod imgui_window;
pub mod menu;
pub mod msg;
pub mod scenery;
#[allow(unsafe_op_in_unsafe_fn)]
#[rustfmt::skip]
pub mod sys;
pub mod util;
pub mod window;

pub use imgui;

use std::ffi::c_char;
use std::path::PathBuf;

use util::{c_buf_to_string, to_cstring};

/// Size of the scratch buffers handed to XPLM functions that write strings.
/// The SDK asks for at least 256 or 512 bytes depending on the call.
const STRING_BUF: usize = 4096;

/// Writes a line to X-Plane's `Log.txt`. A trailing newline is added if missing.
pub fn debug_string(message: &str) {
    let mut line = message.to_owned();
    if !line.ends_with('\n') {
        line.push('\n');
    }
    let line = to_cstring(&line);
    unsafe { sys::XPLMDebugString(line.as_ptr()) }
}

/// Reloads all plugins, including this one. Takes effect after the current
/// callback returns: X-Plane disables, stops and unloads every plugin, then
/// loads them again as at startup.
pub fn reload_plugins() {
    unsafe { sys::XPLMReloadPlugins() }
}

/// Makes XPLM use native OS paths (instead of legacy HFS paths on macOS).
pub fn enable_native_paths() {
    let feature = to_cstring("XPLM_USE_NATIVE_PATHS");
    unsafe { sys::XPLMEnableFeature(feature.as_ptr(), 1) }
}

/// Full path of this plugin's `.xpl` file.
pub fn my_plugin_file() -> PathBuf {
    let mut path = vec![0u8; STRING_BUF];
    unsafe {
        sys::XPLMGetPluginInfo(
            sys::XPLMGetMyID(),
            std::ptr::null_mut(),
            path.as_mut_ptr() as *mut c_char,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
    PathBuf::from(c_buf_to_string(&path))
}

/// Root folder of the X-Plane installation.
pub fn system_path() -> PathBuf {
    let mut path = vec![0u8; STRING_BUF];
    unsafe { sys::XPLMGetSystemPath(path.as_mut_ptr() as *mut c_char) };
    PathBuf::from(c_buf_to_string(&path))
}

/// X-Plane and XPLM version numbers, e.g. `(12400, 430)`.
pub fn versions() -> (i32, i32) {
    let (mut xplane, mut xplm, mut host) = (0, 0, 0);
    unsafe { sys::XPLMGetVersions(&mut xplane, &mut xplm, &mut host) };
    (xplane, xplm)
}

/// The `.acf` file name and full path of an aircraft; index 0 is the user's.
pub fn aircraft_model(index: i32) -> (String, PathBuf) {
    let mut file = vec![0u8; STRING_BUF];
    let mut path = vec![0u8; STRING_BUF];
    unsafe {
        sys::XPLMGetNthAircraftModel(
            index,
            file.as_mut_ptr() as *mut c_char,
            path.as_mut_ptr() as *mut c_char,
        );
    }
    (
        c_buf_to_string(&file),
        PathBuf::from(c_buf_to_string(&path)),
    )
}
