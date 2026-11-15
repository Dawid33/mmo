use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::connections::{ConnEvent, ConnHandle, ConnOpener};
use crossbeam::channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use game::{ClientPacket, ServerPacket};
use log::{error, info, warn};
use crate::quic_verify::{client_crypto, host_of, policy_for, VerifyPolicy};
use quinn::{crypto::rustls::QuicClientConfig, ClientConfig, Endpoint, TransportConfig};

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

/// How often the client pings an otherwise quiet connection (the WebTransport
/// value, `server/src/webtransport.rs`).
pub const CLIENT_KEEP_ALIVE: Duration = Duration::from_secs(3);
/// How long a connection may hear nothing before it counts as lost (the
/// WebTransport value): a dead home host is noticed in ~10 s, not quinn's
/// default 30 s, and the session can rejoin (`login::flow`). The effective
/// timeout is the smaller of both ends' (the server's client endpoint keeps
/// quinn's 30 s), and the pings run on the netcode runtime's own thread, so a
/// main or game thread busy for seconds never trips it.
pub const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const _: () = assert!(CLIENT_KEEP_ALIVE.as_millis() * 3 <= CLIENT_IDLE_TIMEOUT.as_millis());

/// The client's QUIC transport: [`CLIENT_KEEP_ALIVE`], [`CLIENT_IDLE_TIMEOUT`].
pub fn client_transport() -> TransportConfig {
    let mut transport = TransportConfig::default();
    transport.keep_alive_interval(Some(CLIENT_KEEP_ALIVE));
    transport.max_idle_timeout(Some(CLIENT_IDLE_TIMEOUT.try_into().expect("the idle timeout fits a VarInt")));
    transport
}

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

/// Every connection of one session, on one tokio runtime (its own thread): the
/// login connection ([`NetThread::connect`]) and one per other host the home
/// sends the client to ([`ConnOpener`]). Each delivers its packets keyed by its
/// connection's key on the session's one event channel, and
/// a [`ConnEvent::Closed`] when it ends. The runtime runs until the session is
/// left (`cancel` disconnects) and every connection has flushed and closed.
pub struct NetThread {
    runtime: tokio::runtime::Handle,
    events: Sender<(u32, ConnEvent)>,
    cancel: Receiver<()>,
    policy: ConnectPolicy,
    /// Connections not ended yet.
    live: Arc<AtomicUsize>,
}

/// Where a connection dials.
pub enum ConnTarget {
    Addr(SocketAddr),
    /// `host:port` as a host's endpoint names it; resolved on the runtime.
    Name(String),
}

/// A host as its endpoint names it: the QUIC address (`host:port`) and the
/// certificate hash it hands out, if any (`HostEndpoint::cert_hash`, a login
/// ticket's `cert_hash`). Decides how the connection verifies the host
/// ([`policy_for`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostTarget {
    pub quic: String,
    pub cert_hash: Option<String>,
}

impl NetThread {
    /// Start the session's netcode runtime. Connections retry their connect by
    /// `policy` and give it up once `cancel` disconnects.
    pub fn start(
        events: Sender<(u32, ConnEvent)>,
        cancel: Receiver<()>,
        policy: ConnectPolicy,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("netcode").build()?;
        let handle = runtime.handle().clone();
        let live = Arc::new(AtomicUsize::new(0));
        let (left, open) = (cancel.clone(), live.clone());
        std::thread::Builder::new().name("netcode".into()).spawn(move || {
            runtime.block_on(async move {
                // Each connection flushes its last packets (the `Leave`) as it
                // ends: the runtime outlives the session until they all have.
                while !(matches!(left.try_recv(), Err(TryRecvError::Disconnected)) && open.load(Ordering::SeqCst) == 0) {
                    tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
                }
            });
        })?;
        Ok(Self { runtime: handle, events, cancel, policy, live })
    }

