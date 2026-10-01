//! Headless FlyXTogether peer for development.
//!
//! `host` plays the authority with a synthetic flight; `join` is a logging
//! follower that reports how many samples arrive. Together with one
//! X-Plane, either role of a session can be tested on a single machine.

use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use flyx_net::{LocalInfo, NetCommand, NetEvent, NetHandle};
use flyx_protocol::{AircraftId, ByeReason, FlightState};
use flyx_sync::Password;
use flyx_sync::session::{Effect, Event, Notice, Session};
use flyx_sync::trajectory::{Kind, Trajectory};
use tracing::{info, warn};

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
    /// Aircraft file to claim (must match the other seat).
    #[arg(long, global = true, default_value = "Cessna_172SP.acf")]
    acf: String,
    /// Leave the session gracefully after this many seconds.
    #[arg(long, global = true)]
    leave_after: Option<f64>,
}

#[derive(Subcommand)]
enum Command {
    /// Host a session and fly a synthetic trajectory as the authority.
    Host {
        #[arg(long, default_value_t = flyx_net::DEFAULT_PORT)]
        port: u16,
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
        /// End the session this many seconds after the crew joins.
        #[arg(long)]
        end_after_join: Option<f64>,
        /// How the session ends: `graceful` says goodbye, `vanish` exits
        /// without a word (the follower sees a lost connection).
        #[arg(long, value_enum, default_value_t = EndMode::Graceful)]
        end_mode: EndMode,
    },
    /// Join a session and log the flight state as a follower.
    Join {
        /// Host address, e.g. 127.0.0.1 or 203.0.113.7:49700.
        address: String,
    },
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
    let local = LocalInfo {
        plugin_version: env!("CARGO_PKG_VERSION").to_owned(),
        display_name: cli.name.clone(),
        aircraft: AircraftId {
            folder: "Cessna 172 SP".into(),
            acf: cli.acf.clone(),
            name: String::new(),
        },
        definition: [0; 32],
    };
    let net = flyx_net::spawn(&tokio::runtime::Handle::current());
    let mut session = Session::new(env!("CARGO_PKG_VERSION"));
    let password = Password::new(cli.password);

    match cli.command {
        Command::Host {
            port,
            flight,
            lat,
            lon,
            elevation,
            rate,
            end_after_join,
            end_mode,
        } => {
            let trajectory = Trajectory {
                kind: match flight {
                    Flight::Circuit => Kind::Circuit,
                    Flight::Taxi => Kind::Taxi,
                    Flight::Parked => Kind::Parked,
                },
                latitude_deg: lat,
                longitude_deg: lon,
                ground_elevation_m: elevation,
            };
            let event = Event::HostRequested {
                port,
                aircraft: local.aircraft.clone(),
            };
            apply(&mut session, event, &net, &password, &local, None);
            let end = end_after_join.map(|s| (Duration::from_secs_f64(s), end_mode));
            host_loop(session, net, trajectory, rate, deadline, end).await;
        }
        Command::Join { address } => {
            let event = Event::JoinRequested {
                address,
                aircraft: local.aircraft.clone(),
            };
            apply(&mut session, event, &net, &password, &local, None);
            join_loop(session, net, deadline).await;
        }
    }
}

/// Feeds an event into the session and carries out its effects. Returns
/// whether streaming (authority) or following (follower) changed.
fn apply(
    session: &mut Session,
    event: Event,
    net: &NetHandle,
    password: &Password,
    local: &LocalInfo,
    mut active: Option<&mut bool>,
) {
    let outcome = session.handle(event);
    if let Some(notice) = outcome.notice {
        match notice {
            Notice::Info(text) => info!("{text}"),
            Notice::Error(text) => warn!("{text}"),
        }
    }
    for effect in outcome.effects {
        match effect {
            Effect::StartHost { port } => net.send(NetCommand::Host {
                port,
                password: password.clone(),
                local: local.clone(),
            }),
            Effect::Join { address } => net.send(NetCommand::Join {
                address,
                password: password.clone(),
                local: local.clone(),
            }),
            Effect::Disconnect { reason } => net.send(NetCommand::Disconnect { reason }),
            Effect::StartStreaming | Effect::StartFollowing => {
                if let Some(a) = active.as_deref_mut() {
                    *a = true;
                }
            }
            Effect::StopStreaming | Effect::StopFollowing => {
                if let Some(a) = active.as_deref_mut() {
                    *a = false;
                }
            }
        }
    }
    info!(state = ?session.state(), "session");
}

