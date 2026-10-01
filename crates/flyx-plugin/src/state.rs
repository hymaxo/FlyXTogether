//! Everything the plugin owns while enabled, and the glue between the
//! window, the session state machine and the network task.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use flyx_net::{LocalInfo, NetCommand, NetEvent, NetHandle};
use flyx_protocol::{AircraftId, ByeReason, Seat};
use flyx_sync::Password;
use flyx_sync::aircraft;
use flyx_sync::session::{Effect, Event, Outcome, Session, State};
use flyx_xplm::command::{Command, CommandHandler, Phase as CommandPhase};
use flyx_xplm::dataref::DataRef;
use flyx_xplm::flight_loop::{FlightLoop, NextCall, Phase, Tick};
use flyx_xplm::guard;
use flyx_xplm::imgui_window::ImguiWindow;
use flyx_xplm::menu::PluginsMenu;
use flyx_xplm::msg::Message;
use tokio::runtime::Runtime;
use tracing::{debug, error, info, warn};

use crate::VERSION;
use crate::cockpit::{self, CockpitSync};
use crate::logging;
use crate::settings::Settings;
use crate::sync::{self, Authority, Follower, Refs};
use crate::ui::{UiAction, UiModel, WINDOW_SIZE};

/// How long `XPluginDisable` waits for the goodbye to reach the peer.
const NET_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(800);
/// How long `XPluginDisable` then waits for remaining network tasks.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

thread_local! {
    static STATE: RefCell<Option<Enabled>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    Open,
    TakeControls,
    #[cfg(feature = "dev")]
    Dev(crate::dev::DevItem),
}

pub(crate) struct Enabled {
    _menu: PluginsMenu,
    menu_items: Vec<MenuItem>,
    frame_loop: FlightLoop,
    runtime: Option<Runtime>,
    net: Option<NetHandle>,
    session: Session,
    /// Password of the current or last session request (memory only).
    password: Password,
    aircraft: AircraftId,
    /// The loaded aircraft's cockpit sync.
    cockpit: CockpitSync,
    /// Where the optional aircraft profiles live.
    profiles_dir: PathBuf,
    /// Set by the `FlyXTogether/take_controls` command.
    take_controls: Rc<Cell<bool>>,
    _take_controls_command: CommandHandler,
    /// Flight-state datarefs; `None` if one is missing (sync disabled).
    refs: Option<Refs>,
    /// Present while this seat is the authority with crew connected.
    authority: Option<Authority>,
    /// Present while this seat follows the authority.
    follower: Option<Follower>,
    /// Whether a session was connected after the last outcome.
    session_connected: bool,
    frame_stats: FrameStats,
    window: ImguiWindow,
    pub(crate) ui: Rc<RefCell<UiModel>>,
    vr_enabled: Option<DataRef<i32>>,
    settings: Settings,
    settings_path: PathBuf,
    settings_loaded: Option<mpsc::Receiver<Settings>>,
    #[cfg(feature = "dev")]
    pub(crate) dev: crate::dev::DevState,
}

