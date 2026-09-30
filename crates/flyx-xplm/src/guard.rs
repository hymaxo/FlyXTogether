//! Panic containment for every FFI entry point.
//!
//! A panic must never unwind into X-Plane. [`guard`] runs a closure under
//! `catch_unwind`; on panic it logs the payload and backtrace, runs the
//! registered emergency teardown once, and latches the plugin into a failed
//! state in which ordinary callbacks become no-ops.

use std::backtrace::Backtrace;
use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static FAILED: AtomicBool = AtomicBool::new(false);
static FAILURE: Mutex<Option<String>> = Mutex::new(None);
static TEARDOWN: Mutex<Option<Box<dyn FnMut() + Send>>> = Mutex::new(None);

thread_local! {
    /// Backtrace captured by the panic hook, picked up by the guard.
    static LAST_BACKTRACE: RefCell<Option<Backtrace>> = const { RefCell::new(None) };
}

/// Installs a panic hook that records a backtrace for [`guard`] instead of
/// printing to stderr (which X-Plane does not show).
pub fn install_panic_hook() {
    panic::set_hook(Box::new(|_info| {
        LAST_BACKTRACE.with(|b| *b.borrow_mut() = Some(Backtrace::force_capture()));
    }));
}

/// Registers the emergency teardown run after the first caught panic, e.g.
/// releasing flight-model overrides and stopping the network runtime.
pub fn set_teardown(teardown: impl FnMut() + Send + 'static) {
    *TEARDOWN.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(teardown));
}

/// Removes the registered teardown (on a clean disable).
pub fn clear_teardown() {
    *TEARDOWN.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Whether a panic has been caught since the plugin started.
pub fn is_failed() -> bool {
    FAILED.load(Ordering::Acquire)
}

/// Human-readable description of the first failure, for the UI.
pub fn failure() -> Option<String> {
    FAILURE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Runs `body` unless the plugin has failed; returns `fallback` if it has
/// failed or if `body` panics. Use for simulator callbacks.
pub fn guard<R>(context: &str, fallback: R, body: impl FnOnce() -> R) -> R {
    if is_failed() {
        return fallback;
    }
    guard_always(context, fallback, body)
}

/// Like [`guard`] but runs even in the failed state. Use for the window and
/// for the lifecycle exports, which must keep working after a failure.
pub fn guard_always<R>(context: &str, fallback: R, body: impl FnOnce() -> R) -> R {
    match panic::catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            on_panic(context, payload_message(payload.as_ref()));
            fallback
        }
    }
}

fn payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

fn on_panic(context: &str, message: String) {
    let backtrace = LAST_BACKTRACE
        .with(|b| b.borrow_mut().take())
        .map(|b| b.to_string())
        .unwrap_or_else(|| "(no backtrace captured)".to_owned());
    tracing::error!(context, %message, %backtrace, "internal error caught");

    let first = !FAILED.swap(true, Ordering::AcqRel);
    if first {
        *FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("{context}: {message}"));
        tracing::error!(
            "FlyXTogether stopped after an internal error and is now inactive; \
             restart X-Plane to use it again (details in FlyXTogether.log)"
        );
        let teardown = TEARDOWN.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut teardown) = teardown
            && panic::catch_unwind(AssertUnwindSafe(&mut teardown)).is_err()
        {
            tracing::error!("emergency teardown itself panicked");
        }
    }
}

/// Clears the failed state. Only meant for a fresh plugin start and tests.
pub fn reset() {
    FAILED.store(false, Ordering::Release);
    *FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    // The guard state is global, so the scenarios run in one test.
    #[test]
    fn panic_returns_fallback_latches_failed_and_runs_teardown_once() {
        reset();
        let teardowns = Arc::new(AtomicUsize::new(0));
        let counter = teardowns.clone();
        set_teardown(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        assert_eq!(guard("ok", 0, || 7), 7);
        assert!(!is_failed());

        let value = guard("flight loop", -1.0f32, || panic!("boom"));
        assert_eq!(value, -1.0);
        assert!(is_failed());
        assert_eq!(failure().as_deref(), Some("flight loop: boom"));
        assert_eq!(teardowns.load(Ordering::SeqCst), 1);

        // Ordinary callbacks are now skipped...
        let mut ran = false;
        assert_eq!(
            guard("after", 3, || {
                ran = true;
                9
            }),
            3
        );
        assert!(!ran);
        // ...but the always-guard still runs, and a second panic does not
        // run the teardown again.
        assert_eq!(guard_always("window", 1, || 2), 2);
        assert_eq!(guard_always("window", 1, || panic!("again")), 1);
        assert_eq!(teardowns.load(Ordering::SeqCst), 1);
        assert_eq!(failure().as_deref(), Some("flight loop: boom"));

        reset();
        clear_teardown();
    }
}
