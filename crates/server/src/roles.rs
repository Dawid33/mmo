//! Process roles (`VOXEL_ROLE`): one process for the whole world (`all`: the client
//! edge, the regions and the world coordinator on its own thread, joined by a channel
//! link — single-player, `just run`, `just server`), or the world split into processes:
//! a **coordinator** (the history, claims and world requests, saved under its own fence;
//! no client edge, no regions) listening on the service link, and **hosts** (today's
//! server without the coordinator thread) dialing it through a [`QuicWorldLink`]. The
//! split roles need a Postgres world store: several processes write it.

use game::HostId;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crossbeam::channel::{Receiver, RecvTimeoutError, Sender};
use game::{CoordMsg, HostMsg, WorldLink};
use svc_link::{LinkDialer, LinkEvent, LinkHello, LinkTarget};
use world_store::StoreUrl;

use crate::claims::{Fenced, ServerExit};
use crate::world_file::{self, WorldSource};

/// Environment: the process's role, `all` (default), `host`, `coordinator` or `fleet`.
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
    /// The fleet reconciler (Elastic 4, [`crate::fleet`]): makes the world's host Deployments
    /// match `desired_hosts`; reads the store, never writes it.
    Fleet,
}

impl Role {
    /// The role from the value of [`ROLE_ENV`]; unset or empty is [`Role::All`].
    pub fn from_value(value: Option<&str>) -> Result<Role, String> {
        match value.map(str::trim).unwrap_or("") {
            "" | "all" => Ok(Role::All),
            "host" => Ok(Role::Host),
            "coordinator" => Ok(Role::Coordinator),
            "fleet" => Ok(Role::Fleet),
            other => Err(format!("{ROLE_ENV}={other:?} is not a role (all, host, coordinator or fleet)")),
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

/// Why a `claim` host refuses to start without the login link.
pub const CLAIM_NEEDS_LOGIN: &str =
    "VOXEL_OWNERSHIP=claim (any number of hosts) needs the login link (VOXEL_AUTH=jwt with a service token): ids and placement come from it";

/// What `VOXEL_OWNERSHIP=claim` refuses outside a host role.
pub const CLAIM_NEEDS_ROLE_HOST: &str =
    "VOXEL_OWNERSHIP=claim claims super-regions on demand and registers a lease: it needs VOXEL_ROLE=host";

/// What a host refuses without an ownership of its own.
pub const HOST_NEEDS_OWNERSHIP: &str =
    "VOXEL_ROLE=host needs VOXEL_OWNERSHIP=static (one host alone) or claim (any number of hosts)";

/// The start-up refusals of ownership `policy` (Elastic 1: a host reads no host count). `claim`
/// (`Succession`) means "any number of hosts": the role must be `host`, the store Postgres
/// ([`SPLIT_ROLES_NEED_POSTGRES`]) and the auth `jwt` with a service token (`login_link`: the
/// login service's push), since player ids and placement come from the login service. `static`
/// and `allinone` (`Rollback`) need nothing here (a second `static` host is refused by the store
/// at its registration).
pub fn check_claim_needs(role: Role, policy: game::CrashPolicy, postgres: bool, login_link: bool) -> Result<(), String> {
    if policy != game::CrashPolicy::Succession {
        return Ok(());
    }
    if role != Role::Host {
        return Err(CLAIM_NEEDS_ROLE_HOST.to_string());
    }
    if !postgres {
        return Err(SPLIT_ROLES_NEED_POSTGRES.to_string());
    }
    if !login_link {
        return Err(CLAIM_NEEDS_LOGIN.to_string());
    }
    Ok(())
}

/// A host (`role`) under `VOXEL_OWNERSHIP` = `word` (unset: `allinone`): `allinone` is refused
/// ([`HOST_NEEDS_OWNERSHIP`]) — a host's claims, lease and parts are its issued id's, never
/// `HostId::SOLO`'s.
pub fn check_host_ownership(role: Role, word: &str) -> Result<(), String> {
    if role == Role::Host && matches!(word.trim(), "" | "allinone") {
        return Err(HOST_NEEDS_OWNERSHIP.to_string());
    }
    Ok(())
}

/// What a split role refuses to start on: the world's clock anchor says it is earlier
/// than the saved world tick (a restored backup, a cleared-then-stale row).
pub const CLOCK_BEHIND_WORLD_TICK: &str =
    "the world clock is behind the saved world tick; run `worldctl clock-reset`";

/// The start-up decision of a process on the world's clock anchor: its clock, and the
/// phase (ms) to back-date its base instant by ([`game::WorldClock::from_anchor`]). Refused
/// ([`CLOCK_BEHIND_WORLD_TICK`]) when the anchor puts `now_unix_ms` before the saved
/// `world_tick`, since a region would then be asked to run backwards.
pub fn clock_from_anchor(
    anchor: game::ClockAnchor,
    now_unix_ms: u64,
    world_tick: game::Tick,
) -> Result<(game::WorldClock, u64), String> {
    if anchor.tick_at_unix(now_unix_ms) < world_tick {
        return Err(CLOCK_BEHIND_WORLD_TICK.to_string());
    }
    Ok(game::WorldClock::from_anchor(anchor, now_unix_ms))
}

/// Wall time as unix ms.
pub fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The world clock of a process (R3): split roles agree on one wall-anchored clock
/// stored in the world row (first writer wins), so every host and the coordinator
/// number ticks alike; `all` keeps its own clock from the saved tick (it clears the
/// anchor separately, after its claim: [`clear_anchor_after_claim`]). Returns the
/// clock and the phase (ms) to back-date the base instant by.
pub fn start_clock(
    role: Role,
    store: Option<&world_store::StoreHandle>,
    postgres: bool,
    world_tick: game::Tick,
) -> Result<(game::WorldClock, u64), String> {
    match (role, store) {
        (Role::All, _) => Ok((game::WorldClock::starting_at(0, world_tick), 0)),
        (_, Some(store)) => {
            let now = unix_ms_now();
            let anchor = store
                .ensure_clock_anchor(game::ClockAnchor { unix_ms: now, tick: world_tick })
                .map_err(|e| format!("the world clock anchor: {e}"))?;
            clock_from_anchor(anchor, now, world_tick)
        }
        (_, None) => Err(SPLIT_ROLES_NEED_POSTGRES.to_string()),
    }
}

/// The start-up checks of a process that need no host id, made BEFORE a host registers
/// (Elastic 1 final review A-I2): a host that fails after its registration leaves a dirty
/// `hosts` row, which the next start vouches dead and the coordinator declares — a succession
/// for a configuration error, and with a crash-looping pod one on every restart, which can roll
/// the world back. Checked: the ownership word (`VOXEL_OWNERSHIP`); under `claim` the
/// super-region side against the world row (`VOXEL_SUPER_REGION_REGIONS`); a split role's world
/// clock against the saved `world_tick` ([`CLOCK_BEHIND_WORLD_TICK`] — the clock anchor is
/// ensured, i.e. written if the world has none, and the clock is started again after the
/// registration, its phase taken then). `run` also parses every id-free setting it uses later
/// before registering (thread counts, `VOXEL_TERRAIN_NOTIFY`, the coordinator's address, token
/// and pin file).
pub fn check_before_registration(
    role: Role,
    store: Option<&world_store::StoreHandle>,
    postgres: bool,
    ownership_word: &str,
    claim_on_demand: bool,
    super_region_regions: u32,
    world_tick: game::Tick,
) -> Result<(), String> {
    game::ownership_for(ownership_word, HostId::SOLO)?;
    if let (Some(store), true) = (store, claim_on_demand) {
        crate::claims::HostClaims::check_side(store, super_region_regions)?;
    }
    if role == Role::Host {
        start_clock(role, store, postgres, world_tick)?;
    }
    Ok(())
}

/// Role `all` on Postgres forgets the world's clock anchor so a later split start writes a
/// fresh one. Call it only AFTER the super-region claim succeeded: an all-in-one claim bumps
/// every epoch, so any split host still running is fenced and exits; clearing earlier (or
/// unfenced) would let the next host anchor a different phase under live hosts. Other
/// roles and SQLite worlds: nothing.
pub fn clear_anchor_after_claim(
    role: Role,
    store: Option<&world_store::StoreHandle>,
    postgres: bool,
) -> Result<(), String> {
    if let (Role::All, Some(store), true) = (role, store, postgres) {
        store.clear_clock_anchor().map_err(|e| format!("clearing the world clock anchor: {e}"))?;
    }
    Ok(())
}

/// The base instant of a clock started with `phase_ms` (see [`start_clock`]): now, moved
/// earlier by the phase so tick edges fall at the same wall instants in every process.
pub fn back_dated_start(phase_ms: u64) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_sub(Duration::from_millis(phase_ms)).unwrap_or(now)
}

/// The coordinator process's view of the world tick: the anchored clock and its base.
pub struct CoordinatorClock {
    clock: game::WorldClock,
    started: std::time::Instant,
}

impl CoordinatorClock {
    pub fn new(clock: game::WorldClock, phase_ms: u64) -> Self {
        Self { clock, started: back_dated_start(phase_ms) }
    }

    /// The world tick in progress now; agrees with every host's to within one tick.
    pub fn world_tick_now(&self) -> game::Tick {
        self.clock.tick_at(self.started.elapsed().as_millis() as u64)
    }
}

/// A service-link address from an env value; unset or empty is
/// [`DEFAULT_COORDINATOR_ADDR`].
pub fn coordinator_addr_from(var: &str, value: Option<&str>) -> Result<SocketAddr, String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_COORDINATOR_ADDR);
    value.parse().map_err(|_| format!("{var}={value:?} is not an address (ip:port)"))
}