/// Called from `XPluginEnable`. Returns whether enabling succeeded.
pub fn enable(plugin_root: PathBuf) -> bool {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("flyx-net")
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            error!(%err, "cannot start network runtime");
            return false;
        }
    };
    info!("network runtime started");

    let net = flyx_net::spawn(runtime.handle());
    let commander = net.commander();
    guard::set_teardown(move || {
        // Runs once after the first caught panic: give the aircraft back to
        // X-Plane and tell the peer.
        sync::emergency_release();
        cockpit::release_overrides();
        commander.send(NetCommand::Disconnect {
            reason: ByeReason::InternalError,
        });
    });
    let refs = match Refs::find() {
        Ok(refs) => Some(refs),
        Err(e) => {
            error!(%e, "flight-state sync unavailable");
            None
        }
    };

    // Settings are read on a worker thread; the form fills in when ready.
    let settings_path = plugin_root.join("settings.toml");
    let (tx, rx) = mpsc::channel();
    let path = settings_path.clone();
    runtime.spawn_blocking(move || {
        let settings = Settings::load(&path).unwrap_or_else(|err| {
            warn!(%err, path = %path.display(), "cannot read settings, using defaults");
            Settings::default()
        });
        let _ = tx.send(settings);
    });

    let log_path = plugin_root.join("FlyXTogether.log").display().to_string();
    let ui = Rc::new(RefCell::new(UiModel::new(log_path)));
    let window_ui = ui.clone();
    let window = ImguiWindow::new("FlyXTogether", WINDOW_SIZE.0, WINDOW_SIZE.1, move |imgui| {
        let mut model = window_ui.borrow_mut();
        model.failure = guard::failure();
        model.draw(imgui);
    });
    window
        .window()
        .set_resizing_limits((320, 240), (1200, 1000));

    let mut menu = PluginsMenu::new("FlyXTogether", on_menu);
    let mut menu_items = Vec::new();
    menu.add_item("Open");
    menu_items.push(MenuItem::Open);
    menu.add_item("Take controls");
    menu_items.push(MenuItem::TakeControls);

    // A command users can bind to a key or joystick button. The handler
    // only sets a flag: it may run while the plugin state is borrowed.
    let take_controls: Rc<Cell<bool>> = Rc::default();
    let flag = take_controls.clone();
    let take_controls_command = CommandHandler::register(
        Command::create(
            "FlyXTogether/take_controls",
            "FlyXTogether: take the flight controls",
        ),
        true,
        move |phase| {
            if phase == CommandPhase::Begin {
                flag.set(true);
            }
            false
        },
    );

    let profiles_dir = plugin_root.join("profiles");
    let aircraft = current_aircraft();
    let cockpit = load_cockpit(&aircraft, &profiles_dir);
    ui.borrow_mut().untested_aircraft = !cockpit.verified();
    #[cfg(feature = "dev")]
    for (label, item) in crate::dev::MENU {
        menu.add_item(label);
        menu_items.push(MenuItem::Dev(*item));
    }

    let frame_loop = FlightLoop::new(Phase::AfterFlightModel, on_frame);
    frame_loop.schedule(NextCall::Frames(1));

    STATE.with(|s| {
        *s.borrow_mut() = Some(Enabled {
            _menu: menu,
            menu_items,
            frame_loop,
            runtime: Some(runtime),
            net: Some(net),
            session: Session::new(VERSION),
            password: Password::default(),
            aircraft,
            cockpit,
            profiles_dir,
            take_controls,
            _take_controls_command: take_controls_command,
            refs,
            authority: None,
            follower: None,
            session_connected: false,
            frame_stats: FrameStats::default(),
            window,
            ui,
            vr_enabled: DataRef::find("sim/graphics/VR/enabled"),
            settings: Settings::default(),
            settings_path,
            settings_loaded: Some(rx),
            #[cfg(feature = "dev")]
            dev: crate::dev::DevState::on_enable(),
        })
    });
    true
}

/// Called from `XPluginDisable`. Ends any session and stops everything
/// `enable` started.
pub fn disable() {
    let Some(mut enabled) = STATE.with(|s| s.borrow_mut().take()) else {
        return;
    };
    let outcome = enabled.session.handle(Event::PluginStopping);
    apply(&mut enabled, outcome);
    guard::clear_teardown();
    enabled.frame_loop.schedule(NextCall::Stop);
    if let Some(net) = enabled.net.take() {
        // Blocks briefly so the goodbye reaches the peer and the port is
        // released before the runtime stops.
        net.shutdown(ByeReason::PluginStopped, NET_SHUTDOWN_TIMEOUT);
    }
    if let Some(runtime) = enabled.runtime.take() {
        runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        info!("network runtime stopped");
    }
    // Dropping `enabled` destroys the window and flight loop and removes
    // the menu.
    drop(enabled);
}

/// Called from `XPluginReceiveMessage`.
pub fn on_message(message: Message) {
    match message {
        Message::PlaneLoaded(0) => with_state(|e| {
            // A new or reloaded aircraft ends any session (spec: session).
            let outcome = e.session.handle(Event::AircraftChanged);
            apply(e, outcome);
            e.aircraft = current_aircraft();
            e.cockpit = load_cockpit(&e.aircraft, &e.profiles_dir);
            e.ui.borrow_mut().untested_aircraft = !e.cockpit.verified();
        }),
        Message::EnteredVr => with_state(|e| {
            info!("entered VR");
            if e.window.window().is_visible() {
                e.window.window().set_vr(true);
            }
        }),
        Message::ExitingVr => with_state(|e| {
            info!("exiting VR");
            if e.window.window().is_in_vr() {
                e.window.window().set_vr(false);
            }
        }),
        _ => {}
    }
}

fn with_state(f: impl FnOnce(&mut Enabled)) {
    STATE.with(|s| {
        if let Some(enabled) = s.borrow_mut().as_mut() {
            f(enabled);
        }
    });
}

