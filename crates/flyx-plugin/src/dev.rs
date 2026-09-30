//! Developer-only tools, compiled with the `dev` feature. Never shipped.

use flyx_sync::session::{Notice, State};
use tracing::info;

use crate::state::{Enabled, open_window};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevItem {
    TestPanic,
    Reload,
    ReloadTenTimes,
    CycleWindowState,
    OverrideSpike,
}

/// Dev menu entries, appended after the normal ones.
pub const MENU: &[(&str, DevItem)] = &[
    ("Trigger test panic (dev)", DevItem::TestPanic),
    ("Reload plugins (dev)", DevItem::Reload),
    ("Reload plugins 10x (dev)", DevItem::ReloadTenTimes),
    ("Cycle window state (dev)", DevItem::CycleWindowState),
    ("Run override spike, ~18 s (dev)", DevItem::OverrideSpike),
];

/// Remaining automatic reloads. An environment variable is the one piece of
/// state that survives X-Plane unloading and reloading the plugin library,
/// and it disappears with the X-Plane process.
const RELOADS_LEFT_VAR: &str = "FLYX_DEV_RELOADS_LEFT";
/// Frames to run after each enable before reloading again.
const FRAMES_BEFORE_RELOAD: u32 = 90;

pub struct DevState {
    panic_requested: bool,
    reload_in_frames: Option<u32>,
    window_state: usize,
    spike: Option<crate::dev_spike::Spike>,
}

impl Drop for DevState {
    fn drop(&mut self) {
        // Never leave the flight model overridden when the plugin stops.
        if let Some(spike) = self.spike.as_mut() {
            spike.stop();
        }
    }
}

impl DevState {
    pub fn on_enable() -> Self {
        let left = reloads_left();
        let reload_in_frames = if left > 0 {
            info!(left, "dev reload test: reloading again shortly");
            Some(FRAMES_BEFORE_RELOAD)
        } else {
            if std::env::var_os(RELOADS_LEFT_VAR).is_some() {
                info!("dev reload test finished");
                set_reloads_left(None);
            }
            None
        };
        Self {
            panic_requested: false,
            reload_in_frames,
            window_state: 0,
            spike: None,
        }
    }
}

fn reloads_left() -> u32 {
    std::env::var(RELOADS_LEFT_VAR)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn set_reloads_left(value: Option<u32>) {
    // SAFETY: called on X-Plane's main thread from a dev-only code path.
    // Other threads could in theory read the environment at the same time;
    // this is accepted for a developer tool that never ships.
    unsafe {
        match value {
            Some(n) => std::env::set_var(RELOADS_LEFT_VAR, n.to_string()),
            None => std::env::remove_var(RELOADS_LEFT_VAR),
        }
    }
}

pub fn on_frame(dev: &mut DevState, dt: f32) {
    if std::mem::take(&mut dev.panic_requested) {
        panic!("test panic requested from the dev menu");
    }
    if let Some(spike) = dev.spike.as_mut()
        && !spike.frame(dt)
    {
        dev.spike = None;
    }
    match dev.reload_in_frames {
        Some(0) => {
            dev.reload_in_frames = None;
            let left = reloads_left().saturating_sub(1);
            set_reloads_left(Some(left));
            info!(left, "dev reload test: reloading plugins");
            flyx_xplm::reload_plugins();
        }
        Some(n) => dev.reload_in_frames = Some(n - 1),
        None => {}
    }
}

pub fn on_menu(e: &mut Enabled, item: DevItem) {
    match item {
        DevItem::TestPanic => e.dev.panic_requested = true,
        DevItem::Reload => flyx_xplm::reload_plugins(),
        DevItem::ReloadTenTimes => {
            info!("dev reload test: 10 reloads requested");
            set_reloads_left(Some(10));
            e.dev.reload_in_frames = Some(0);
        }
        DevItem::OverrideSpike => {
            if e.dev.spike.is_none() {
                e.dev.spike = crate::dev_spike::Spike::start();
            }
        }
        DevItem::CycleWindowState => {
            let states = preview_states();
            let index = e.dev.window_state % states.len();
            e.dev.window_state += 1;
            let (label, state, notice, failure) = states[index].clone();
            info!(state = label, "dev: previewing window state");
            let mut ui = e.ui.borrow_mut();
            ui.state = state;
            ui.notice = notice;
            ui.failure_preview = failure;
            drop(ui);
            open_window(e);
        }
    }
}

type Preview = (&'static str, State, Option<Notice>, Option<String>);

/// Window states for visual checks. They only change what the window
/// shows; the real session state is restored by the next session event.
fn preview_states() -> Vec<Preview> {
    let hosting = |crew: Option<&str>| State::Hosting {
        port: 49700,
        addresses: vec!["192.168.1.20:49700".into(), "[2001:db8::20]:49700".into()],
        crew: crew.map(Into::into),
    };
    vec![
        ("idle", State::Idle, None, None),
        (
            "idle, wrong password",
            State::Idle,
            Some(Notice::Error("Wrong password".into())),
            None,
        ),
        (
            "starting to host",
            State::StartingHost { port: 49700 },
            None,
            None,
        ),
        ("hosting", hosting(None), None, None),
        (
            "hosting, follower left",
            hosting(None),
            Some(Notice::Info("Alex left the session.".into())),
            None,
        ),
        ("connected as authority", hosting(Some("Alex")), None, None),
        (
            "connecting",
            State::Joining {
                address: "203.0.113.7:49700".into(),
            },
            None,
            None,
        ),
        (
            "connected as follower",
            State::Joined {
                address: "203.0.113.7:49700".into(),
                host: "Sam".into(),
            },
            None,
            None,
        ),
        (
            "idle, host unreachable",
            State::Idle,
            Some(Notice::Error(
                "Could not reach the host at 203.0.113.7:49700. Check the address, and ask \
                 the host to check that their UDP port is forwarded to their computer."
                    .into(),
            )),
            None,
        ),
        (
            "failed",
            State::Idle,
            None,
            Some("flight loop: test panic (preview only)".into()),
        ),
    ]
}
