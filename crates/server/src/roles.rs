//! Process roles (`VOXEL_ROLE`): one process for the whole world (`all`: the client
//! edge, the regions and the world coordinator on its own thread, joined by a channel
//! link — single-player, `just run`, `just server`), or the world split into processes:
//! a **coordinator** (the history, claims and world requests, saved under its own fence;
//! no client edge, no regions) listening on the service link, and **hosts** (today's
//! server without the coordinator thread) dialing it through a [`QuicWorldLink`]. The
//! split roles need a Postgres world store: several processes write it.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crossbeam::channel::{Receiver, RecvTimeoutError, Sender};
use game::{CoordMsg, HostMsg, WorldLink};
use svc_link::{LinkDialer, LinkEvent, LinkHello};
use world_store::StoreUrl;

use crate::claims::{Fenced, ServerExit};
use crate::world_file::{self, WorldSource};

/// Environment: the process's role, `all` (default), `host` or `coordinator`.
pub const ROLE_ENV: &str = "VOXEL_ROLE";
/// Environment: where a coordinator listens for hosts.
pub const COORDINATOR_BIND_ENV: &str = "VOXEL_COORDINATOR_BIND";
/// Environment: where a host dials its coordinator.
pub const COORDINATOR_ADDR_ENV: &str = "VOXEL_COORDINATOR_ADDR";
/// The coordinator's service-link address when neither variable names one.
pub const DEFAULT_COORDINATOR_ADDR: &str = "127.0.0.1:6480";
/// Environment: the file the coordinator writes its certificate's SHA-256 pin to, and a
/// host reads it from (the certificate and its private key are kept beside it:
/// `<file>.cert`, `<file>.key`). Keep it out of version control.
pub const COORDINATOR_CERT_HASH_FILE_ENV: &str = "VOXEL_COORDINATOR_CERT_HASH_FILE";
/// Default of [`COORDINATOR_CERT_HASH_FILE_ENV`], relative to the working directory
/// (under the git-ignored `target/`).
pub const DEFAULT_COORDINATOR_CERT_HASH_FILE: &str = "target/dev-r2/coordinator.cert-sha256";
/// Environment: the coordinator's pin itself, for a host — overrides the file.
pub const COORDINATOR_CERT_SHA256_ENV: &str = "VOXEL_COORDINATOR_CERT_SHA256";
/// Why a split role refuses a store that is not Postgres.
pub const SPLIT_ROLES_NEED_POSTGRES: &str = "split roles need a Postgres world (VOXEL_WORLD_DB=postgres://…)";
/// How often a host looks for the coordinator's pin while it has none yet (the
/// coordinator has never started).
pub const PIN_POLL: Duration = Duration::from_millis(500);
/// How often the coordinator process checks for a stop signal or a stopped thread.
const COORDINATOR_PROCESS_POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The whole world in this process.
    All,
    /// Regions and the client edge; the world coordinator is another process.
    Host,
    /// The world coordinator alone, serving host processes.
    Coordinator,
}

impl Role {
    /// The role from the value of [`ROLE_ENV`]; unset or empty is [`Role::All`].
    pub fn from_value(value: Option<&str>) -> Result<Role, String> {
        match value.map(str::trim).unwrap_or("") {
            "" | "all" => Ok(Role::All),
            "host" => Ok(Role::Host),
            "coordinator" => Ok(Role::Coordinator),
            other => Err(format!("{ROLE_ENV}={other:?} is not a role (all, host or coordinator)")),
        }
    }

    pub fn from_env() -> Result<Role, String> {
        Role::from_value(std::env::var(ROLE_ENV).ok().as_deref())
    }

    /// Whether this role may serve the world in `source`: the split roles only a
    /// Postgres store ([`SPLIT_ROLES_NEED_POSTGRES`]).
    pub fn check_store(self, source: &WorldSource) -> Result<(), String> {
        match (self, source) {
            (Role::All, _) | (_, WorldSource::Store { url: StoreUrl::Postgres(_), .. }) => Ok(()),
            _ => Err(SPLIT_ROLES_NEED_POSTGRES.to_string()),
        }
    }
}