/// The coordinator's address as a host dials it: `host:port`, an IP or a DNS name (resolved at
/// every dial); unset or empty is [`DEFAULT_COORDINATOR_ADDR`]. The coordinator's BIND stays
/// `ip:port` ([`coordinator_addr_from`]).
pub fn coordinator_dial_target_from(var: &str, value: Option<&str>) -> Result<LinkTarget, String> {
    let value = value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_COORDINATOR_ADDR);
    LinkTarget::parse(value).map_err(|e| format!("{var}={value:?} is not an address (host:port): {e}"))
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
        addr: impl Into<LinkTarget>,
        mut pin: impl FnMut() -> io::Result<String> + Send + 'static,
        hello: LinkHello,
    ) -> QuicWorldLink {
        let addr: LinkTarget = addr.into();
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

/// Why a `claim` host refuses a Postgres without two-phase commit (however many hosts run:
/// its saves are world cuts).
pub const CLAIM_NEEDS_TWO_PHASE: &str =
    "a claim host (VOXEL_OWNERSHIP=claim, on the hosts and on their coordinator; any number of hosts) needs Postgres with max_prepared_transactions > 0 (its world cuts are staged in two phases)";

/// A coordinator's start-up on Postgres, before it claims its row: EVERY world cut part a
/// crash left staged is finished ([`crate::cuts::resolve_staged_cuts`]) — the cuts of a
/// coordinator before this one are over. Count-free (Elastic 1): the parts are found by their
/// prepared transactions' names, whatever hosts wrote them
/// ([`world_store::StoreHandle::staged_cut_parts`]); none staged (a server without prepared
/// transactions has none) is nothing to do. Whether two-phase commit must be available is the
/// crash policy's business ([`check_two_phase`]). `Err`: why the process must not start.
pub fn recover_staged_cuts(store: &world_store::StoreHandle) -> Result<(), String> {
    let staged = match store.staged_cut_parts() {
        Ok(staged) => staged,
        // A store with no prepared transactions at all (SQLite) has none staged.
        Err(world_store::StoreError::Unsupported(_)) => return Ok(()),
        Err(e) => return Err(format!("the world cut parts left staged could not be read: {e}")),
    };
    if staged.is_empty() {
        return Ok(());
    }
    log::warn!("[coordinator] world cut parts left staged by a crash (cut, host): {staged:?}");
    crate::cuts::resolve_staged_cuts(store)
        .map(|_| ())
        .map_err(|e| format!("the world cut parts left staged could not be resolved: {e}"))
}

/// Start-up of a `server::run` process, before it claims, registers or loads anything: on
/// Postgres the world cut parts a crash left staged are finished, in EVERY role — a part left
/// by a multi-host stack holds its `super_regions` rows, and a later `all` process's claim (or
/// a total crash's registration) would wait on it. Count-free (Elastic 1): the staged parts are
/// found by their prepared transactions' names, whatever hosts wrote them
/// ([`world_store::StoreHandle::staged_cut_parts`]); none staged is nothing to do (a server
/// without prepared transactions has none).
///
/// Elastic 1 Task 4: a cut with a part of a host that is ALIVE is not a crash's leftover but a
/// cut under way — a host starting beside it (a newcomer) must not abort it. Such a cut is
/// left to its coordinator (which decides it, or aborts it when a participant lapses). "Alive"
/// is what `register_host` reads: a dirty, live row of the world's generation beaten within
/// the host lease ([`game::HOST_LEASE_MS`]) by the database clock — a row of this process's own `name` (its
/// earlier process, which the registration lapses) never counts. With nobody alive (a total
/// crash) every staged part is finished, as before.
pub fn resolve_before_claim(postgres: bool, store: &world_store::StoreHandle, name: &str) -> Result<(), String> {
    if !postgres {
        return Ok(());
    }
    let staged = store.staged_cut_parts().map_err(|e| format!("the world cut parts left staged could not be read: {e}"))?;
    if staged.is_empty() {
        return Ok(());
    }
    let read = |e: world_store::StoreError| format!("the hosts beside the staged world cut parts could not be read: {e}");
    let rows = store.hosts().map_err(read)?;
    let now = store.db_now_ms().map_err(read)?;
    let generation = store.world_generation().map_err(read)?;
    let lease_ms = store.host_lease_ms();
    let alive: std::collections::BTreeSet<game::HostId> = rows
        .iter()
        .filter(|r| r.generation == generation && (name.is_empty() || r.name != name))
        .filter(|r| r.state == game::HOST_LIVE && !r.clean && now.saturating_sub(r.seen_ms) <= lease_ms)
        .map(|r| r.id)
        .collect();
    let mut cuts: std::collections::BTreeMap<game::CutId, Vec<game::HostId>> = std::collections::BTreeMap::new();
    for (cut, host) in staged {
        cuts.entry(cut).or_default().push(host);
    }
    for (cut, hosts) in cuts {
        if hosts.iter().any(|h| alive.contains(h)) {
            log::info!(
                "[server] world cut {} has staged parts of hosts {hosts:?}, alive beside this one: under way, left to its coordinator",
                cut.0
            );
            continue;
        }
        log::warn!("[server] world cut {} part(s) left staged by a crash (hosts {hosts:?}): finishing them", cut.0);
        store.abort_cut(cut, world_store::SWEEP_ABORT_TICK).map_err(|e| format!("the world cut parts left staged could not be resolved: {e}"))?;
    }
    Ok(())
}

/// A `claim` host (`policy` `Succession`: any number of hosts, its saves world cuts in two
/// phases) needs two-phase commit ([`CLAIM_NEEDS_TWO_PHASE`]); any other: nothing.
pub fn check_two_phase(policy: game::CrashPolicy, store: &world_store::StoreHandle) -> Result<(), String> {
    if policy == game::CrashPolicy::Succession {
        store.two_phase_ready().map_err(|e| format!("{CLAIM_NEEDS_TWO_PHASE}: {e}"))?;
    }
    Ok(())
}

/// Why role `all` does not start on a world other hosts ran ([`check_all_role_beside_hosts`]).
pub const ALL_ROLE_BESIDE_HOSTS: &str = "VOXEL_ROLE=all (one process running every region) is refused on a world with dirty \
`hosts` rows of other hosts";

/// Role `all` (`role`; any other role passes) on a store whose `hosts` table holds dirty rows
/// of ordinals other than `me` at the world's generation: other hosts ran it in a split role
/// and did not leave cleanly, so they may hold unsaved state, and this process — which claims
/// every super-region and resolves every staged part — would abort their staged cut parts and
/// fence them. Refused ([`ALL_ROLE_BESIDE_HOSTS`]), naming the rows and the remedies. A row of
/// an older generation (the world rolled back since: it holds nothing of this world) or a
/// clean one is no obstacle. A store whose rows cannot be read is refused too.
pub fn check_all_role_beside_hosts(role: Role, store: &world_store::StoreHandle, me: HostId) -> Result<(), String> {
    if role != Role::All {
        return Ok(());
    }
    let rows = store.hosts().map_err(|e| format!("{ALL_ROLE_BESIDE_HOSTS}: the hosts rows could not be read ({e})"))?;
    let generation = store.world_generation().map_err(|e| format!("{ALL_ROLE_BESIDE_HOSTS}: the world generation could not be read ({e})"))?;
    let others: Vec<u64> =
        rows.iter().filter(|r| r.id != me && !r.clean && r.generation == generation).map(|r| r.id.0).collect();
    if others.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{ALL_ROLE_BESIDE_HOSTS}: hosts {others:?} ran this world as split hosts and did not leave cleanly — they may hold \
         unsaved state, and an `all` process would claim their land, abort their staged cut parts and fence them. Start them \
         again (VOXEL_ROLE=host) until they leave cleanly; retire one that will never return with `worldctl retire-host \
         <url> <host id>` while other hosts run (claim ownership; with none running the first host to register takes the \
         land); or restart the world from its last save with `worldctl rollback <url> --yes`"
    ))
}

