use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use crossbeam::channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
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

/// How often a pending connect attempt or retry pause checks whether the
/// session was abandoned.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Longest the netcode waits, once the game thread ends the session, for the
/// last packet (the `Leave`) to be read by the server, and again for the
/// connection close to be acknowledged.
const LEAVE_FLUSH_TIMEOUT: Duration = Duration::from_millis(250);

/// What `race_cancel` saw first.
enum Raced<T> {
    Done(T),
    TimedOut,
    Cancelled,
}

pub struct ServerConnection {
    send: Sender<ServerPacket>,
    recv: Receiver<ClientPacket>,
    server: SocketAddr,
    connect_policy: ConnectPolicy,
    /// Disconnects (its sender drops) when the game thread abandons the
    /// session; `never()` when nobody can cancel.
    cancel: Receiver<()>,
    /// Sent as the connection's first packet; `None` = this machine's
    /// (`player_token::load_or_create_token`), read once connected.
    token: Option<game::PlayerToken>,
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
            cancel: crossbeam::channel::never(),
            token: None,
        }
    }

    /// Identify as this player instead of the machine's token (test hook).
    pub fn with_token(mut self, token: game::PlayerToken) -> Self {
        self.token = Some(token);
        self
    }

    /// Abandon the connect once every sender of `cancel` has dropped: the
    /// game thread holds one for the session, so leaving before the server
    /// answers stops the retries at once instead of letting a stale attempt
    /// reach whatever server binds the port next.
    pub fn with_cancel(mut self, cancel: Receiver<()>) -> Self {
        self.cancel = cancel;
        self
    }

    fn cancelled(&self) -> bool {
        matches!(self.cancel.try_recv(), Err(TryRecvError::Disconnected))
    }

    /// Drive `fut` until it completes, `limit` has passed, or the session is
    /// cancelled, whichever comes first.
    async fn race_cancel<F: std::future::Future>(
        &self,
        fut: F,
        limit: Duration,
    ) -> Raced<F::Output> {
        let mut fut = std::pin::pin!(fut);
        let started = Instant::now();
        loop {
            if self.cancelled() {
                return Raced::Cancelled;
            }
            let left = limit.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Raced::TimedOut;
            }
            if let Ok(out) =
                tokio::time::timeout(left.min(CANCEL_POLL_INTERVAL), fut.as_mut()).await
            {
                return Raced::Done(out);
            }
        }
    }

    /// Override the connect retry policy (test hook).
    pub fn with_connect_policy(mut self, policy: ConnectPolicy) -> Self {
        self.connect_policy = policy;
        self
    }

    /// Connect, retrying until the policy's deadline: the server may still be
    /// starting (or not started yet) when the client comes up. `Ok(None)`
    /// when the session was cancelled first (an in-flight attempt is
    /// abandoned, and no further one is made).
    async fn connect_with_retry(
        &self,
        endpoint: &Endpoint,
    ) -> Result<Option<quinn::Connection>, std::io::Error> {
        let policy = self.connect_policy;
        let started = Instant::now();
        let mut attempt = 1u32;
        loop {
            if self.cancelled() {
                return Ok(None);
            }
            let connecting = endpoint.connect(self.server, "localhost").unwrap();
            let failure = match self.race_cancel(connecting, policy.attempt_timeout).await {
                Raced::Cancelled => {
                    info!(
                        "[client] connect to {} abandoned: session left",
                        self.server
                    );
                    return Ok(None);
                }
                Raced::Done(Ok(connection)) => {
                    game::perf_event!(
                        "connected",
                        "attempts={attempt} connect_ms={}",
                        game::perf::ms(started.elapsed())
                    );
                    return Ok(Some(connection));
                }
                Raced::Done(Err(error)) => error.to_string(),
                Raced::TimedOut => format!("no answer within {:?}", policy.attempt_timeout),
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
            if let Raced::Cancelled = self
                .race_cancel(std::future::pending::<()>(), policy.retry_interval)
                .await
            {
                info!(
                    "[client] connect to {} abandoned: session left",
                    self.server
                );
                return Ok(None);
            }
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
        let Some(connection) = self.connect_with_retry(&endpoint).await? else {
            return Ok(());
        };
        info!("[client] connected: addr={}", connection.remote_address());
        // The handshake: the server starts the session once this resolves,
        // and refuses a connection whose first packet is anything else.
        let token = self.token.clone().unwrap_or_else(crate::player_token::load_or_create_token);
        let hello = game::encode_packet(&ClientPacket::Hello { token })
            .map_err(|e| std::io::Error::other(format!("cannot encode Hello: {e}")))?;
        let mut stream = connection.open_uni().await.map_err(connection_lost)?;
        stream.write_all(&hello).await.map_err(connection_lost)?;
        stream.finish().map_err(connection_lost)?;
        let sender = self.send.clone();
        let recv_conn = connection.clone();

        tokio::spawn(async move {
            while let Ok(mut recv) = recv_conn.accept_uni().await {
                let read_started = Instant::now();
                let bytes = match recv.read_to_end(usize::MAX).await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        warn!("[client] dropping a server stream: {e}");
                        continue;
                    }
                };
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

                if sender.send(packet).is_err() {
                    // The game thread ended the session.
                    break;
                }
                let _ = recv.received_reset().await;
            }
        });

        // Forward the game thread's packets until it ends the session (its
        // sender drops: `Ok`) or the connection goes away (`Err`, whose
        // message is the session's end reason). Polled, so a lost server is
        // noticed even while the game sends nothing.
        let mut last_sent: Option<tokio::task::JoinHandle<()>> = None;
        loop {
            if let Some(close_reason) = connection.close_reason() {
                error!("Server connection closed: {:?}", close_reason);
                return Err(connection_lost(close_reason));
            }
            let packet = match self.recv.recv_timeout(CLOSE_POLL_INTERVAL) {
                Ok(packet) => packet,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    close_gracefully(&endpoint, &connection, last_sent.take()).await;
                    return Ok(());
                }
            };
            let payload = match game::encode_packet(&packet) {
                Ok(payload) => payload,
                Err(e) => {
                    error!("[client] failed to encode {packet:?}: {e}");
                    continue;
                }
            };
            let mut send = connection.open_uni().await.map_err(connection_lost)?;
            send.write_all(&payload).await.map_err(connection_lost)?;
            send.finish().map_err(connection_lost)?;
            last_sent = Some(tokio::spawn(async move {
                let _ = send.stopped().await;
            }));
        }
    }
}