fn on_frame(tick: Tick) -> NextCall {
    logging::flush_xplane_queue();
    let started = Instant::now();
    with_state(|e| {
        if let Some(rx) = &e.settings_loaded
            && let Ok(settings) = rx.try_recv()
        {
            let mut ui = e.ui.borrow_mut();
            ui.form.name = settings.display_name.clone();
            ui.form.host_port = settings.port.to_string();
            ui.form.join_address = settings.last_address.clone();
            drop(ui);
            e.settings = settings;
            e.settings_loaded = None;
        }

        let actions = e.ui.borrow_mut().take_actions();
        for action in actions {
            handle_action(e, action);
        }
        if e.take_controls.take() {
            handle_action(e, UiAction::TakeControls);
        }

        while let Some(event) = e.net.as_ref().and_then(|n| n.try_event()) {
            match event {
                NetEvent::Session(event) => {
                    info!(?event, "network event");
                    let outcome = e.session.handle(event);
                    apply(e, outcome);
                }
                NetEvent::Paused(paused) => {
                    info!(paused, "pilot flying pause state");
                    if let (Some(f), Some(refs)) = (e.follower.as_mut(), &e.refs) {
                        f.set_paused(paused, refs);
                    }
                }
                NetEvent::Cockpit(message) => {
                    if let Some(net) = &e.net {
                        e.cockpit.receive(message, net);
                    }
                }
                NetEvent::Systems {
                    epoch,
                    state,
                    repair,
                } => {
                    let current = e.session.controls().map(|c| c.epoch);
                    if e.follower.is_some()
                        && current == Some(epoch)
                        && let Some(net) = &e.net
                    {
                        e.cockpit.systems(&state, &repair, net);
                    } else {
                        debug!(epoch, "systems state ignored");
                    }
                }
            }
        }
        // Samples are only used while following; otherwise they are dropped.
        while let Some(sample) = e.net.as_ref().and_then(|n| n.try_sample()) {
            if let Some(f) = e.follower.as_mut() {
                f.push(sample);
            }
        }
        if let Some(refs) = &e.refs {
            if let (Some(a), Some(net)) = (e.authority.as_mut(), e.net.as_ref()) {
                a.frame(refs, tick.since_last_call, net, e.cockpit.sample_inputs());
            }
            if let Some(f) = e.follower.as_mut() {
                if let Some(pose) = f.frame(refs) {
                    e.cockpit.write_inputs(&pose.controls);
                }
                for (engine, begin) in f.take_starter_requests() {
                    e.cockpit.run_starter(engine, begin);
                }
            }
        }
        e.cockpit.tick();
        if e.session.controls().is_some()
            && let Some(net) = &e.net
        {
            let flying = e.authority.as_ref().map(|a| a.epoch());
            e.cockpit.frame(flying, net);
        }

        #[cfg(feature = "dev")]
        crate::dev::on_frame(&mut e.dev, tick.since_last_call);

        let syncing = e.authority.is_some() || e.follower.is_some();
        e.frame_stats.record(started.elapsed(), syncing);
    });
    NextCall::Frames(1)
}

/// Per-frame callback time while syncing, logged every 30 s.
#[derive(Default)]
struct FrameStats {
    frames: u32,
    total: Duration,
    worst: Duration,
    since: Option<Instant>,
}

impl FrameStats {
    const PERIOD: Duration = Duration::from_secs(30);

    fn record(&mut self, took: Duration, syncing: bool) {
        if !syncing {
            *self = Self::default();
            return;
        }
        let since = *self.since.get_or_insert_with(Instant::now);
        self.frames += 1;
        self.total += took;
        self.worst = self.worst.max(took);
        if since.elapsed() >= Self::PERIOD {
            let average = self.total / self.frames.max(1);
            info!(
                frames = self.frames,
                average_us = average.as_micros() as u64,
                worst_us = self.worst.as_micros() as u64,
                "plugin frame time while syncing"
            );
            *self = Self::default();
        }
    }
}

fn handle_action(e: &mut Enabled, action: UiAction) {
    let event = match action {
        UiAction::Host {
            port,
            password,
            name,
        } => {
            e.settings.port = port;
            e.settings.display_name = name;
            save_settings(e);
            e.password = password;
            Event::HostRequested {
                port,
                aircraft: e.aircraft.clone(),
            }
        }
        UiAction::Join {
            address,
            password,
            name,
        } => {
            e.settings.last_address = address.clone();
            e.settings.display_name = name;
            save_settings(e);
            e.password = password;
            Event::JoinRequested {
                address,
                aircraft: e.aircraft.clone(),
            }
        }
        UiAction::Leave => Event::LeaveRequested,
        UiAction::TakeControls => Event::TakeControlsRequested,
    };
    info!(?event, "user request");
    let outcome = e.session.handle(event);
    apply(e, outcome);
}