/// `VOXEL_ROLE=coordinator`: the world coordinator alone. Opens the Postgres world
/// store, claims the coordinator row, loads (or migrates, or bootstraps) its state and
/// serves hosts on [`COORDINATOR_BIND_ENV`] until a stop signal (an orderly stop saves
/// once more) or a fence.
pub fn run_coordinator() -> ServerExit {
    let fatal = |e: String| -> ! { crate::fatal(&e) };
    crate::health::start_from_env("coordinator").unwrap_or_else(|e| fatal(e));
    let source = world_file::resolve(
        std::env::var(worldgen::forged::WORLD_FILE_ENV).ok().as_deref(),
        std::env::var(world_file::WORLD_DB_ENV).ok().as_deref(),
    )
    .unwrap_or_else(|e| fatal(e));
    Role::Coordinator.check_store(&source).unwrap_or_else(|e| fatal(e));
    let empty = world_file::empty_store_from_env().unwrap_or_else(|e| fatal(e));
    // Watched before the store opens only when waiting for a world; otherwise where it always was.
    let early_stop = (empty == world_file::EmptyStore::Wait).then(crate::watch_stop_signals_on_thread);
    let Some(opened) = world_file::open_world_with(&source, &worldgen::forge::ForgeParams::default(), empty, early_stop.as_deref())
        .unwrap_or_else(|e| fatal(e))
    else {
        log::info!("[coordinator] stop signal while waiting for a world: exiting");
        return ServerExit::Clean;
    };
    let store = opened.store.clone().expect("a Postgres world has a store");
    let map = opened.map.clone().expect("a stored world has a map");
    // Elastic 1: no host count. `claim` (R4): any number of hosts, each with an issued id, all
    // saving in world cuts this coordinator runs (every save period, at the world tick of its
    // own clock), a crashed host's land inherited by one heir — the coordinator declares and
    // runs the successions. `static` (or unset): one host alone, which saves on its own; a
    // crash rolls the world back, never declared. The coordinator has no id of its own: the
    // policy from `VOXEL_OWNERSHIP` alone. A `VOXEL_HOSTS` left in its environment is ignored.
    let policy = coordinator_crash_policy(std::env::var(game::ownership::OWNERSHIP_ENV).ok().as_deref())
        .unwrap_or_else(|e| fatal(e));
    check_two_phase(policy, &store).unwrap_or_else(|e| fatal(e));
    // Before it claims the coordinator row: the world cut parts a crash left staged.
    recover_staged_cuts(&store).unwrap_or_else(|e| fatal(e));
    let (clock, phase) = start_clock(Role::Coordinator, Some(&store), true, opened.world_tick).unwrap_or_else(|e| fatal(e));
    let clock = CoordinatorClock::new(clock, phase);
    log::info!("[coordinator] world clock anchored: world tick {}", clock.world_tick_now());
    let bind = coordinator_addr_from(COORDINATOR_BIND_ENV, std::env::var(COORDINATOR_BIND_ENV).ok().as_deref())
        .unwrap_or_else(|e| fatal(e));
    // Registered as a replica first (Elastic 2b Task 4: an id issued by the store), then the row.
    let mut start = match world_file::ensure_coordinator_state_at(&store, &map, None, &bind.to_string()).unwrap_or_else(|e| fatal(e)) {
        Some(start) => start,
        None => fatal("this world has no history (nobody can live in it): there is nothing to coordinate".into()),
    };
    // Elastic 4: how many hosts the world wants (`VOXEL_FLEET_MAX` unset: no fleet decision).
    let fleet = fleet_dials_from(|name| std::env::var(name).ok()).unwrap_or_else(|e| fatal(e));
    match fleet {
        Some(d) => log::info!(
            "[coordinator] fleet: {} to {} hosts (scale up after {} ms, down after {} ms, a host lives at least {} ms)",
            d.min,
            d.max,
            d.up_after_ms,
            d.down_after_ms,
            d.min_lifetime_ms
        ),
        None => log::info!("[coordinator] fleet: {FLEET_MAX_ENV} is unset: the coordinator adds, drains and removes no host"),
    }
    start.coordinator.set_fleet(fleet);
    let token = svc_link::service_token_from_env().unwrap_or_else(|e| fatal(format!("the service link's token: {e}")));
    let cert_file = cert_hash_file_from(std::env::var(COORDINATOR_CERT_HASH_FILE_ENV).ok().as_deref());
    if let Some(dir) = cert_file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| fatal(format!("{}: {e}", dir.display())));
    }
    let fenced = Fenced::default();
    let claim_world = policy == game::CrashPolicy::Succession;
    let cuts = claim_world.then(|| crate::coordinator_rt::WorldCuts {
        every: Duration::from_secs(crate::world_save_secs(std::env::var(crate::WORLD_SAVE_SECS_ENV).ok().as_deref())),
        tick: Box::new(move || clock.world_tick_now()),
    });
    if claim_world {
        log::info!("[coordinator] ownership `claim`: world cuts of any number of hosts; a crashed host's land goes to an heir (successions)");
    }
    // Its links name the world (`VOXEL_WORLD_ID`, empty when unset: then only peers that name
    // none are admitted — hosts and the login service must agree). The login service links to
    // the coordinator (Elastic 1 Task 6), never to a host: the coordinator relays its orders to a
    // live host and the answers back.
    let world = crate::login_link::link_world_from(std::env::var(crate::auth_edge::WORLD_ID_ENV).ok().as_deref());
    let login = crate::coordinator_rt::LoginListen {
        bind: crate::login_link::login_bind_from(std::env::var(crate::login_link::LOGIN_LINK_BIND_ENV).ok().as_deref())
            .unwrap_or_else(|e| fatal(e)),
        token: token.clone(),
        cert_hash_file: Some(crate::login_link::login_cert_hash_file_from(
            std::env::var(crate::login_link::LOGIN_LINK_CERT_HASH_FILE_ENV).ok().as_deref(),
        )),
    };
    let coordinator = crate::coordinator_rt::start_listening_as(
        store.clone(),
        start,
        bind,
        token,
        Some(cert_file),
        fenced.clone(),
        cuts,
        policy,
        &world,
        Some(login),
    )
    .unwrap_or_else(|e| fatal(e));
    // Ready (`/readyz` 200) once its `ReplicaJoined` transition committed: the thread says so
    // (Elastic 2b Task 4). Its thread beats the loop.
    let health = crate::health::Health::global();

    let stop = early_stop.unwrap_or_else(crate::watch_stop_signals_on_thread);
    while !coordinator.thread.is_finished() && !stop.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(COORDINATOR_PROCESS_POLL);
    }
    health.set_registered(false);
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

