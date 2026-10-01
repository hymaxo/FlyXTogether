//! Headless FlyXTogether peer for development.
//!
//! `host` or `join` a session. While this peer is the pilot flying it flies
//! a synthetic trajectory; while the other seat flies it logs how many
//! samples arrive. With `--aircraft` it builds the aircraft's sync
//! definition like the plugin does, takes part in cockpit sync, and runs
//! scripted cockpit actions (`--at`). Together with one X-Plane, every role
//! of a session can be tested on a single machine.

mod cockpit;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use flyx_net::{LocalInfo, NetCommand, NetEvent, NetHandle};
use flyx_protocol::{AircraftId, ByeReason, FlightState};
use flyx_sync::Password;
use flyx_sync::session::{Effect, Event, Notice, Session, State};
use flyx_sync::trajectory::{Kind, Trajectory};
use tracing::{info, warn};

use cockpit::{Cockpit, ScriptStep};

#[derive(Parser)]
#[command(version, about = "Headless FlyXTogether peer for development")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Session password.
    #[arg(long, global = true, default_value = "flyx")]
    password: String,
    /// Display name shown to the other seat.
    #[arg(long, global = true, default_value = "flyx-peer")]
    name: String,
    /// Aircraft file to claim when `--aircraft` is not given.
    #[arg(long, global = true, default_value = "Cessna_172SP.acf")]
    acf: String,
    /// Full path of an `.acf` file: build its sync definition like the
    /// plugin does and take part in cockpit sync.
    #[arg(long, global = true)]
    aircraft: Option<PathBuf>,
    /// Engine count of `--aircraft`.
    #[arg(long, global = true, default_value_t = 1)]
    engines: usize,
    /// Folder of profile files for `--aircraft`.
    #[arg(long, global = true, default_value = "profiles")]
    profiles: PathBuf,
    /// Scripted cockpit actions, relative to the session connecting:
    /// `<seconds>:set <dataref[i]>=<value>`, `<seconds>:press <command>`,
    /// `<seconds>:hold <command> <seconds>` or `<seconds>:take`.
    #[arg(long = "at", global = true)]
    script: Vec<String>,
    /// Cockpit values set before connecting, `<dataref[i]>=<value>`; a
    /// hosting peer sends them in the join snapshot.
    #[arg(long, global = true)]
    preset: Vec<String>,
    /// Fly the trajectory as an aircraft with this many running engines.
    #[arg(long, global = true, default_value_t = 1)]
    running_engines: usize,
    /// Retract the gear while airborne.
    #[arg(long, global = true)]
    retractable: bool,
    /// Shortcut for `--at <seconds>:take`.
    #[arg(long, global = true)]
    take_controls_after: Option<f64>,
    /// Leave the session gracefully after this many seconds.
    #[arg(long, global = true)]
    leave_after: Option<f64>,
}

#[derive(Subcommand)]
enum Command {
    /// Host a session; fly a synthetic trajectory while pilot flying.
    Host {
        #[arg(long, default_value_t = flyx_net::DEFAULT_PORT)]
        port: u16,
        #[command(flatten)]
        flight: FlightArgs,
        /// End the session this many seconds after the crew joins.
        #[arg(long)]
        end_after_join: Option<f64>,
        /// How the session ends: `graceful` says goodbye, `vanish` exits
        /// without a word (the other seat sees a lost connection).
        #[arg(long, value_enum, default_value_t = EndMode::Graceful)]
        end_mode: EndMode,
    },
    /// Join a session; fly the trajectory if given the controls.
    Join {
        /// Host address, e.g. 127.0.0.1 or 203.0.113.7:49700.
        address: String,
        #[command(flatten)]
        flight: FlightArgs,
    },
}

#[derive(clap::Args, Clone, Copy)]
struct FlightArgs {
    #[arg(long, value_enum, default_value_t = Flight::Circuit)]
    flight: Flight,
    /// Centre of the trajectory (default: Seattle-Tacoma, KSEA).
    #[arg(long, default_value_t = 47.4480)]
    lat: f64,
    #[arg(long, default_value_t = -122.3088)]
    lon: f64,
    /// Ground elevation at the centre, metres MSL.
    #[arg(long, default_value_t = 132.0)]
    elevation: f64,
    /// Samples per second.
    #[arg(long, default_value_t = 30.0)]
    rate: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EndMode {
    Graceful,
    Vanish,
}

#[derive(Clone, Copy, ValueEnum)]
enum Flight {
    Circuit,
    Taxi,
    Parked,
}

fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(run(cli));
}

