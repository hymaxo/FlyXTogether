//! QUIC endpoints: sockets, TLS and transport settings.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use flyx_protocol::ALPN;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint, EndpointConfig, ServerConfig, TransportConfig, VarInt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use socket2::{Domain, Protocol, Socket, Type};

/// No traffic for this long ends a session (spec: loss detected within 10 s).
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Keep-alive interval, well below the idle timeout.
pub(crate) const KEEP_ALIVE: Duration = Duration::from_secs(2);
/// TLS server name used by joiners. Certificates are not checked against
/// it; the password proof authenticates the host instead.
pub(crate) const SERVER_NAME: &str = "flyxtogether";

/// Error while starting to host.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ListenError {
    #[error("port {0} is already in use")]
    PortInUse(u16),
    #[error("{0}")]
    Other(String),
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn transport() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.max_idle_timeout(Some(IDLE_TIMEOUT.try_into().expect("10 s fits")))
        .keep_alive_interval(Some(KEEP_ALIVE))
        .datagram_receive_buffer_size(Some(256 * 1024))
        .max_concurrent_bidi_streams(VarInt::from_u32(1))
        .max_concurrent_uni_streams(VarInt::from_u32(0));
    Arc::new(t)
}

/// A server configuration with a fresh self-signed certificate.
fn server_config() -> Result<ServerConfig, ListenError> {
    let other = |e: &dyn std::fmt::Display| ListenError::Other(e.to_string());
    let certified =
        rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()]).map_err(|e| other(&e))?;
    let cert: CertificateDer<'static> = certified.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| other(&e))?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(|e| other(&e))?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicServerConfig::try_from(tls).map_err(|e| other(&e))?;
    let mut config = ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(transport());
    Ok(config)
}

fn client_config() -> ClientConfig {
    let provider = provider();
    let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3 is supported by ring")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicClientConfig::try_from(tls).expect("ring provides a QUIC cipher suite");
    let mut config = ClientConfig::new(Arc::new(crypto));
    config.transport_config(transport());
    config
}

/// Accepts any certificate but still checks the handshake signature.
/// Authentication happens afterwards through the password proof, which is
/// bound to this TLS session (see `auth`).
#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Binds a UDP socket. `dual_stack` binds IPv6 with IPv4-mapped addresses
/// enabled, so one socket serves both families.
fn bind_udp(addr: SocketAddr, dual_stack: bool) -> io::Result<UdpSocket> {
    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(!dual_stack)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// A listening endpoint on all local interfaces (IPv4 and, where
/// available, IPv6).
pub(crate) fn server(port: u16) -> Result<Endpoint, ListenError> {
    let config = server_config()?;
    // Windows lets a dual-stack IPv6 socket bind even when another program
    // holds the same port on IPv4, which would then swallow all IPv4
    // traffic. Probe IPv4 first so that conflict is reported.
    match bind_udp(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port), false) {
        Ok(probe) => drop(probe),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => return Err(ListenError::PortInUse(port)),
        Err(e) => return Err(ListenError::Other(e.to_string())),
    }
    let socket = match bind_udp(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), port), true) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => return Err(ListenError::PortInUse(port)),
        // No IPv6 on this machine: fall back to IPv4 only.
        Err(_) => match bind_udp(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port), false) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                return Err(ListenError::PortInUse(port));
            }
            Err(e) => return Err(ListenError::Other(e.to_string())),
        },
    };
    Endpoint::new(
        EndpointConfig::default(),
        Some(config),
        socket,
        Arc::new(quinn::TokioRuntime),
    )
    .map_err(|e| ListenError::Other(e.to_string()))
}

/// A client endpoint on an ephemeral port of the family of `target`.
pub(crate) fn client(target: SocketAddr) -> io::Result<Endpoint> {
    let local = match target {
        SocketAddr::V4(_) => SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0),
        SocketAddr::V6(_) => SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0),
    };
    let socket = bind_udp(local, false)?;
    let mut endpoint = Endpoint::new(
        EndpointConfig::default(),
        None,
        socket,
        Arc::new(quinn::TokioRuntime),
    )?;
    endpoint.set_default_client_config(client_config());
    Ok(endpoint)
}

/// Addresses on this machine that a crew member could use to reach it,
/// formatted with the port, most likely first: home-network addresses,
/// then other private and VPN (e.g. Tailscale) ranges, then global IPv6.
pub(crate) fn local_addresses(port: u16) -> Vec<String> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut ips: Vec<IpAddr> = interfaces
        .iter()
        .map(|i| i.ip())
        .filter(is_shareable)
        .collect();
    ips.sort_by_key(|ip| (rank(ip), *ip));
    ips.dedup();
    ips.into_iter()
        .map(|ip| SocketAddr::new(ip, port).to_string())
        .collect()
}