/// A service-link address from an env value; unset or empty is
/// [`DEFAULT_COORDINATOR_ADDR`].
pub fn coordinator_addr_from(var: &str, value: Option<&str>) -> Result<SocketAddr, String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_COORDINATOR_ADDR);
    value.parse().map_err(|_| format!("{var}={value:?} is not an address (ip:port)"))
}

/// The coordinator's cert-hash file from an env value; unset or empty is
/// [`DEFAULT_COORDINATOR_CERT_HASH_FILE`].
pub fn cert_hash_file_from(value: Option<&str>) -> PathBuf {
    PathBuf::from(value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_COORDINATOR_CERT_HASH_FILE))
}

/// The host's end of the world link over the service link: a [`LinkDialer`] to the
/// coordinator that reconnects forever. Until the coordinator's pin is readable (it has
/// never started), the link waits for it, down.
///
/// The link's state moves only in [`WorldLink::pump`]: the dialer's events are taken in
/// order, and a message received before a `Down` / `Up` is read before that change shows
/// (a pump stops at a change while messages wait). Every `Up` is a new connection
/// ([`WorldLink::generation`]), so a reconnect between two pumps is still seen. Sends
/// while down are dropped (the manager's outbox and `Attach` cover them); delivery is
/// at-least-once around a reconnect (`svc_link`), which the coordinator's request memo
/// and the host's reply dedupe absorb.
pub struct QuicWorldLink {
    out: Sender<HostMsg>,
    events: Receiver<LinkEvent<CoordMsg>>,
    /// A `Up` / `Down` taken while messages before it were still unread.
    held: Option<LinkEvent<CoordMsg>>,
    inbox: VecDeque<CoordMsg>,
    up: bool,
    generation: u64,
    wake: Receiver<()>,
}

impl QuicWorldLink {
    /// Dial the coordinator at `addr` with `hello`; `pin` reads its certificate's
    /// SHA-256, retried every [`PIN_POLL`] until it answers.
    pub fn connect(
        addr: SocketAddr,
        mut pin: impl FnMut() -> io::Result<String> + Send + 'static,
        hello: LinkHello,
    ) -> QuicWorldLink {
        let (out, out_rx) = crossbeam::channel::unbounded::<HostMsg>();
        let (events_tx, events) = crossbeam::channel::unbounded();
        let (wake_tx, wake) = crossbeam::channel::bounded(1);
        std::thread::Builder::new()
            .name("coordinator-link".into())
            .spawn(move || {
                let mut said = false;
                let pin = loop {
                    match pin() {
                        Ok(pin) => break pin,
                        Err(e) => {
                            if !said {
                                log::info!("[server] waiting for the world coordinator's certificate pin: {e}");
                                said = true;
                            }
                        }
                    }
                    // Nothing is sent while down; this only notices the link dropped.
                    if let Err(RecvTimeoutError::Disconnected) = out_rx.recv_timeout(PIN_POLL) {
                        return;
                    }
                };
                log::info!("[server] dialing the world coordinator at {addr}");
                let (dial, dial_events) = LinkDialer::connect::<CoordMsg, HostMsg>(addr, pin, hello);
                loop {
                    crossbeam::channel::select! {
                        recv(out_rx) -> msg => match msg {
                            Ok(msg) => {
                                let _ = dial.send(msg);
                            }
                            // The link was dropped: dropping `dial` ends the dialer.
                            Err(_) => return,
                        },
                        recv(dial_events) -> ev => match ev {
                            Ok(ev) => {
                                if events_tx.send(ev).is_err() {
                                    return;
                                }
                                let _ = wake_tx.try_send(());
                            }
                            Err(_) => return,
                        },
                    }
                }
            })
            .expect("spawn the coordinator link thread");
        QuicWorldLink::over(out, events, wake)
    }

    /// The link's state machine over a dialer's `events` (and its outgoing queue `out`).
    fn over(out: Sender<HostMsg>, events: Receiver<LinkEvent<CoordMsg>>, wake: Receiver<()>) -> QuicWorldLink {
        QuicWorldLink { out, events, held: None, inbox: VecDeque::new(), up: false, generation: 0, wake }
    }