    /// Open a connection keyed `key` to `target`, presenting `credential` (`None`
    /// = the local player name) and sending what arrives on `outgoing` until its
    /// sender drops (then it closes gracefully). The host is verified by
    /// `cert_hash` when there is one, else by the address ([`policy_for`]).
    pub fn connect(
        &self,
        key: u32,
        target: ConnTarget,
        credential: Option<game::Credential>,
        outgoing: Receiver<ClientPacket>,
        cert_hash: Option<String>,
    ) {
        self.live.fetch_add(1, Ordering::SeqCst);
        let (events, policy, cancel) = (self.events.clone(), self.policy, self.cancel.clone());
        let work = self.runtime.spawn(async move {
            let (server, named) = match target {
                ConnTarget::Addr(addr) => (addr, addr.to_string()),
                ConnTarget::Name(name) => (resolve(name.clone()).await?, name),
            };
            let mut connection = ServerConnection::new(events, key, outgoing, server)
                .with_verification(&named, cert_hash.as_deref())
                .with_connect_policy(policy)
                .with_credential(credential)
                .with_cancel(cancel);
            connection.connect_and_handle().await
        });
        let (events, live) = (self.events.clone(), self.live.clone());
        self.runtime.spawn(async move {
            let reason = match work.await {
                Ok(Ok(())) => "server connection closed".to_string(),
                Ok(Err(e)) => e.to_string(),
                Err(e) => format!("netcode task failed: {e}"),
            };
            let _ = events.send((key, ConnEvent::Closed(reason)));
            live.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

impl ConnOpener for NetThread {
    fn open(&mut self, key: u32, endpoint: &game::HostEndpoint, credential: game::Credential) -> ConnHandle {
        let (send, outgoing) = crossbeam::channel::unbounded();
        match &endpoint.quic {
            Some(quic) => {
                self.connect(key, ConnTarget::Name(quic.clone()), Some(credential), outgoing, endpoint.cert_hash.clone())
            }
            None => {
                let ordinal = endpoint.id;
                let _ = self.events.send((key, ConnEvent::Closed(format!("host {ordinal} offers no QUIC address"))));
            }
        }
        ConnHandle { send }
    }
}

/// `name` (`host:port`) as an address: parsed, else looked up off the runtime's
/// workers.
async fn resolve(name: String) -> std::io::Result<SocketAddr> {
    if let Ok(addr) = name.parse() {
        return Ok(addr);
    }
    let shown = name.clone();
    let found = tokio::task::spawn_blocking(move || {
        std::net::ToSocketAddrs::to_socket_addrs(&name).map(|mut addrs| addrs.next())
    })
    .await
    .map_err(std::io::Error::other)??;
    found.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{shown} resolves to no address")))
}

pub struct ServerConnection {
    events: Sender<(u32, ConnEvent)>,
    /// This connection's key on `events` (`connections`: one per connection, for
    /// its whole life).
    key: u32,
    recv: Receiver<ClientPacket>,
    server: SocketAddr,
    /// How the host's certificate is checked, and the name asked for in the
    /// handshake (`localhost` unless a public name must match its certificate).
    verify: VerifyPolicy,
    server_name: String,
    connect_policy: ConnectPolicy,
    /// Disconnects (its sender drops) when the game thread abandons the
    /// session; `never()` when nobody can cancel.
    cancel: Receiver<()>,
    /// Sent as the connection's first packet; `None` = this client's local
    /// player name (`player_name::player_name`), read once connected.
    credential: Option<game::Credential>,
}

impl ServerConnection {
    pub fn new(
        events: Sender<(u32, ConnEvent)>,
        key: u32,
        recv: Receiver<ClientPacket>,
        server: SocketAddr,
    ) -> Self {
        Self {
            events,
            key,
            recv,
            server,
            verify: policy_for(&server.to_string(), None),
            server_name: "localhost".into(),
            connect_policy: ConnectPolicy::default(),
            cancel: crossbeam::channel::never(),
            credential: None,
        }
    }

    /// Verify the host `named` (`host:port`, as its endpoint gives it) by `cert_hash`
    /// or, without one, by its address: loopback is not verified, a public name
    /// must present a web PKI certificate for that name. The default is
    /// [`policy_for`] the connection's address with no hash.
    pub fn with_verification(mut self, named: &str, cert_hash: Option<&str>) -> Self {
        self.verify = policy_for(named, cert_hash);
        if self.verify == VerifyPolicy::WebPki {
            self.server_name = host_of(named).to_string();
        }
        self
    }

    /// Identify as this local player instead of the configured name (test hook).
    pub fn with_name(self, name: game::PlayerName) -> Self {
        self.with_credential(Some(game::Credential::Local(name)))
    }

    /// Identify with this credential (a join JWT); `None` = the local player name.
    pub fn with_credential(mut self, credential: Option<game::Credential>) -> Self {
        self.credential = credential;
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
            let connecting = endpoint
                .connect(self.server, &self.server_name)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("cannot connect to {}: {e}", self.server)))?;
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
                Raced::Done(Err(quinn::ConnectionError::TransportError(error)))
                    if crate::quic_verify::refused_certificate(error.code) =>
                {
                    // The same certificate comes back on every attempt: fail now, with the reason
                    // (not a timeout two minutes later).
                    error!("[client] {} presented a certificate this client refuses: {error}", self.server);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("the server {} presented a certificate this client refuses: {}", self.server, error.reason),
                    ));
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
        let mut config = ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(client_crypto(&self.verify)).expect("ring offers the QUIC initial cipher suite"),
        ));
        config.transport_config(Arc::new(client_transport()));
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
        let credential = self
            .credential
            .clone()
            .unwrap_or_else(|| game::Credential::Local(crate::player_name::player_name()));
        let hello = game::encode_packet(&ClientPacket::Hello { credential })
            .map_err(|e| std::io::Error::other(format!("cannot encode Hello: {e}")))?;
        let mut stream = connection.open_uni().await.map_err(connection_lost)?;
        stream.write_all(&hello).await.map_err(connection_lost)?;
        stream.finish().map_err(connection_lost)?;
        let (events, key) = (self.events.clone(), self.key);
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