fn is_shareable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            // 198.18.0.0/15 is reserved for benchmarking and used by some
            // VPN clients; nobody else can reach it.
            let benchmarking = o[0] == 198 && (o[1] & 0xfe) == 18;
            !v4.is_loopback()
                && !v4.is_link_local()
                && !v4.is_unspecified()
                && !v4.is_broadcast()
                && !benchmarking
        }
        // Only global unicast (2000::/3); others are local or overlays.
        IpAddr::V6(v6) => (v6.segments()[0] & 0xe000) == 0x2000,
    }
}

/// Sort key: typical home networks first.
fn rank(ip: &IpAddr) -> u8 {
    match ip {
        IpAddr::V4(v4) => match v4.octets() {
            [192, 168, 56, _] => 3, // VirtualBox host-only network
            [192, 168, _, _] => 0,
            [10, _, _, _] => 1,
            [172, b, _, _] if (16..32).contains(&b) => 2,
            [100, b, _, _] if (64..128).contains(&b) => 2, // Tailscale / CGNAT
            _ => 4,
        },
        IpAddr::V6(_) => 5,
    }
}

/// The peer's IP without IPv4-mapped IPv6 wrapping, for rate limiting.
pub(crate) fn canonical_ip(addr: SocketAddr) -> IpAddr {
    match addr.ip() {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flyx_protocol::{Control, Datagram, FlightState, FrameDecoder, encode_frame};

    /// Finds a port that is free right now.
    pub(crate) fn free_port() -> u16 {
        UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn shareable_addresses() {
        let ok = |s: &str| is_shareable(&s.parse().unwrap());
        assert!(ok("192.168.1.20"));
        assert!(ok("10.0.0.5"));
        assert!(ok("100.101.102.103"));
        assert!(ok("2001:db8::20"));
        assert!(!ok("127.0.0.1"));
        assert!(!ok("169.254.1.1"));
        assert!(!ok("198.18.0.1"));
        assert!(!ok("fe80::1"));
        assert!(!ok("::1"));
        assert!(!ok("fd00::1"));
        assert!(!ok("200:a2c7:c375:b457:3fea:93b5:2e6f:136b"));
    }

    #[test]
    fn home_network_addresses_come_first() {
        let mut ips: Vec<IpAddr> = [
            "2001:db8::1",
            "172.18.32.1",
            "192.168.56.1",
            "10.0.0.2",
            "192.168.1.3",
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
        ips.sort_by_key(|ip| (rank(ip), *ip));
        let sorted: Vec<String> = ips.iter().map(|i| i.to_string()).collect();
        assert_eq!(
            sorted,
            [
                "192.168.1.3",
                "10.0.0.2",
                "172.18.32.1",
                "192.168.56.1",
                "2001:db8::1"
            ]
        );
    }

    #[test]
    fn canonical_ip_unwraps_mapped_v4() {
        let mapped: SocketAddr = "[::ffff:203.0.113.7]:1".parse().unwrap();
        assert_eq!(
            canonical_ip(mapped),
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn port_in_use_is_reported() {
        let port = free_port();
        let _first = server(port).unwrap();
        assert!(matches!(server(port), Err(ListenError::PortInUse(p)) if p == port));
    }

    /// Two endpoints over loopback exchange a control message each way and
    /// a flight-state datagram.
    #[tokio::test]
    async fn loopback_control_and_datagrams() {
        let port = free_port();
        let host = server(port).unwrap();
        let target: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let joiner = client(target).unwrap();

        let host_task = tokio::spawn(async move {
            let conn = host.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            let mut decoder = FrameDecoder::new();
            let message = loop {
                let mut buf = [0u8; 1024];
                let n = recv.read(&mut buf).await.unwrap().unwrap();
                decoder.push(&buf[..n]);
                if let Some(m) = decoder.next_message().unwrap() {
                    break m;
                }
            };
            assert_eq!(message, Control::Paused(true));
            send.write_all(&encode_frame(&Control::Paused(false)))
                .await
                .unwrap();
            let state = FlightState {
                seq: 42,
                ..FlightState::default()
            };
            conn.send_datagram(Datagram::FlightState(state).encode().into())
                .unwrap();
            // Wait until the joiner closes the connection.
            conn.closed().await;
        });

        let conn = joiner.connect(target, SERVER_NAME).unwrap().await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(&encode_frame(&Control::Paused(true)))
            .await
            .unwrap();
        let mut decoder = FrameDecoder::new();
        let reply = loop {
            let mut buf = [0u8; 1024];
            let n = recv.read(&mut buf).await.unwrap().unwrap();
            decoder.push(&buf[..n]);
            if let Some(m) = decoder.next_message().unwrap() {
                break m;
            }
        };
        assert_eq!(reply, Control::Paused(false));
        let datagram = conn.read_datagram().await.unwrap();
        match Datagram::decode(&datagram).unwrap() {
            Datagram::FlightState(s) => assert_eq!(s.seq, 42),
            other => panic!("unexpected {other:?}"),
        }
        conn.close(VarInt::from_u32(0), b"done");
        host_task.await.unwrap();
    }
}