async fn run(cli: Cli) {
    let deadline = cli
        .leave_after
        .map(|s| tokio::time::Instant::now() + Duration::from_secs_f64(s));
    let mut script = Vec::new();
    for text in &cli.script {
        match ScriptStep::parse(text) {
            Ok(step) => script.push(step),
            Err(e) => {
                warn!("bad --at {text:?}: {e}");
                return;
            }
        }
    }
    if let Some(t) = cli.take_controls_after {
        script.push(ScriptStep::take(t));
    }
    script.sort_by(|a, b| a.at.total_cmp(&b.at));

    let (aircraft, cockpit) = match &cli.aircraft {
        Some(path) => match Cockpit::load(path, cli.engines, &cli.profiles) {
            Ok(mut c) => {
                for preset in &cli.preset {
                    if let Err(e) = c.preset(preset) {
                        warn!("bad --preset {preset:?}: {e}");
                        return;
                    }
                }
                (c.aircraft.clone(), Some(c))
            }
            Err(e) => {
                warn!("{e}");
                return;
            }
        },
        None => (
            AircraftId {
                folder: "Cessna 172 SP".into(),
                acf: cli.acf.clone(),
                name: String::new(),
            },
            None,
        ),
    };
    let local = LocalInfo {
        plugin_version: env!("CARGO_PKG_VERSION").to_owned(),
        display_name: cli.name.clone(),
        definition: cockpit.as_ref().map_or([0; 32], |c| c.definition.identity),
        aircraft,
    };
    let net = flyx_net::spawn(&tokio::runtime::Handle::current());
    let password = Password::new(cli.password);
    let (request, flight, end, is_host) = match cli.command {
        Command::Host {
            port,
            flight,
            end_after_join,
            end_mode,
        } => (
            Event::HostRequested {
                port,
                aircraft: local.aircraft.clone(),
            },
            flight,
            end_after_join.map(|s| (Duration::from_secs_f64(s), end_mode)),
            true,
        ),
        Command::Join { address, flight } => (
            Event::JoinRequested {
                address,
                aircraft: local.aircraft.clone(),
            },
            flight,
            None,
            false,
        ),
    };
    let mut peer = Peer {
        session: Session::new(env!("CARGO_PKG_VERSION")),
        net,
        password,
        local,
        trajectory: Trajectory {
            kind: match flight.flight {
                Flight::Circuit => Kind::Circuit,
                Flight::Taxi => Kind::Taxi,
                Flight::Parked => Kind::Parked,
            },
            latitude_deg: flight.lat,
            longitude_deg: flight.lon,
            ground_elevation_m: flight.elevation,
        },
        start: Instant::now(),
        seq: 0,
        streaming: None,
        following: false,
        stats: Stats::default(),
        cockpit,
        script,
        connected_at: None,
        is_host,
        following_ended_now: false,
        running_engines: cli.running_engines.clamp(1, flyx_protocol::MAX_ENGINES),
        retractable: cli.retractable,
        last_received: None,
        carry_on: None,
    };
    peer.handle(request);
    peer.run(flight.rate, deadline, end).await;
}

/// The peer's whole state.
struct Peer {
    session: Session,
    net: NetHandle,
    password: Password,
    local: LocalInfo,
    trajectory: Trajectory,
    start: Instant,
    seq: u32,
    /// The control epoch being streamed, while pilot flying.
    streaming: Option<u32>,
    following: bool,
    stats: Stats,
    cockpit: Option<Cockpit>,
    script: Vec<ScriptStep>,
    connected_at: Option<Instant>,
    is_host: bool,
    /// Set while handling an event whose effects stopped following.
    following_ended_now: bool,
    running_engines: usize,
    retractable: bool,
    /// The latest sample received while following.
    last_received: Option<FlightState>,
    /// After a handover, the peer flies on straight and level from where
    /// it was following: the sample it started from and when.
    carry_on: Option<(FlightState, f64)>,
}

