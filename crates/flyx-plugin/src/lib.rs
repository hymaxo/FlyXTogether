//! FlyXTogether X-Plane 12 plugin.
#![allow(non_snake_case)] // the library must be named FlyXTogether for X-Plane

#[cfg(feature = "dev")]
mod dev;
#[cfg(feature = "dev")]
mod dev_spike;
mod logging;
mod paths;
mod settings;
mod state;
mod sync;
mod ui;

use std::ffi::{c_char, c_int, c_void};

use flyx_xplm::guard;
use flyx_xplm::msg::Message;
use flyx_xplm::util::write_c_buf;
use tracing::info;

use logging::XPLANE_LOG;

const NAME: &str = "FlyXTogether";
const SIGNATURE: &str = "org.flyxtogether.plugin";
const DESCRIPTION: &str = "Open-source shared cockpit for X-Plane 12.";
/// The plugin version. Nightly builds set `FLYX_VERSION` to include the
/// commit they were built from.
pub(crate) const VERSION: &str = match option_env!("FLYX_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};
/// XPluginStart output buffers are at least this large.
const XPLM_STRING_CAPACITY: usize = 256;

/// # Safety
/// Called by X-Plane with three writable buffers of at least 256 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn XPluginStart(
    out_name: *mut c_char,
    out_signature: *mut c_char,
    out_description: *mut c_char,
) -> c_int {
    guard::guard_always("XPluginStart", 0, || {
        unsafe {
            write_c_buf(out_name, XPLM_STRING_CAPACITY, NAME);
            write_c_buf(out_signature, XPLM_STRING_CAPACITY, SIGNATURE);
            write_c_buf(out_description, XPLM_STRING_CAPACITY, DESCRIPTION);
        }
        flyx_xplm::enable_native_paths();
        guard::reset();
        guard::install_panic_hook();

        let root = paths::plugin_root(&flyx_xplm::my_plugin_file());
        logging::start(&root.join("FlyXTogether.log"));
        let (xplane, xplm) = flyx_xplm::versions();
        info!(
            target: XPLANE_LOG,
            "FlyXTogether {VERSION} started ({} {}, X-Plane {xplane}, XPLM {xplm}), log: {}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            root.join("FlyXTogether.log").display(),
        );
        1
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn XPluginStop() {
    guard::guard_always("XPluginStop", (), || {
        info!("plugin stopped");
        logging::stop();
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn XPluginEnable() -> c_int {
    guard::guard("XPluginEnable", 0, || {
        let root = paths::plugin_root(&flyx_xplm::my_plugin_file());
        let ok = state::enable(root);
        info!(ok, "plugin enabled");
        ok as c_int
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn XPluginDisable() {
    guard::guard_always("XPluginDisable", (), || {
        state::disable();
        info!("plugin disabled");
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn XPluginReceiveMessage(from: c_int, message: c_int, param: *mut c_void) {
    guard::guard("XPluginReceiveMessage", (), || {
        state::on_message(Message::from_raw(from, message, param));
    })
}