    /// Ready when the coordinator link has something (a message, up or down): a host
    /// loop that blocks on its other channels selects on it too. Its messages carry
    /// nothing; the manager pumps the link.
    pub fn wake(&self) -> Receiver<()> {
        self.wake.clone()
    }
}

impl WorldLink for QuicWorldLink {
    fn send(&mut self, msg: HostMsg) {
        if !self.up {
            log::debug!("coordinator link down: {} lost", crate::coordinator_rt::describe_host(&msg));
            return;
        }
        let _ = self.out.send(msg);
    }

    fn recv(&mut self) -> Option<CoordMsg> {
        self.inbox.pop_front()
    }

    fn is_up(&self) -> bool {
        self.up
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    fn pump(&mut self, _now_ms: u64) {
        loop {
            let event = match self.held.take() {
                Some(event) => event,
                None => match self.events.try_recv() {
                    Ok(event) => event,
                    Err(_) => return,
                },
            };
            match event {
                LinkEvent::Msg(msg) => self.inbox.push_back(msg),
                change if !self.inbox.is_empty() => {
                    // Read what came before the change first, as the link stood.
                    self.held = Some(change);
                    return;
                }
                LinkEvent::Up(_) => {
                    self.up = true;
                    self.generation += 1;
                    log::info!("[server] world coordinator link up (connection {})", self.generation);
                }
                LinkEvent::Down(why) => {
                    if self.up {
                        log::warn!("[server] world coordinator link down: {why}");
                    }
                    self.up = false;
                }
            }
        }
    }
}

/// `VOXEL_ROLE=coordinator`: the world coordinator alone. Opens the Postgres world
/// store, claims the coordinator row, loads (or migrates, or bootstraps) its state and
/// serves hosts on [`COORDINATOR_BIND_ENV`] until a stop signal (an orderly stop saves
/// once more) or a fence.
pub fn run_coordinator() -> ServerExit {
    let fatal = |e: String| -> ! { crate::fatal(&e) };
    let source = world_file::resolve(
        std::env::var(worldgen::forged::WORLD_FILE_ENV).ok().as_deref(),
        std::env::var(world_file::WORLD_DB_ENV).ok().as_deref(),
    )
    .unwrap_or_else(|e| fatal(e));
    Role::Coordinator.check_store(&source).unwrap_or_else(|e| fatal(e));
    let opened = world_file::open_world(&source, &worldgen::forge::ForgeParams::default()).unwrap_or_else(|e| fatal(e));
    let store = opened.store.clone().expect("a Postgres world has a store");
    let map = opened.map.clone().expect("a stored world has a map");
    let start = match world_file::ensure_coordinator_state(&store, &map, None).unwrap_or_else(|e| fatal(e)) {
        Some(start) => start,
        None => fatal("this world has no history (nobody can live in it): there is nothing to coordinate".into()),
    };
    let token = svc_link::service_token_from_env().unwrap_or_else(|e| fatal(format!("the service link's token: {e}")));
    let bind = coordinator_addr_from(COORDINATOR_BIND_ENV, std::env::var(COORDINATOR_BIND_ENV).ok().as_deref())
        .unwrap_or_else(|e| fatal(e));
    let cert_file = cert_hash_file_from(std::env::var(COORDINATOR_CERT_HASH_FILE_ENV).ok().as_deref());
    if let Some(dir) = cert_file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| fatal(format!("{}: {e}", dir.display())));
    }
    let fenced = Fenced::default();
    let coordinator =
        crate::coordinator_rt::start_listening(store.clone(), start, bind, token, Some(cert_file), fenced.clone())
            .unwrap_or_else(|e| fatal(e));

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("stop-signals".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("signal runtime");
                rt.block_on(crate::watch_stop_signals(stop));
            })
            .expect("spawn the stop-signal thread");
    }
    while !coordinator.thread.is_finished() && !stop.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(COORDINATOR_PROCESS_POLL);
    }
    if stop.load(std::sync::atomic::Ordering::SeqCst) {
        log::info!("[coordinator] stop signal: saving and exiting");
    } else {
        log::error!("[coordinator] the coordinator thread stopped");
    }
    let exit = coordinator.thread.shutdown(crate::EXIT_SAVE_TIMEOUT);
    store.shutdown();
    if fenced.is_set() {
        return ServerExit::Fenced;
    }
    exit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_from_env_value() {
        assert_eq!(Role::from_value(None), Ok(Role::All));
        assert_eq!(Role::from_value(Some(" ")), Ok(Role::All));
        assert_eq!(Role::from_value(Some("all")), Ok(Role::All));
        assert_eq!(Role::from_value(Some("host")), Ok(Role::Host));
        assert_eq!(Role::from_value(Some("coordinator")), Ok(Role::Coordinator));
        let err = Role::from_value(Some("both")).unwrap_err();
        assert!(err.contains(ROLE_ENV) && err.contains("both"), "{err}");
    }

    fn hello() -> LinkHello {
        LinkHello::new("t".into(), svc_link::PeerRole::Host { ordinal: 0, boot: 1 })
    }

    fn calendar(t: u64) -> CoordMsg {
        CoordMsg::Calendar(game::history::CalendarTick(t))
    }

    /// Each `Up` is a new generation; what arrived before a `Down` / `Up` is read before
    /// that change shows; nothing is sent while down.
    #[test]
    fn quic_link_reads_in_order_and_counts_connections() {
        let (out, out_rx) = crossbeam::channel::unbounded();
        let (tx, events) = crossbeam::channel::unbounded();
        let (_wake_tx, wake) = crossbeam::channel::bounded(1);
        let mut link = QuicWorldLink::over(out, events, wake);
        assert!(!link.is_up());
        link.send(HostMsg::PlayerOffline { player: game::PlayerId(1) });
        assert!(out_rx.try_recv().is_err(), "nothing is sent while down");

        for ev in [
            LinkEvent::Up(hello()),
            LinkEvent::Msg(calendar(1)),
            LinkEvent::Down("gone".into()),
            LinkEvent::Up(hello()),
            LinkEvent::Msg(calendar(2)),
        ] {
            tx.send(ev).unwrap();
        }
        link.pump(0);
        assert!(link.is_up() && link.generation() == 1, "the first connection, its message unread");
        assert_eq!(link.recv(), Some(calendar(1)));
        assert_eq!(link.recv(), None);
        link.pump(0);
        assert!(link.is_up(), "down and up again within one pump");
        assert_eq!(link.generation(), 2, "a new connection: the host re-attaches");
        assert_eq!(link.recv(), Some(calendar(2)));
        link.send(HostMsg::PlayerOffline { player: game::PlayerId(1) });
        assert!(out_rx.try_recv().is_ok(), "sent while up");

        tx.send(LinkEvent::Down("gone".into())).unwrap();
        link.pump(0);
        assert!(!link.is_up());
        assert_eq!(link.generation(), 2);
    }

    #[test]
    fn coordinator_addresses_default_to_6480() {
        let want: SocketAddr = "127.0.0.1:6480".parse().unwrap();
        assert_eq!(coordinator_addr_from(COORDINATOR_ADDR_ENV, None), Ok(want));
        assert_eq!(coordinator_addr_from(COORDINATOR_BIND_ENV, Some("")), Ok(want));
        assert_eq!(coordinator_addr_from(COORDINATOR_ADDR_ENV, Some("10.0.0.2:7000")), Ok("10.0.0.2:7000".parse().unwrap()));
        let err = coordinator_addr_from(COORDINATOR_ADDR_ENV, Some("coordinator")).unwrap_err();
        assert!(err.contains(COORDINATOR_ADDR_ENV), "{err}");
        assert_eq!(cert_hash_file_from(None), PathBuf::from("target/dev-r2/coordinator.cert-sha256"));
        assert_eq!(cert_hash_file_from(Some("/x/pin")), PathBuf::from("/x/pin"));
    }
}