impl Peer {
    /// Feeds an event into the session and carries out its effects.
    fn handle(&mut self, event: Event) {
        self.following_ended_now = false;
        let outcome = self.session.handle(event);
        if let Some(notice) = outcome.notice {
            match notice {
                Notice::Info(text) => info!("{text}"),
                Notice::Error(text) => warn!("{text}"),
            }
        }
        for effect in outcome.effects {
            match effect {
                Effect::StartHost { port } => self.net.send(NetCommand::Host {
                    port,
                    password: self.password.clone(),
                    local: self.local.clone(),
                }),
                Effect::Join { address } => self.net.send(NetCommand::Join {
                    address,
                    password: self.password.clone(),
                    local: self.local.clone(),
                }),
                Effect::Disconnect { reason } => self.net.send(NetCommand::Disconnect { reason }),
                Effect::SendControls { controls } => {
                    self.net.send(NetCommand::SendControls(controls))
                }
                Effect::SendTakeControls { seen_epoch } => {
                    self.net.send(NetCommand::SendTakeControls { seen_epoch })
                }
                Effect::StartStreaming { epoch } => {
                    self.streaming = Some(epoch);
                    match self.last_received.take() {
                        Some(from) if self.following_ended_now => {
                            info!(epoch, "pilot flying: carrying on from the handover");
                            self.carry_on = Some((from, self.start.elapsed().as_secs_f64()));
                        }
                        _ => info!(epoch, "pilot flying: streaming the trajectory"),
                    }
                }
                Effect::StopStreaming => self.streaming = None,
                Effect::StartFollowing { epoch, .. } => {
                    info!(epoch, "pilot monitoring: following");
                    self.following = true;
                }
                Effect::StopFollowing => {
                    self.following = false;
                    self.following_ended_now = true;
                }
            }
        }
        if let Some(line) = self.session.controls_line() {
            info!("{line}");
        }
        let connected = self.session.controls().is_some();
        match (connected, self.connected_at) {
            (true, None) => {
                self.connected_at = Some(Instant::now());
                if let Some(c) = &mut self.cockpit {
                    c.connected(self.is_host, &self.net);
                }
            }
            (false, Some(_)) => {
                self.connected_at = None;
                if let Some(c) = &mut self.cockpit {
                    c.disconnected();
                }
            }
            _ => {}
        }
    }

    async fn run(
        mut self,
        rate: f64,
        deadline: Option<tokio::time::Instant>,
        end: Option<(Duration, EndMode)>,
    ) {
        let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / rate.max(1.0)));
        let mut report = Instant::now();
        let mut frame = 0u64;
        let reason = loop {
            tokio::select! {
                _ = tick.tick() => {
                    frame += 1;
                    if let Some(reason) = self.tick(frame, end, &mut report) {
                        break reason;
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("stopping");
                    break self.leave_reason();
                }
                _ = sleep_until(deadline) => {
                    info!("time is up, stopping");
                    break self.leave_reason();
                }
            }
        };
        if let Some(c) = &self.cockpit {
            c.log_values();
        }
        if let Some(reason) = reason {
            stop(self.net, reason).await;
        }
    }

    /// The synthetic flight at `now`, as this aircraft would report it.
    fn flight_state(&self, epoch: u32, now: f64) -> FlightState {
        if let Some((from, t0)) = self.carry_on {
            let mut state = carry_on(&from, now - t0);
            state.epoch = epoch;
            state.seq = self.seq;
            state.sim_time = now;
            return state;
        }
        let mut state = self.trajectory.state_at(now, self.seq);
        state.epoch = epoch;
        if !state.on_ground {
            wobble(&mut state, now);
        }
        let v = state.visuals;
        for i in 1..self.running_engines {
            state.visuals.engine_running[i] = v.engine_running[0];
            state.visuals.prop_speed_rad_s[i] = v.prop_speed_rad_s[0];
        }
        if self.retractable && !state.on_ground {
            state.visuals.gear_deploy = [0.0; flyx_protocol::MAX_GEAR];
        }
        if let Some(c) = &self.cockpit {
            state.controls = c.controls(&state);
        }
        state
    }

    fn leave_reason(&self) -> Option<ByeReason> {
        Some(if self.is_host {
            ByeReason::StoppedHosting
        } else {
            ByeReason::Left
        })
    }

    /// One tick. Returns `Some` to stop (with the goodbye to send, if any).
    fn tick(
        &mut self,
        frame: u64,
        end: Option<(Duration, EndMode)>,
        report: &mut Instant,
    ) -> Option<Option<ByeReason>> {
        while let Some(event) = self.net.try_event() {
            match event {
                NetEvent::Session(e) => self.handle(e),
                NetEvent::Paused(p) => info!(paused = p, "pilot flying pause state"),
                NetEvent::Cockpit(message) => {
                    if let Some(c) = &mut self.cockpit {
                        c.receive(
                            message,
                            frame,
                            self.start.elapsed().as_secs_f64(),
                            &self.net,
                        );
                    } else {
                        info!(?message, "cockpit message");
                    }
                }
                NetEvent::Systems {
                    epoch,
                    state,
                    repair,
                } => {
                    if let Some(c) = &mut self.cockpit {
                        c.systems(
                            epoch,
                            &state,
                            &repair,
                            self.start.elapsed().as_secs_f64(),
                            &self.net,
                        );
                    }
                }
            }
        }
        while let Some(sample) = self.net.try_sample() {
            if self.following {
                self.stats.add(&sample.state, sample.received_at);
                self.last_received = Some(sample.state);
            }
        }
        let now = self.start.elapsed().as_secs_f64();
        if let Some(at) = self.connected_at {
            let since = at.elapsed().as_secs_f64();
            while self.script.first().is_some_and(|s| s.at <= since) {
                let step = self.script.remove(0);
                if step.is_take() {
                    info!("script: taking the controls");
                    self.handle(Event::TakeControlsRequested);
                } else if let Some(c) = &mut self.cockpit {
                    c.run(&step, now, &self.net);
                } else {
                    warn!("script step needs --aircraft: {step:?}");
                }
            }
            if let Some((after, mode)) = end
                && at.elapsed() >= after
            {
                if mode == EndMode::Vanish {
                    info!("vanishing without a goodbye");
                    std::process::exit(0);
                }
                info!("ending the session");
                return Some(Some(ByeReason::StoppedHosting));
            }
        }
        let flight = self
            .streaming
            .map(|epoch| (epoch, self.flight_state(epoch, now)));
        if let Some(c) = &mut self.cockpit {
            c.frame(frame, now, flight, &self.net);
        }
        if let Some((_, state)) = flight {
            self.seq = self.seq.wrapping_add(1);
            self.net.send(NetCommand::SendFlightState(state));
        }
        if report.elapsed() >= Duration::from_secs(1) {
            if self.following {
                self.stats.report(report.elapsed());
            }
            self.stats = Stats {
                last: self.stats.last,
                ..Stats::default()
            };
            *report = Instant::now();
        }
        if *self.session.state() == State::Idle {
            info!("session ended");
            return Some(None);
        }
        None
    }
}

