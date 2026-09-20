use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use crossbeam::channel::{Receiver, Sender};
use game::{ClientPacket, ServerPacket};
use log::{error, info, warn};
use quinn::{
    crypto::rustls::QuicClientConfig,
    rustls::{
        self,
        pki_types::{CertificateDer, ServerName, UnixTime},
    },
    ClientConfig, Endpoint,
};

/// Longest one connect attempt may take. A localhost handshake is
/// milliseconds; without this an attempt against a not-yet-listening server
/// sits in quinn's retransmit backoff (1s, 2s, 4s, ...) until the 30s idle
/// timeout, so the client would notice a late server many seconds late.
pub const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
/// Pause between connect attempts.
pub const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(500);
/// Give up connecting after this long. Generous on purpose: `cargo run` may
/// still be linking a debug server, which then builds its spawn regions.
pub const CONNECT_DEADLINE: Duration = Duration::from_secs(120);

/// How long and how often `connect_and_handle` retries the initial connect.
#[derive(Clone, Copy, Debug)]
pub struct ConnectPolicy {
    pub attempt_timeout: Duration,
    pub retry_interval: Duration,
    pub deadline: Duration,
}

impl Default for ConnectPolicy {
    fn default() -> Self {
        Self {
            attempt_timeout: CONNECT_ATTEMPT_TIMEOUT,
            retry_interval: CONNECT_RETRY_INTERVAL,
            deadline: CONNECT_DEADLINE,
        }
    }
}

pub struct ServerConnection {
    send: Sender<ServerPacket>,
    recv: Receiver<ClientPacket>,
    server: SocketAddr,
    connect_policy: ConnectPolicy,
}

impl ServerConnection {
    pub fn new(
        send: Sender<ServerPacket>,
        recv: Receiver<ClientPacket>,
        server: SocketAddr,
    ) -> Self {
        Self {
            send,
            recv,
            server,
            connect_policy: ConnectPolicy::default(),
        }
    }

    /// Override the connect retry policy (test hook).
    pub fn with_connect_policy(mut self, policy: ConnectPolicy) -> Self {
        self.connect_policy = policy;
        self
    }

    /// Connect, retrying until the policy's deadline: the server may still be
    /// starting (or not started yet) when the client comes up.
    async fn connect_with_retry(
        &self,
        endpoint: &Endpoint,
    ) -> Result<quinn::Connection, std::io::Error> {
        let policy = self.connect_policy;
        let started = Instant::now();
        let mut attempt = 1u32;
        loop {
            let connecting = endpoint.connect(self.server, "localhost").unwrap();
            let failure = match tokio::time::timeout(policy.attempt_timeout, connecting).await {
                Ok(Ok(connection)) => {
                    game::perf_event!(
                        "connected",
                        "attempts={attempt} connect_ms={}",
                        game::perf::ms(started.elapsed())
                    );
                    return Ok(connection);
                }
                Ok(Err(error)) => error.to_string(),
                Err(_) => format!("no answer within {:?}", policy.attempt_timeout),
            };
            if started.elapsed() >= policy.deadline {
                error!(
                    "[client] giving up on {} after {attempt} attempts over {:?}: {failure}",
                    self.server,
                    started.elapsed()
                );
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("could not connect to server {}: {failure}", self.server),
                ));
            }
            warn!(
                "[client] connect attempt {attempt} to {} failed ({failure}); is the server running? retrying",
                self.server
            );
            attempt += 1;
            tokio::time::sleep(policy.retry_interval).await;
        }
    }

    /// Connect to the server and handle sending / receiving.
    pub async fn connect_and_handle(&mut self) -> Result<(), std::io::Error> {
        let config = ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(
                rustls::ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(SkipServerVerification::new())
                    .with_no_client_auth(),
            )
            .unwrap(),
        ));
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(config);
        info!("client endpoint at {:?}", endpoint.local_addr().unwrap());
        info!("connecting to server...");
        game::perf_event!("endpoint_up");
        // Receive events over the wire and send them to the instance manager.
        let connection = self.connect_with_retry(&endpoint).await?;
        info!("[client] connected: addr={}", connection.remote_address());
        let sender = self.send.clone();
        let recv_conn = connection.clone();

        tokio::spawn(async move {
            while let Ok(mut recv) = recv_conn.accept_uni().await {
                let read_started = Instant::now();
                let bytes = recv.read_to_end(usize::MAX).await.unwrap();
                let read = read_started.elapsed();
                let decode_started = Instant::now();
                // Unframe (+ lz4-decompress large packets) and deserialize;
                // `decode_ms` below covers both. `bytes` is the wire size.
                let packet = game::decode_packet(&bytes[..]);
                let packet: ServerPacket = match packet {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("Failed deserializing packet {:?}", e);
                        continue;
                    }
                };
                // Region snapshots are the join's big packets (MBs each); the
                // per-tick traffic is far too frequent for the timeline.
                if let ServerPacket::Region(id, _) = &packet {
                    game::perf_event!(
                        "snapshot_received",
                        "region={},{} bytes={} read_ms={} decode_ms={}",
                        id.x,
                        id.z,
                        bytes.len(),
                        game::perf::ms(read),
                        game::perf::ms(decode_started.elapsed())
                    );
                }

                let packet = match packet {
                    ServerPacket::SyncClock(region_id, server_tick_rate, server_tick, _) => {
                        ServerPacket::SyncClock(
                            region_id,
                            server_tick_rate,
                            server_tick,
                            recv_conn.rtt(),
                        )
                    }
                    p => p,
                };

                sender.send(packet).unwrap();
                recv.received_reset().await.unwrap();
            }
        });

        while let Ok(e) = self.recv.recv() {
            if let Some(close_reason) = connection.close_reason() {
                error!("Server connection closed: {:?}", close_reason);
                break;
            }
            connection.handshake_data().unwrap();
            let mut send = connection.open_uni().await.unwrap();
            let payload = game::encode_packet(&e).unwrap();
            send.write_all(&payload).await.unwrap();
            send.finish().unwrap();
            tokio::spawn(async move {
                send.stopped().await.unwrap();
            });
        }
        Ok(())
    }
}

/// Dummy certificate verifier that treats any certificate as valid.
/// NOTE, such verification is vulnerable to MITM attacks, but convenient for testing.
#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
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
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No server listening: the connect must retry, then return a `TimedOut`
    /// error at the deadline — the old single attempt panicked the netcode
    /// thread ~30s in, which is what `just run` hit when the server was slow.
    #[test]
    fn connect_gives_up_with_timed_out_instead_of_panicking() {
        // Bind then drop a UDP socket to get a port nothing listens on.
        let dead = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let (send, _server_packets) = crossbeam::channel::unbounded();
        let (_client_packets, recv) = crossbeam::channel::unbounded();
        let mut connection =
            ServerConnection::new(send, recv, dead).with_connect_policy(ConnectPolicy {
                attempt_timeout: Duration::from_millis(15),
                retry_interval: Duration::from_millis(5),
                deadline: Duration::from_millis(40),
            });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(connection.connect_and_handle())
            .expect_err("nothing is listening");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains(&dead.to_string()));
    }
}