/// `VOXEL_ROLE=fleet`: the fleet reconciler ([`crate::fleet`], Elastic 4). Postgres only; the
/// store opened read-only once it is forged; a pass every [`crate::fleet::FLEET_PASS_MS`] until
/// a stop signal (exit 0). Settings: [`crate::fleet::FleetConfig`].
pub fn run_fleet() -> ServerExit {
    let fatal = |e: String| -> ! { crate::fatal(&e) };
    crate::health::start_from_env("fleet").unwrap_or_else(|e| fatal(e));
    let source = world_file::resolve(
        std::env::var(worldgen::forged::WORLD_FILE_ENV).ok().as_deref(),
        std::env::var(world_file::WORLD_DB_ENV).ok().as_deref(),
    )
    .unwrap_or_else(|e| fatal(e));
    Role::Fleet.check_store(&source).unwrap_or_else(|e| fatal(e));
    let WorldSource::Store { url, .. } = &source else { fatal(SPLIT_ROLES_NEED_POSTGRES.to_string()) };
    let config = crate::fleet::FleetConfig::from_env(|name| std::env::var(name).ok()).unwrap_or_else(|e| fatal(e));
    let kube = kube_api::Kube::from_env(crate::fleet::FLEET_KUBE_API_ENV)
        .unwrap_or_else(|e| fatal(format!("the Kubernetes API ({}): {e}", crate::fleet::FLEET_KUBE_API_ENV)));
    let stop = crate::watch_stop_signals_on_thread();
    crate::fleet::run(url, config, kube, &stop)
}