/// `from` moved on for `dt` seconds, straight and level.
/// Rolls and pitches gently around the circuit's steady attitude, so the
/// other seat's attitude indicator and yoke visibly move.
fn wobble(s: &mut FlightState, t: f64) {
    use std::f64::consts::TAU;
    let (roll_amp, roll_period) = (12.0, 14.0);
    let (pitch_amp, pitch_period) = (4.0, 9.0);
    let roll = TAU * t / roll_period;
    let pitch = TAU * t / pitch_period;
    s.phi_deg += (roll_amp * roll.sin()) as f32;
    s.theta_deg += (pitch_amp * pitch.sin()) as f32;
    s.rates_deg[0] += (roll_amp * TAU / roll_period * roll.cos()) as f32;
    s.rates_deg[1] += (pitch_amp * TAU / pitch_period * pitch.cos()) as f32;
}

fn carry_on(from: &FlightState, dt: f64) -> FlightState {
    const EARTH_RADIUS_M: f64 = 6_371_000.0;
    let mut s = *from;
    let east = from.velocity[0] as f64 * dt;
    let north = -(from.velocity[2] as f64) * dt;
    s.latitude_deg += (north / EARTH_RADIUS_M).to_degrees();
    s.longitude_deg +=
        (east / (EARTH_RADIUS_M * from.latitude_deg.to_radians().cos())).to_degrees();
    s.velocity[1] = 0.0;
    s.acceleration = [0.0; 3];
    s.rates_deg = [0.0; 3];
    s.phi_deg = 0.0;
    s
}

/// Sleeps until `deadline`, or forever without one.
async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

async fn stop(net: NetHandle, reason: ByeReason) {
    tokio::task::spawn_blocking(move || net.shutdown(reason, Duration::from_secs(1)))
        .await
        .ok();
}

/// Per-second receive statistics while following.
#[derive(Default)]
struct Stats {
    count: u32,
    dropped: u32,
    stale: u32,
    last: Option<(u32, Instant)>,
    gaps_ms: Vec<f64>,
    latest: Option<FlightState>,
}

impl Stats {
    fn add(&mut self, state: &FlightState, at: Instant) {
        if let Some((seq, prev_at)) = self.last {
            if state.seq <= seq {
                self.stale += 1;
                return;
            }
            self.dropped += state.seq - seq - 1;
            self.gaps_ms
                .push(at.duration_since(prev_at).as_secs_f64() * 1000.0);
        }
        self.count += 1;
        self.last = Some((state.seq, at));
        self.latest = Some(*state);
    }

    fn report(&self, window: Duration) {
        let rate = self.count as f64 / window.as_secs_f64();
        let (mean, jitter) = if self.gaps_ms.is_empty() {
            (0.0, 0.0)
        } else {
            let mean = self.gaps_ms.iter().sum::<f64>() / self.gaps_ms.len() as f64;
            let var = self.gaps_ms.iter().map(|g| (g - mean).powi(2)).sum::<f64>()
                / self.gaps_ms.len() as f64;
            (mean, var.sqrt())
        };
        let position = self
            .latest
            .map(|s| {
                format!(
                    "{:.5},{:.5} {:.0} m hdg {:.0} epoch {}",
                    s.latitude_deg, s.longitude_deg, s.elevation_m, s.psi_deg, s.epoch
                )
            })
            .unwrap_or_default();
        info!(
            "{rate:.1} samples/s, gap {mean:.1} ms, jitter {jitter:.1} ms, dropped {}, stale {}, {position}",
            self.dropped, self.stale
        );
    }
}