/// Carries out a session outcome and updates the window.
fn apply(e: &mut Enabled, outcome: Outcome) {
    let Outcome {
        effects,
        notice,
        clear_notice,
    } = outcome;
    let was_connected = e.session_connected;
    for effect in effects {
        info!(?effect, "session effect");
        match effect {
            Effect::StartHost { port } => send(
                e,
                NetCommand::Host {
                    port,
                    password: e.password.clone(),
                    local: local_info(e),
                },
            ),
            Effect::Join { address } => send(
                e,
                NetCommand::Join {
                    address,
                    password: e.password.clone(),
                    local: local_info(e),
                },
            ),
            Effect::Disconnect { reason } => send(e, NetCommand::Disconnect { reason }),
            Effect::StartStreaming { epoch } => e.authority = Some(Authority::new(epoch)),
            Effect::StopStreaming => e.authority = None,
            Effect::StartFollowing {
                epoch,
                after_handover,
            } => {
                if let Some(refs) = &e.refs {
                    let headings = e.cockpit.heading_inputs();
                    e.follower = Some(Follower::start(refs, epoch, after_handover, headings));
                }
                e.cockpit.set_monitoring(true);
            }
            Effect::SendControls { controls } => send(e, NetCommand::SendControls(controls)),
            Effect::SendTakeControls { seen_epoch } => {
                send(e, NetCommand::SendTakeControls { seen_epoch })
            }
            Effect::StopFollowing => {
                if let (Some(f), Some(refs)) = (e.follower.take(), &e.refs) {
                    for (engine, begin) in f.release(refs) {
                        e.cockpit.run_starter(engine, begin);
                    }
                }
                e.cockpit.set_monitoring(false);
            }
        }
    }
    // Cockpit sync runs while a session is connected.
    let connected = e.session.controls().is_some();
    if connected && !was_connected {
        let seat = match e.session.state() {
            State::Hosting { .. } => Seat::Host,
            _ => Seat::Crew,
        };
        if let Some(net) = &e.net {
            e.cockpit.connect(seat, net);
        }
    } else if !connected && was_connected {
        e.cockpit.disconnect();
    }
    e.session_connected = connected;
    let mut ui = e.ui.borrow_mut();
    ui.state = e.session.state().clone();
    ui.controls_line = e.session.controls_line();
    ui.can_take_controls = e.session.can_take_controls();
    if let Some(notice) = notice {
        info!(?notice, "notice");
        ui.notice = Some(notice);
    } else if clear_notice {
        ui.notice = None;
    }
}

fn send(e: &Enabled, command: NetCommand) {
    if let Some(net) = &e.net {
        net.send(command);
    }
}

fn local_info(e: &Enabled) -> LocalInfo {
    LocalInfo {
        plugin_version: VERSION.to_owned(),
        display_name: e.settings.display_name.clone(),
        aircraft: e.aircraft.clone(),
        definition: e.cockpit.identity(),
    }
}

fn save_settings(e: &Enabled) {
    let Some(runtime) = &e.runtime else { return };
    let settings = e.settings.clone();
    let path = e.settings_path.clone();
    runtime.spawn_blocking(move || {
        if let Err(err) = settings.save(&path) {
            warn!(%err, path = %path.display(), "cannot save settings");
        }
    });
}

fn on_menu(index: usize) {
    let item = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|e| e.menu_items.get(index).copied())
    });
    match item {
        Some(MenuItem::Open) => with_state(open_window),
        Some(MenuItem::TakeControls) => with_state(|e| handle_action(e, UiAction::TakeControls)),
        #[cfg(feature = "dev")]
        Some(MenuItem::Dev(item)) => with_state(|e| crate::dev::on_menu(e, item)),
        None => {}
    }
}

pub(crate) fn open_window(e: &mut Enabled) {
    let window = e.window.window();
    window.set_visible(true);
    let in_vr = e.vr_enabled.is_some_and(|d| d.get() != 0);
    if in_vr != window.is_in_vr() {
        window.set_vr(in_vr);
    }
}

/// Builds the cockpit sync for the user's aircraft.
fn load_cockpit(aircraft: &AircraftId, profiles_dir: &std::path::Path) -> CockpitSync {
    let (_file, path) = flyx_xplm::aircraft_model(0);
    CockpitSync::load(&path, &aircraft.folder, &aircraft.acf, profiles_dir)
}

/// The user's aircraft as a protocol identity.
fn current_aircraft() -> AircraftId {
    let (_file, path) = flyx_xplm::aircraft_model(0);
    let ui_name = flyx_xplm::dataref::ArrayRef::<u8>::find("sim/aircraft/view/acf_ui_name")
        .map(|r| {
            let mut bytes = vec![0u8; 260];
            let n = r.get(0, &mut bytes);
            bytes.truncate(n);
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        })
        .unwrap_or_default();
    let id = aircraft::identify(&path, &ui_name);
    info!(folder = %id.folder, acf = %id.acf, name = %id.name, "user aircraft");
    id
}