/// How often the send loop checks for a closed connection while the game
/// thread has nothing to send.
const CLOSE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The game thread ended the session (usually after queueing `Leave`): let
/// the server read the last stream, then close the connection and wait for
/// the close to go out, each bounded by `LEAVE_FLUSH_TIMEOUT`. Without this
/// the runtime is dropped before quinn flushes anything, and the server only
/// notices at the idle timeout.
async fn close_gracefully(
    endpoint: &Endpoint,
    connection: &quinn::Connection,
    last_sent: Option<tokio::task::JoinHandle<()>>,
) {
    if let Some(last_sent) = last_sent {
        let _ = tokio::time::timeout(LEAVE_FLUSH_TIMEOUT, last_sent).await;
    }
    connection.close(0u32.into(), b"leave");
    let _ = tokio::time::timeout(LEAVE_FLUSH_TIMEOUT, endpoint.wait_idle()).await;
}

fn connection_lost(reason: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::ConnectionAborted,
        format!("connection to server lost: {reason}"),
    )
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
pub(crate) mod tests {
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

    /// Datagrams `socket` receives within `window`.
    pub(crate) fn datagrams_within(socket: &std::net::UdpSocket, window: Duration) -> usize {
        let end = Instant::now() + window;
        let mut count = 0;
        let mut buf = [0u8; 2048];
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return count;
            }
            socket.set_read_timeout(Some(left)).unwrap();
            if socket.recv_from(&mut buf).is_ok() {
                count += 1;
            }
        }
    }

    /// Leaving the session while the connect is still retrying must end the
    /// netcode at once and stop sending: a stale retry loop would otherwise
    /// reach the next world's server when it binds the same port.
    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn cancel_during_connect_retry_stops_attempts() {
        // A UDP socket that never answers: it sees every QUIC Initial.
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = silent.local_addr().unwrap();
        let (send, _server_packets) = crossbeam::channel::unbounded();
        let (_client_packets, recv) = crossbeam::channel::unbounded();
        let (cancel, cancel_recv) = crossbeam::channel::bounded::<()>(0);
        let mut connection = ServerConnection::new(send, recv, addr)
            .with_connect_policy(ConnectPolicy {
                attempt_timeout: Duration::from_millis(50),
                retry_interval: Duration::from_millis(10),
                deadline: Duration::from_secs(60),
            })
            .with_cancel(cancel_recv);
        let (done_send, done) = crossbeam::channel::bounded(1);
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(connection.connect_and_handle());
            let _ = done_send.send(result.map_err(|e| e.to_string()));
        });
        assert!(
            datagrams_within(&silent, Duration::from_millis(300)) > 0,
            "the client never tried to connect"
        );

        drop(cancel);
        let cancelled_at = Instant::now();
        let result = done
            .recv_timeout(Duration::from_secs(1))
            .expect("netcode still running 1 s after the cancel");
        assert_eq!(result, Ok(()));
        assert!(cancelled_at.elapsed() < Duration::from_millis(500));
        // Let anything already in flight land, then nothing more may come.
        let _ = datagrams_within(&silent, Duration::from_millis(100));
        assert_eq!(datagrams_within(&silent, Duration::from_millis(400)), 0);
    }
}
