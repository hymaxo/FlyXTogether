//! Loopback tests of hosting, joining and the handshake (tasks 4.4-4.6).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use flyx_protocol::{
    AircraftId, ByeReason, Control, FlightState, KdfParams, PROTOCOL_VERSION, RejectReason,
};
use flyx_sync::Password;
use flyx_sync::session::{Event, HostFailure, JoinFailure, PeerGone, Refusal};
use tokio::net::UdpSocket;

use super::*;
use crate::control::ControlReader;

fn c172() -> AircraftId {
    AircraftId {
        folder: "Cessna 172 SP".into(),
        acf: "Cessna_172SP.acf".into(),
        name: "Cessna 172 SP".into(),
    }
}

fn seaplane() -> AircraftId {
    AircraftId {
        folder: "Cessna 172 SP".into(),
        acf: "Cessna_172SP_seaplane.acf".into(),
        name: "Cessna 172 SP Seaplane".into(),
    }
}

fn local(name: &str, aircraft: AircraftId) -> LocalInfo {
    LocalInfo {
        plugin_version: "0.1.0".into(),
        display_name: name.into(),
        aircraft,
        definition: [1; 32],
    }
}

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Waits until an event matching `pred` arrives; returns it.
async fn wait_event(
    net: &NetHandle,
    timeout: Duration,
    pred: impl Fn(&NetEvent) -> bool,
) -> NetEvent {
    let deadline = Instant::now() + timeout;
    loop {
        while let Some(event) = net.try_event() {
            if pred(&event) {
                return event;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for an event");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn session_event(event: NetEvent) -> Event {
    match event {
        NetEvent::Session(e) => e,
        other => panic!("expected a session event, got {other:?}"),
    }
}

/// Starts hosting on a fresh port and waits until it listens.
async fn host(password: &str) -> (NetHandle, u16) {
    host_with(password, c172()).await
}

async fn host_with(password: &str, aircraft: AircraftId) -> (NetHandle, u16) {
    let port = free_port();
    let net = spawn(&tokio::runtime::Handle::current());
    net.send(NetCommand::Host {
        port,
        password: Password::new(password),
        local: local("Sam", aircraft),
    });
    wait_event(&net, Duration::from_secs(5), |e| {
        matches!(e, NetEvent::Session(Event::Listening { .. }))
    })
    .await;
    (net, port)
}

fn join(address: String, password: &str, name: &str, aircraft: AircraftId) -> NetHandle {
    let net = spawn(&tokio::runtime::Handle::current());
    net.send(NetCommand::Join {
        address,
        password: Password::new(password),
        local: local(name, aircraft),
    });
    net
}

async fn join_result(net: &NetHandle) -> Event {
    session_event(
        wait_event(net, Duration::from_secs(20), |e| {
            matches!(
                e,
                NetEvent::Session(Event::Joined { .. } | Event::JoinFailed(_))
            )
        })
        .await,
    )
}

async fn crew_event(net: &NetHandle) -> Event {
    session_event(
        wait_event(net, Duration::from_secs(20), |e| {
            matches!(
                e,
                NetEvent::Session(
                    Event::CrewJoined { .. } | Event::CrewRefused(_) | Event::CrewGone(_)
                )
            )
        })
        .await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_with_correct_password_streams_state_and_pause() {
    let (host_net, port) = host("secret").await;
    let joiner = join(format!("127.0.0.1:{port}"), "secret", "Alex", c172());

    assert_eq!(
        join_result(&joiner).await,
        Event::Joined { host: "Sam".into() }
    );
    assert_eq!(
        crew_event(&host_net).await,
        Event::CrewJoined {
            name: "Alex".into()
        }
    );

    for seq in 0..5 {
        host_net.send(NetCommand::SendFlightState(FlightState {
            seq,
            ..FlightState::default()
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = Vec::new();
    while received.len() < 5 && Instant::now() < deadline {
        while let Some(s) = joiner.try_sample() {
            received.push(s.state.seq);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(received, vec![0, 1, 2, 3, 4]);

    host_net.send(NetCommand::SendPaused(true));
    let event = wait_event(&joiner, Duration::from_secs(5), |e| {
        matches!(e, NetEvent::Paused(_))
    })
    .await;
    assert_eq!(event, NetEvent::Paused(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_password_is_refused_and_retries_are_rate_limited() {
    let (host_net, port) = host("secret").await;
    let address = format!("127.0.0.1:{port}");

    let joiner = join(address.clone(), "guess", "Alex", c172());
    assert_eq!(
        join_result(&joiner).await,
        Event::JoinFailed(JoinFailure::Rejected(RejectReason::BadPassword))
    );
    assert_eq!(
        crew_event(&host_net).await,
        Event::CrewRefused(Refusal::BadPassword)
    );

    // An immediate retry from the same address is refused without a check.
    let retry = join(address.clone(), "secret", "Alex", c172());
    assert_eq!(
        join_result(&retry).await,
        Event::JoinFailed(JoinFailure::Rejected(RejectReason::TooManyAttempts))
    );

    // After the delay the correct password works; the host kept waiting.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let later = join(address, "secret", "Alex", c172());
    assert_eq!(
        join_result(&later).await,
        Event::Joined { host: "Sam".into() }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aircraft_mismatch_names_host_aircraft() {
    let (host_net, port) = host("secret").await;
    let joiner = join(format!("127.0.0.1:{port}"), "secret", "Alex", seaplane());
    assert_eq!(
        join_result(&joiner).await,
        Event::JoinFailed(JoinFailure::Rejected(RejectReason::AircraftMismatch {
            host_aircraft: c172()
        }))
    );
    assert_eq!(
        crew_event(&host_net).await,
        Event::CrewRefused(Refusal::AircraftMismatch {
            joiner_aircraft: seaplane(),
            host_aircraft: c172()
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn any_aircraft_is_accepted_when_both_seats_fly_it() {
    let baron = AircraftId {
        folder: "Beechcraft Baron 58".into(),
        acf: "Baron_58.acf".into(),
        name: "Baron 58".into(),
    };
    let (_host_net, port) = host_with("secret", baron.clone()).await;
    let joiner = join(format!("127.0.0.1:{port}"), "secret", "Alex", baron);
    assert!(matches!(join_result(&joiner).await, Event::Joined { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_mismatch_reports_both_versions() {
    let (host_net, port) = host("secret").await;
    let target: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    // A joiner from a future version, speaking only the frozen Hello.
    let client = endpoint::client(target).unwrap();
    let conn = client
        .connect(target, endpoint::SERVER_NAME)
        .unwrap()
        .await
        .unwrap();
    let (mut send, recv) = conn.open_bi().await.unwrap();
    control::send(
        &mut send,
        &Control::Hello {
            protocol_version: 99,
            plugin_version: "9.9.9".into(),
        },
    )
    .await
    .unwrap();
    let mut reader = ControlReader::new(recv);
    assert_eq!(
        control::expect(&mut reader).await.unwrap(),
        Control::Reject(RejectReason::VersionMismatch {
            host_protocol_version: PROTOCOL_VERSION,
            host_plugin_version: "0.1.0".into()
        })
    );
    conn.close(0u32.into(), b"");
    assert_eq!(
        crew_event(&host_net).await,
        Event::CrewRefused(Refusal::VersionMismatch {
            joiner_plugin_version: "9.9.9".into(),
            joiner_protocol_version: 99
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn third_seat_is_refused_and_session_continues() {
    let (host_net, port) = host("secret").await;
    let address = format!("127.0.0.1:{port}");
    let first = join(address.clone(), "secret", "Alex", c172());
    assert!(matches!(join_result(&first).await, Event::Joined { .. }));
    assert!(matches!(
        crew_event(&host_net).await,
        Event::CrewJoined { .. }
    ));

    let second = join(address, "secret", "Kim", c172());
    assert_eq!(
        join_result(&second).await,
        Event::JoinFailed(JoinFailure::Rejected(RejectReason::SessionFull))
    );
    assert_eq!(
        crew_event(&host_net).await,
        Event::CrewRefused(Refusal::SessionFull)
    );

    // The first follower still receives flight state.
    host_net.send(NetCommand::SendFlightState(FlightState {
        seq: 7,
        ..FlightState::default()
    }));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = first.try_sample() {
            assert_eq!(s.state.seq, 7);
            break;
        }
        assert!(Instant::now() < deadline, "no sample after refusal");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deliberate_leave_is_seen_within_a_second() {
    let (host_net, port) = host("secret").await;
    let joiner = join(format!("127.0.0.1:{port}"), "secret", "Alex", c172());
    assert!(matches!(join_result(&joiner).await, Event::Joined { .. }));
    crew_event(&host_net).await;

    let start = Instant::now();
    joiner.send(NetCommand::Disconnect {
        reason: ByeReason::Left,
    });
    assert_eq!(
        crew_event(&host_net).await,
        Event::CrewGone(PeerGone::Said(ByeReason::Left))
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_stopping_notifies_follower_and_releases_port() {
    let (host_net, port) = host("secret").await;
    let joiner = join(format!("127.0.0.1:{port}"), "secret", "Alex", c172());
    assert!(matches!(join_result(&joiner).await, Event::Joined { .. }));
    crew_event(&host_net).await;

    let start = Instant::now();
    host_net.send(NetCommand::Disconnect {
        reason: ByeReason::StoppedHosting,
    });
    let event = session_event(
        wait_event(&joiner, Duration::from_secs(5), |e| {
            matches!(e, NetEvent::Session(Event::HostGone(_)))
        })
        .await,
    );
    assert_eq!(
        event,
        Event::HostGone(PeerGone::Said(ByeReason::StoppedHosting))
    );
    assert!(start.elapsed() < Duration::from_secs(1));

    // The port can be bound again.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(endpoint::server(port).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_in_use_fails_hosting() {
    let port = free_port();
    let _blocker = std::net::UdpSocket::bind(("0.0.0.0", port)).unwrap();
    let net = spawn(&tokio::runtime::Handle::current());
    net.send(NetCommand::Host {
        port,
        password: Password::new("secret"),
        local: local("Sam", c172()),
    });
    let event = session_event(
        wait_event(&net, Duration::from_secs(5), |e| {
            matches!(e, NetEvent::Session(Event::HostFailed(_)))
        })
        .await,
    );
    assert_eq!(event, Event::HostFailed(HostFailure::PortUnavailable));
}

/// A UDP relay that can be cut to simulate a network drop.
struct UdpProxy {
    addr: SocketAddr,
    cut: Arc<AtomicBool>,
}

async fn udp_proxy(target: SocketAddr) -> UdpProxy {
    let front = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let back = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    back.connect(target).await.unwrap();
    let addr = front.local_addr().unwrap();
    let cut = Arc::new(AtomicBool::new(false));
    let is_cut = cut.clone();
    tokio::spawn(async move {
        let mut client: Option<SocketAddr> = None;
        let mut a = vec![0u8; 65536];
        let mut b = vec![0u8; 65536];
        loop {
            tokio::select! {
                Ok((n, from)) = front.recv_from(&mut a) => {
                    client = Some(from);
                    if !is_cut.load(Ordering::Relaxed) {
                        let _ = back.send(&a[..n]).await;
                    }
                }
                Ok(n) = back.recv(&mut b) => {
                    if let Some(c) = client
                        && !is_cut.load(Ordering::Relaxed)
                    {
                        let _ = front.send_to(&b[..n], c).await;
                    }
                }
            }
        }
    });
    UdpProxy { addr, cut }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_drop_is_detected_within_ten_seconds() {
    let (host_net, port) = host("secret").await;
    let proxy = udp_proxy(format!("127.0.0.1:{port}").parse().unwrap()).await;
    let joiner = join(proxy.addr.to_string(), "secret", "Alex", c172());
    assert!(matches!(join_result(&joiner).await, Event::Joined { .. }));
    crew_event(&host_net).await;

    let start = Instant::now();
    proxy.cut.store(true, Ordering::Relaxed);
    assert_eq!(crew_event(&host_net).await, Event::CrewGone(PeerGone::Lost));
    let lost_after = start.elapsed();
    assert!(
        lost_after <= Duration::from_millis(10_500),
        "{lost_after:?}"
    );
    let event = session_event(
        wait_event(&joiner, Duration::from_secs(12), |e| {
            matches!(e, NetEvent::Session(Event::HostGone(_)))
        })
        .await,
    );
    assert_eq!(event, Event::HostGone(PeerGone::Lost));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_host_times_out() {
    let port = free_port();
    let start = Instant::now();
    let joiner = join(format!("127.0.0.1:{port}"), "secret", "Alex", c172());
    assert_eq!(
        join_result(&joiner).await,
        Event::JoinFailed(JoinFailure::Unreachable)
    );
    assert!(start.elapsed() <= Duration::from_millis(10_500));
}

/// A man in the middle relays every handshake message unchanged between
/// two separate TLS sessions. Neither proof verifies, so the host refuses
/// the password and the joiner is not admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn man_in_the_middle_fails_both_proofs() {
    let (host_net, host_port) = host("secret").await;
    let host_addr: SocketAddr = format!("127.0.0.1:{host_port}").parse().unwrap();
    let mitm_port = free_port();
    let mitm = endpoint::server(mitm_port).unwrap();

    tokio::spawn(async move {
        let front = mitm.accept().await.unwrap().await.unwrap();
        let (mut front_send, front_recv) = front.accept_bi().await.unwrap();
        let back_endpoint = endpoint::client(host_addr).unwrap();
        let back = back_endpoint
            .connect(host_addr, endpoint::SERVER_NAME)
            .unwrap()
            .await
            .unwrap();
        let (mut back_send, back_recv) = back.open_bi().await.unwrap();
        let mut from_joiner = ControlReader::new(front_recv);
        let mut from_host = ControlReader::new(back_recv);
        loop {
            tokio::select! {
                Ok(Some(m)) = from_joiner.next() => {
                    control::send(&mut back_send, &m).await.unwrap();
                }
                Ok(Some(m)) = from_host.next() => {
                    control::send(&mut front_send, &m).await.unwrap();
                }
                else => break,
            }
        }
        // Keep both connections open until the peers are done.
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop((front, back, back_endpoint));
    });

    let joiner = join(format!("127.0.0.1:{mitm_port}"), "secret", "Alex", c172());
    assert_eq!(
        join_result(&joiner).await,
        Event::JoinFailed(JoinFailure::Rejected(RejectReason::BadPassword))
    );
    assert_eq!(
        crew_event(&host_net).await,
        Event::CrewRefused(Refusal::BadPassword)
    );
}

/// A fake host that does not know the password cannot produce a valid
/// host proof, so the joiner refuses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impostor_host_is_detected() {
    let port = free_port();
    let fake = endpoint::server(port).unwrap();
    tokio::spawn(async move {
        let conn = fake.accept().await.unwrap().await.unwrap();
        let (mut send, recv) = conn.accept_bi().await.unwrap();
        let mut reader = ControlReader::new(recv);
        assert!(matches!(
            control::expect(&mut reader).await.unwrap(),
            Control::Hello { .. }
        ));
        control::send(
            &mut send,
            &Control::Challenge {
                salt: [5; 16],
                kdf: KdfParams::DEFAULT,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            control::expect(&mut reader).await.unwrap(),
            Control::ClientProof { .. }
        ));
        control::send(&mut send, &Control::HostProof { mac: [0; 32] })
            .await
            .unwrap();
        conn.closed().await;
    });

    let joiner = join(format!("127.0.0.1:{port}"), "secret", "Alex", c172());
    assert_eq!(
        join_result(&joiner).await,
        Event::JoinFailed(JoinFailure::HostProofFailed)
    );
}

#[test]
fn clean_name_limits_and_sanitises() {
    assert_eq!(clean_name("  Alex  "), "Alex");
    assert_eq!(clean_name("A\u{7}lex\n"), "Alex");
    assert_eq!(clean_name(""), "Pilot");
    assert_eq!(clean_name(&"x".repeat(100)).chars().count(), 32);
}
