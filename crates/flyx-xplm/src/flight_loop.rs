//! Flight-loop callbacks (X-Plane's per-frame hook).

use std::ffi::{c_int, c_void};

use crate::guard;
use crate::sys;

/// When in the frame the callback runs relative to the flight model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    BeforeFlightModel,
    AfterFlightModel,
}

/// When the callback should run next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NextCall {
    /// Stop calling until rescheduled.
    Stop,
    /// After this many frames (1 = every frame).
    Frames(u32),
    /// After this many seconds.
    Seconds(f32),
}

impl NextCall {
    fn to_xplm(self) -> f32 {
        match self {
            NextCall::Stop => 0.0,
            NextCall::Frames(n) => -(n.max(1) as f32),
            NextCall::Seconds(s) => s.max(f32::MIN_POSITIVE),
        }
    }
}

/// Timing information passed to each callback.
#[derive(Debug, Clone, Copy)]
pub struct Tick {
    /// Wall-clock seconds since this callback last ran.
    pub since_last_call: f32,
    /// Wall-clock seconds since the last flight loop of any kind.
    pub since_last_loop: f32,
    /// Monotonic flight-loop counter.
    pub counter: i32,
}

type Callback = Box<dyn FnMut(Tick) -> NextCall>;

/// An owned flight loop. It is destroyed when dropped. Main thread only.
pub struct FlightLoop {
    id: sys::XPLMFlightLoopID,
    // Double box: the outer box gives the callback a stable address to pass
    // to X-Plane as the refcon.
    _callback: Box<Callback>,
}

impl FlightLoop {
    /// Creates a flight loop. It does not run until [`FlightLoop::schedule`].
    pub fn new(phase: Phase, callback: impl FnMut(Tick) -> NextCall + 'static) -> Self {
        let mut callback: Box<Callback> = Box::new(Box::new(callback));
        let mut params = sys::XPLMCreateFlightLoop_t {
            structSize: std::mem::size_of::<sys::XPLMCreateFlightLoop_t>() as c_int,
            phase: match phase {
                Phase::BeforeFlightModel => sys::xplm_FlightLoop_Phase_BeforeFlightModel as i32,
                Phase::AfterFlightModel => sys::xplm_FlightLoop_Phase_AfterFlightModel as i32,
            },
            callbackFunc: Some(trampoline),
            refcon: callback.as_mut() as *mut Callback as *mut c_void,
        };
        let id = unsafe { sys::XPLMCreateFlightLoop(&mut params) };
        Self {
            id,
            _callback: callback,
        }
    }

    /// Schedules the next call, relative to now.
    pub fn schedule(&self, next: NextCall) {
        unsafe { sys::XPLMScheduleFlightLoop(self.id, next.to_xplm(), 1) }
    }
}

impl Drop for FlightLoop {
    fn drop(&mut self) {
        unsafe { sys::XPLMDestroyFlightLoop(self.id) }
    }
}

unsafe extern "C" fn trampoline(
    since_last_call: f32,
    since_last_loop: f32,
    counter: c_int,
    refcon: *mut c_void,
) -> f32 {
    // A failed plugin stops all its flight loops.
    guard::guard("flight loop", 0.0, || {
        let callback = unsafe { &mut *(refcon as *mut Callback) };
        callback(Tick {
            since_last_call,
            since_last_loop,
            counter,
        })
        .to_xplm()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_call_encoding_matches_xplm_convention() {
        assert_eq!(NextCall::Stop.to_xplm(), 0.0);
        assert_eq!(NextCall::Frames(1).to_xplm(), -1.0);
        assert_eq!(NextCall::Frames(0).to_xplm(), -1.0);
        assert_eq!(NextCall::Frames(3).to_xplm(), -3.0);
        assert_eq!(NextCall::Seconds(0.5).to_xplm(), 0.5);
        assert!(NextCall::Seconds(0.0).to_xplm() > 0.0);
    }
}