async fn host_loop(
    mut session: Session,
    net: NetHandle,
    trajectory: Trajectory,
    rate: f64,
    deadline: Option<tokio::time::Instant>,
    end: Option<(Duration, EndMode)>,
) {
    let mut streaming = false;
    let mut joined_at: Option<Instant> = None;
    let start = Instant::now();
    let mut seq = 0u32;
    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / rate.max(1.0)));
    let password = Password::default();
    let local = dummy_local();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                while let Some(event) = net.try_event() {
                    if let NetEvent::Session(e) = event {
                        apply(&mut session, e, &net, &password, &local, Some(&mut streaming));
                    }
                }
                if streaming && joined_at.is_none() {
                    joined_at = Some(Instant::now());
                } else if !streaming {
                    joined_at = None;
                }
                if let (Some((after, mode)), Some(at)) = (end, joined_at)
                    && at.elapsed() >= after
                {
                    if mode == EndMode::Vanish {
                        info!("vanishing without a goodbye");
                        std::process::exit(0);
                    }
                    info!("ending the session");
                    stop(net, ByeReason::StoppedHosting).await;
                    return;
                }
                if streaming {
                    let state = trajectory.state_at(start.elapsed().as_secs_f64(), seq);
                    seq = seq.wrapping_add(1);
                    net.send(NetCommand::SendFlightState(state));
                }
                if *session.state() == flyx_sync::session::State::Idle {
                    info!("hosting ended");
                    return;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("stopping");
                stop(net, ByeReason::StoppedHosting).await;
                return;
            }
            _ = sleep_until(deadline) => {
                info!("time is up, stopping");
                stop(net, ByeReason::StoppedHosting).await;
                return;
            }
        }
    }
}

async fn join_loop(mut session: Session, net: NetHandle, deadline: Option<tokio::time::Instant>) {
    let mut following = false;
    let mut stats = Stats::default();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut report = Instant::now();
    let password = Password::default();
    let local = dummy_local();
    loop {
        tokio::select! {
            _ = tick.tick() => {
                while let Some(event) = net.try_event() {
                    match event {
                        NetEvent::Session(e) => {
                            apply(&mut session, e, &net, &password, &local, Some(&mut following));
                        }
                        NetEvent::Paused(p) => info!(paused = p, "authority pause state"),
                    }
                }
                while let Some(sample) = net.try_sample() {
                    stats.add(&sample.state, sample.received_at);
                }
                if report.elapsed() >= Duration::from_secs(1) {
                    if following {
                        stats.report(report.elapsed());
                    }
                    stats = Stats { last: stats.last, ..Stats::default() };
                    report = Instant::now();
                }
                if !following && *session.state() == flyx_sync::session::State::Idle {
                    info!("session ended");
                    return;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("leaving");
                stop(net, ByeReason::Left).await;
                return;
            }
            _ = sleep_until(deadline) => {
                info!("time is up, leaving");
                stop(net, ByeReason::Left).await;
                return;
            }
        }
    }
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

fn dummy_local() -> LocalInfo {
    // Only needed for effects that start new activities, which do not
    // happen after the initial request.
    LocalInfo {
        plugin_version: String::new(),
        display_name: String::new(),
        aircraft: AircraftId {
            folder: String::new(),
            acf: String::new(),
            name: String::new(),
        },
        definition: [0; 32],
    }
}

/// Per-second receive statistics for the follower.
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
                    "{:.5},{:.5} {:.0} m hdg {:.0}",
                    s.latitude_deg, s.longitude_deg, s.elevation_m, s.psi_deg
                )
            })
            .unwrap_or_default();
        info!(
            "{rate:.1} samples/s, gap {mean:.1} ms, jitter {jitter:.1} ms, dropped {}, stale {}, {position}",
            self.dropped, self.stale
        );
    }
}