                if events.send((key, ConnEvent::Packet(packet))).is_err() {
                    // The game thread ended the session.
                    break;
                }
                let _ = recv.received_reset().await;
            }
        });

        // The game thread's packets, moved off its channel by a blocking
        // bridge (a worker blocked on it would starve the session's other
        // connections). It ends with the game's sender, or with this loop.
        let (bridge, mut outgoing) = tokio::sync::mpsc::unbounded_channel::<ClientPacket>();
        let from_game = self.recv.clone();
        tokio::task::spawn_blocking(move || loop {
            match from_game.recv_timeout(CLOSE_POLL_INTERVAL) {
                Ok(packet) => {
                    if bridge.send(packet).is_err() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) if bridge.is_closed() => return,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        });
        // Forward the game thread's packets until it closes this connection
        // (its sender drops: `Ok`) or the connection goes away (`Err`, whose
        // message is the reason). Polled, so a lost server is noticed even
        // while the game sends nothing.
        let mut last_sent: Option<tokio::task::JoinHandle<()>> = None;
        loop {
            if let Some(close_reason) = connection.close_reason() {
                error!("Server connection closed: {:?}", close_reason);
                return Err(connection_lost(close_reason));
            }
            let packet = match tokio::time::timeout(CLOSE_POLL_INTERVAL, outgoing.recv()).await {
                Ok(Some(packet)) => packet,
                Err(_) => continue,
                Ok(None) => {
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
            ServerConnection::new(send, 0, recv, dead).with_connect_policy(ConnectPolicy {
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

    /// A dead host is noticed within ~10 s (the WebTransport values), not
    /// quinn's 30 s default with no keep-alive.
    #[test]
    fn client_transport_keeps_alive() {
        let shown = format!("{:?}", client_transport());
        assert!(shown.contains("keep_alive_interval: Some(3s)"), "{shown}");
        assert!(shown.contains("max_idle_timeout: Some(10000)"), "{shown}");
        let default = format!("{:?}", TransportConfig::default());
        assert!(default.contains("keep_alive_interval: None"), "quinn's default has no keep-alive: {default}");
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
        let mut connection = ServerConnection::new(send, 0, recv, addr)
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