/// The most desired hosts (Elastic 4; unset or empty: no fleet decision at all).
pub const FLEET_MAX_ENV: &str = "VOXEL_FLEET_MAX";
/// The fewest desired hosts (unset or empty: 1; 0 lets the last host go when idle, and needs
/// [`FLEET_MAX_ENV`] ≥ 2).
pub const FLEET_MIN_ENV: &str = "VOXEL_FLEET_MIN";
/// The pace dials of the load rules, ms (unset or empty: [`game::coordinator::SCALE_UP_AFTER_MS`],
/// [`game::coordinator::SCALE_DOWN_AFTER_MS`], [`game::coordinator::HOST_MIN_LIFETIME_MS`]).
pub const SCALE_UP_AFTER_MS_ENV: &str = "VOXEL_SCALE_UP_AFTER_MS";
pub const SCALE_DOWN_AFTER_MS_ENV: &str = "VOXEL_SCALE_DOWN_AFTER_MS";
pub const HOST_MIN_LIFETIME_MS_ENV: &str = "VOXEL_HOST_MIN_LIFETIME_MS";

/// The fleet's dials from the environment (`var` reads one variable; Elastic 4, plan decision 8):
/// `None` while [`FLEET_MAX_ENV`] is unset or empty. A dial that is not a number, a maximum or a
/// pace dial of 0, or a minimum above the maximum refuses the start; the minimum alone may be 0,
/// and only with a maximum of 2 or more: a last host that left still holding in-flight rows keeps
/// its desired row (and slot) until its `Leave` succession gives them to the next host, so scale
/// to zero needs a second slot for that host (Elastic 4 Task 8, fix round 1). The pace dials are
/// read (and refused) only with a maximum set.
pub fn fleet_dials_from(var: impl Fn(&str) -> Option<String>) -> Result<Option<game::coordinator::FleetDials>, String> {
    let read = |name: &str| var(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let number = |name: &str, value: &str, zero_ok: bool| -> Result<u64, String> {
        match value.parse::<u64>() {
            Ok(n) if n > 0 || zero_ok => Ok(n),
            _ => Err(format!(
                "invalid {name} value `{value}` (expected a number{})",
                if zero_ok { "" } else { " above 0" }
            )),
        }
    };
    let Some(max) = read(FLEET_MAX_ENV) else { return Ok(None) };
    let count = |name: &str, value: &str, zero_ok: bool| -> Result<u32, String> {
        let n = number(name, value, zero_ok)?;
        u32::try_from(n).map_err(|_| format!("invalid {name} value `{value}` (too large)"))
    };
    let max = count(FLEET_MAX_ENV, &max, false)?;
    let min = match read(FLEET_MIN_ENV) {
        Some(v) => count(FLEET_MIN_ENV, &v, true)?,
        None => 1,
    };
    if min > max {
        return Err(format!("{FLEET_MIN_ENV} ({min}) is above {FLEET_MAX_ENV} ({max})"));
    }
    if min == 0 && max < 2 {
        return Err(format!(
            "{FLEET_MIN_ENV}=0 needs {FLEET_MAX_ENV} of 2 or more (it is {max}): a last host that left still holding \
             in-flight rows keeps its slot until the next host inherits them, so the world could never start again"
        ));
    }
    let pace = |name: &str, default: u64| -> Result<u64, String> {
        match read(name) {
            Some(v) => number(name, &v, false),
            None => Ok(default),
        }
    };
    Ok(Some(game::coordinator::FleetDials {
        min,
        max,
        up_after_ms: pace(SCALE_UP_AFTER_MS_ENV, game::coordinator::SCALE_UP_AFTER_MS)?,
        down_after_ms: pace(SCALE_DOWN_AFTER_MS_ENV, game::coordinator::SCALE_DOWN_AFTER_MS)?,
        min_lifetime_ms: pace(HOST_MIN_LIFETIME_MS_ENV, game::coordinator::HOST_MIN_LIFETIME_MS)?,
    }))
}

/// The crash policy of a coordinator of a world whose ownership is `ownership`
/// (`VOXEL_OWNERSHIP`'s value; the coordinator has no id of its own), from that word alone —
/// no host count (Elastic 1): `claim` runs world cuts and successions whatever the number of
/// hosts (one attached host inherits from another that lapses; with nobody left alive no
/// heir exists, nothing is declared, and the next registration's total-crash rule takes the
/// land); `static`, `allinone` and unset roll the world back (R3). A bad value is an error.
pub fn coordinator_crash_policy(ownership: Option<&str>) -> Result<game::CrashPolicy, String> {
    game::crash_policy_from_value(ownership)
}

