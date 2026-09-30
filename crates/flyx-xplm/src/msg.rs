//! Inter-plugin messages X-Plane sends to `XPluginReceiveMessage`.

use std::ffi::c_void;

use crate::sys;

/// Messages the plugin reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// An aircraft finished loading; the index is 0 for the user's aircraft.
    PlaneLoaded(i32),
    /// An aircraft is being unloaded.
    PlaneUnloaded(i32),
    /// A different livery was loaded.
    LiveryLoaded(i32),
    EnteredVr,
    ExitingVr,
    Other(i32),
}

impl Message {
    /// Decodes a raw message. Only X-Plane itself (sender 0) is trusted for
    /// the simulator messages; anything else is reported as `Other`.
    pub fn from_raw(from: i32, message: i32, param: *mut c_void) -> Self {
        if from != sys::XPLM_PLUGIN_XPLANE {
            return Message::Other(message);
        }
        let index = param as isize as i32;
        match message {
            sys::XPLM_MSG_PLANE_LOADED => Message::PlaneLoaded(index),
            sys::XPLM_MSG_PLANE_UNLOADED => Message::PlaneUnloaded(index),
            sys::XPLM_MSG_LIVERY_LOADED => Message::LiveryLoaded(index),
            sys::XPLM_MSG_ENTERED_VR => Message::EnteredVr,
            sys::XPLM_MSG_EXITING_VR => Message::ExitingVr,
            other => Message::Other(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_plane_loaded_index() {
        let m = Message::from_raw(0, sys::XPLM_MSG_PLANE_LOADED, std::ptr::null_mut());
        assert_eq!(m, Message::PlaneLoaded(0));
        let m = Message::from_raw(
            0,
            sys::XPLM_MSG_PLANE_LOADED,
            std::ptr::without_provenance_mut(3),
        );
        assert_eq!(m, Message::PlaneLoaded(3));
    }

    #[test]
    fn ignores_messages_from_other_plugins() {
        let m = Message::from_raw(42, sys::XPLM_MSG_PLANE_LOADED, std::ptr::null_mut());
        assert_eq!(m, Message::Other(sys::XPLM_MSG_PLANE_LOADED));
    }
}
