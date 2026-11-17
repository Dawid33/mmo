//! Debug launches: `client debug <preset>` skips the manual setup and starts in
//! a prepared scenario. A plain `client` opens on the main menu.
//!
//! Presets:
//! - `world` — a small generated world (`ForgeParams::default()`: the 16 km /
//!   32 m continent) in `worlds/debug-world/`, played as an adventure at the
//!   first populated site ([`choose_adventure_site`]; a world with no history
//!   plays where the player spawned). The world is REUSED between runs: an
//!   existing world is loaded (not re-forged) and a player the server restored
//!   into an adventure plays on. Only deleting the world directory or
//!   `--reset` starts over.
//!   `VOXEL_DEBUG_WORLD=<slug>` (native) plays an existing `worlds/<slug>/`
//!   instead — e.g. a big benchmark world — and is never forged or reset.
//!   Several clients on one server (`just run-two`, native): `--port N` hosts
//!   the world's server on UDP port N instead of a free one, and
//!   `--join HOST:PORT` skips the world (no forge, no server of its own) and
//!   joins that server; either way the client starts its adventure
//!   automatically. `--site SITE` starts it at that site instead of the first
//!   populated one (`just run-two`: client 2 adventures at the site client 1
//!   logs as `[debug] playing: adventure site=N`).
//! - `online` — start from the login kept in `VOXEL_LOGIN_REFRESH_FILE` (a
//!   refresh token an operator issued: `login account issue-refresh <email>`)
//!   at the login service (`VOXEL_LOGIN_URL`), join its world `--world <id>`
//!   (default [`DEFAULT_ONLINE_WORLD`]) with the ticket's JWT, then start an
//!   adventure as `world` does (or resume the account's). `just run-online`
//!   starts the login service and a jwt server and runs this. The login goes
//!   through the Play online flow (`login::flow`: its `login-http` worker, the
//!   kept refresh token, the ticket's [`crate::session::PendingJoin`]): the
//!   scenario only presses its buttons ([`LoginIntent`]s), no password
//!   involved. A refused kept login, a failed login or a failed join prints
//!   the screen's error line and exits non-zero.
//!   A session whose home host was replaced rejoins as a screen session does
//!   (`login::flow`) and starts again; where a screen session would leave the
//!   game (the rejoin gave up, the login was revoked, any other lost session)
//!   it prints why and exits non-zero ([`exit_when_lost`]).
//!
//! The scenario drives the ordinary flow, nothing is faked: the world goes
//! through the generator's forge job and `PendingWorld` → `AppState::Starting`
//! (its own local server), and the adventure is the Adventure tab's request
//! (`StartAdventure`), answered by the server as for a player.

use std::path::{Path, PathBuf};

use bevy::app::AppExit;
use bevy::prelude::*;
use game::history::SiteId;
use game::SiteMarker;
use worldgen::forge::ForgeParams;
use worldgen::{forged, world_store};

use crate::frontend::generator::{progress_text, ForgeEvent, ForgeJob};
use crate::instance::ClientRequest;
use crate::login::flow::{LoginIntent, LoginSession, RejoinEnd};
use crate::renderer::commander::CommanderCamera;
use crate::renderer::history_view::{populated, HistoryView};
use crate::renderer::ClientRequests;
use crate::session::{AppState, InPlay, LastSessionError, PendingWorld, PreparingWorld, SessionEnded, StartingStatus};

/// `worlds/<this>/`: the `world` preset's world.
pub const DEBUG_WORLD_SLUG: &str = "debug-world";
/// Its display name (`world_store::slug_for` turns it into [`DEBUG_WORLD_SLUG`]).
pub const DEBUG_WORLD_NAME: &str = "Debug: world";

/// `VOXEL_DEBUG_WORLD=<slug>`: play the `world` preset in an existing
/// `worlds/<slug>/` instead of [`DEBUG_WORLD_SLUG`] (benchmarks on a bigger
/// world). Such a world is never forged or `--reset`. Native only.
fn debug_world_override() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var("VOXEL_DEBUG_WORLD").ok().filter(|v| !v.is_empty())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

pub const USAGE: &str = "usage: client [debug world [--reset | --port N | --join HOST:PORT] [--site SITE] [--prefab NAME[:DX,DZ[:TURN]]... | --town STYLE[:ARMS[:LEN]]]]\n       \
    client [debug online [--world ID]]\n\
    \n\
    With no arguments the client opens on the main menu.\n\
    \n\
    presets:\n  \
      world      a small generated world, played as an adventure at the first populated site\n             \
                 (worlds/debug-world/); reused between runs, --reset re-forges it;\n             \
                 --port N hosts its server on UDP port N, --join HOST:PORT joins a\n             \
                 running server instead (several clients on one world); --site SITE\n             \
                 (a number) adventures at that site instead of the first populated one;\n             \
                 --prefab (repeatable, joined with `;`)\n             \
                 stamps prefabs beside the adventure site's region (not the spawn\n             \
                 region a fresh session waits in; other regions are untouched): only in a region\n             \
                 never saved (--reset, which --town takes too); extra prefab\n             \
                 directories via VOXEL_PREFAB_DIRS; --town STYLE[:ARMS[:LEN]] lays one town of\n             \
                 that style (ARMS 1..=4 streets of LEN 16..=256 blocks) beside that region\n             \
                 instead (not with --prefab), from the houses tagged STYLE (`just town`)\n  \
      online     start from the kept login (VOXEL_LOGIN_URL, VOXEL_LOGIN_REFRESH_FILE), join the\n             \
                 login service's world ID (default `dev`) and start an adventure (or resume the\n             \
                 account's) as `world` does (`just run-online` starts the login service and a\n             \
                 server for it)";

/// The `online` preset's world when `--world` is not given (`just dev-login`'s).
pub const DEFAULT_ONLINE_WORLD: &str = "dev";

/// A debug launch picked on the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugScenario {
    /// `port`: host the world's server there. `join`: play on that running
    /// server instead of the world's own (never with `reset` or `port`).
    /// `site`: start the adventure there instead of at the first populated site.
    /// `prefab`: the `--prefab` entries joined with `;` (a `parse_dev_spec` string).
    World {
        reset: bool,
        port: Option<u16>,
        join: Option<std::net::SocketAddr>,
        site: Option<SiteId>,
        prefab: Option<String>,
        /// `town`: the `--town` value, `STYLE[:ARMS[:LEN]]` (a `parse_town_spec` string).
        town: Option<String>,
    },
    /// Log in, join the login service's `world`, start an adventure.
    Online { world: String },
}

impl DebugScenario {
    fn site(&self) -> Option<SiteId> {
        match self {
            DebugScenario::World { site, .. } => *site,
            DebugScenario::Online { .. } => None,
        }
    }
}

/// Whether the `online` preset can start: `login_url` is the configured login
/// service, `kept` the refresh token the store keeps ([`crate::login::store::load_refresh`]).
/// A missing one is an error naming it (the variable, never a token).
pub fn online_login_from(login_url: Option<&str>, kept: Option<&str>) -> Result<(), String> {
    if login_url.is_none() {
        return Err(format!("`debug online` needs {} set (the login service)\n\n{USAGE}", crate::login::api::LOGIN_URL_ENV));
    }
    if kept.is_none() {
        return Err(format!(
            "`debug online` needs a kept login: set {} to a file made by `login account issue-refresh`\n\n{USAGE}",
            crate::login::store::REFRESH_FILE_ENV
        ));
    }
    Ok(())
}

/// The command line after the program name. `Ok(None)` = no debug launch.
pub fn parse_args<S: AsRef<str>>(args: &[S]) -> Result<Option<DebugScenario>, String> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    match args.as_slice() {
        [] => Ok(None),
        ["-h" | "--help" | "help", ..] => Err(USAGE.into()),
        ["debug"] => Err(format!("`debug` needs a preset\n\n{USAGE}")),
        ["debug", preset, flags @ ..] => {
            let mut reset = false;
            let mut port = None;
            let mut join = None;
            let mut site: Option<SiteId> = None;
            let mut world = None;
            let mut prefab: Option<String> = None;
            let mut town: Option<String> = None;
            let mut flags = flags.iter();
            while let Some(flag) = flags.next() {
                let mut value = || {
                    flags.next().copied().ok_or_else(|| format!("`{flag}` needs a value\n\n{USAGE}"))
                };
                match *flag {
                    "--reset" => reset = true,
                    "--port" => {
                        let v = value()?;
                        port = Some(v.parse::<u16>().map_err(|e| format!("bad --port `{v}`: {e}\n\n{USAGE}"))?);
                    }
                    "--join" => {
                        let v = value()?;
                        let addr = std::net::ToSocketAddrs::to_socket_addrs(v)
                            .ok()
                            .and_then(|mut a| a.next())
                            .ok_or_else(|| format!("bad --join `{v}`: want HOST:PORT\n\n{USAGE}"))?;
                        join = Some(addr);
                    }
                    "--site" => {
                        let v = value()?;
                        let n = v.parse::<u32>().map_err(|e| format!("bad --site `{v}`: {e}\n\n{USAGE}"))?;
                        if site.replace(SiteId(n)).is_some() {
                            return Err(format!("--site: only once\n\n{USAGE}"));
                        }
                    }
                    "--adventure" | "--adventure-at" => {
                        return Err(format!(
                            "{flag} is gone: an adventure is the default; --site N picks the site\n\n{USAGE}"
                        ));
                    }
                    "--prefab" => {
                        let v = value()?;
                        if v.trim().is_empty() || v.starts_with('-') {
                            return Err(format!("bad --prefab `{v}`: want NAME[:DX,DZ[:TURN]]\n\n{USAGE}"));
                        }
                        prefab = Some(match prefab.take() {
                            Some(p) => format!("{p};{v}"),
                            None => v.to_string(),
                        });
                    }
                    "--town" => {
                        let v = value()?;
                        if v.trim().is_empty() || v.starts_with('-') || town.replace(v.to_string()).is_some() {
                            return Err(format!("bad --town `{v}`: want STYLE[:ARMS[:LEN]], once\n\n{USAGE}"));
                        }
                    }
                    "--world" => {
                        let v = value()?;
                        if v.is_empty() || v.starts_with('-') {
                            return Err(format!("bad --world `{v}`: want a world id\n\n{USAGE}"));
                        }
                        world = Some(v.to_string());
                    }
                    other => return Err(format!("unknown flag `{other}` for `debug {preset}`\n\n{USAGE}")),
                }
            }
            if join.is_some() && (reset || port.is_some()) {
                return Err(format!("--join plays on another client's world: not with --reset or --port\n\n{USAGE}"));
            }
            match *preset {
                "world"
                    if world.is_none()
                        && !(join.is_some() && (prefab.is_some() || town.is_some()))
                        && !(prefab.is_some() && town.is_some()) =>
                {
                    Ok(Some(DebugScenario::World { reset, port, join, site, prefab, town }))
                }
                "online"
                    if !reset
                        && port.is_none()
                        && join.is_none()
                        && site.is_none()
                        && prefab.is_none()
                        && town.is_none() =>
                {
                    Ok(Some(DebugScenario::Online { world: world.unwrap_or_else(|| DEFAULT_ONLINE_WORLD.into()) }))
                }
                "world" | "online" => Err(format!("flags not valid for `debug {preset}`\n\n{USAGE}")),
                other => Err(format!("unknown debug preset `{other}`\n\n{USAGE}")),
            }
        }
        [other, ..] => Err(format!("unknown argument `{other}`\n\n{USAGE}")),
    }
}

/// The first site the Adventure tab would offer — a populated site of the
/// lowest civ, in the order the server lists them.
pub fn choose_adventure_site(sites: &[SiteMarker]) -> Option<SiteId> {
    sites.iter().filter(|s| populated(s)).min_by_key(|s| s.civ).map(|s| s.site)
}

/// What the automatic start does next, from what the server has sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Start {
    /// The server's `Sites` have not arrived.
    Wait,
    /// No populated site (a world without history): play where we spawned.
    PlayHere,
    /// Start an adventure here.
    Adventure(SiteId),
}

/// `received`: the server's `Sites` have been applied ([`HistoryView::sites_received`]).
fn adventure_start(sites: &[SiteMarker], received: bool) -> Start {
    if !received {
        return Start::Wait;
    }
    match choose_adventure_site(sites) {
        Some(site) => Start::Adventure(site),
        None => Start::PlayHere,
    }
}

/// Where the scenario is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Not started (the first frame).
    Boot,
    /// The world is being forged ([`DebugForge`]).
    Forging,
    /// `online`: on the login screens, before the session starts.
    Online(OnlineStep),
    /// Starting / in the world, the adventure not settled yet.
    Starting(Sent),
    /// Finished (or abandoned): the app behaves as after a manual start.
    Done,
}

/// How far the `online` preset's login is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnlineStep {
    /// Waiting for the login screen, then the kept login is presented.
    Login,
    /// The intent is written; one frame for the login flow to read it (before that a
    /// not-yet-busy session looks like a refused one).
    Presented,
    /// The kept login is on its way; once on the world list (and its fetch answered) the join is.
    LoggingIn,
    /// The join is sent; its ticket starts the session.
    Joining,
}

/// The adventure request out, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Nothing,
    /// A StartAdventure.
    Adventure,
}

#[derive(Resource, Debug)]
pub struct DebugLaunch {
    scenario: DebugScenario,
    phase: Phase,
}

/// The `world` preset's world forge.
#[derive(Resource)]
struct DebugForge(ForgeJob);

/// Runs the scenario once, from the first frame; the menu flow is untouched
/// otherwise (leaving the world returns to the main menu as usual).
pub struct DebugLaunchPlugin {
    pub scenario: DebugScenario,
}

impl DebugLaunchPlugin {
    /// The plugin for `scenario`, once the environment has what it needs
    /// (`online`: the login service and a kept login). An error explains what
    /// is missing (like a bad command line, before any window opens).
    pub fn from_env(scenario: DebugScenario) -> Result<Self, String> {
        if matches!(scenario, DebugScenario::Online { .. }) {
            online_login_from(crate::login::login_url().as_deref(), crate::login::store::load_refresh().as_deref())?;
        }
        Ok(Self { scenario })
    }
}

impl Plugin for DebugLaunchPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DebugLaunch { scenario: self.scenario.clone(), phase: Phase::Boot })
            .add_systems(Update, (boot, drive_online_login, poll_forge, auto_start).chain());
        // An online session ends on the world list instead of the menu (or,
        // its login revoked during a rejoin, on the sign-in screen).
        for home in [AppState::MainMenu, AppState::Worlds, AppState::Login] {
            app.add_systems(OnTransition { exited: AppState::Starting, entered: home }, (exit_when_lost, abandon).chain())
                .add_systems(OnTransition { exited: AppState::InWorld, entered: home }, (exit_when_lost, abandon).chain());
        }
        // A rejoin (`login::flow`): the new session starts again.
        app.add_systems(OnTransition { exited: AppState::InWorld, entered: AppState::Starting }, restart_after_rejoin);
    }
}

fn worlds_root() -> PathBuf {
    forged::from_repo_root(Path::new(world_store::WORLDS_DIR))
}

/// The session ended before the scenario settled: it does not come back.
fn abandon(mut launch: ResMut<DebugLaunch>, mut commands: Commands, forge: Option<Res<DebugForge>>) {
    if let Some(forge) = forge {
        forge.0.cancel();
        commands.remove_resource::<DebugForge>();
    }
    launch.phase = Phase::Done;
}

/// `online`: a session that ended where a screen session leaves the game
/// ends the launch: why is printed, and the app exits non-zero. Keyed on the
/// outcome, never on a screen line: a rejoin that gave up or was signed out
/// ([`RejoinEnd`]), or a session lost for a reason it does not rejoin on
/// ([`LastSessionError`]). A session the player left (no reason) does not.
fn exit_when_lost(
    launch: Res<DebugLaunch>,
    login: Option<Res<LoginSession>>,
    last: Res<LastSessionError>,
    mut exit: MessageWriter<AppExit>,
) {
    if !matches!(launch.scenario, DebugScenario::Online { .. }) || matches!(launch.phase, Phase::Boot | Phase::Online(_)) {
        return;
    }
    let Some(login) = login else { return };
    let why = match (&login.rejoin_ended, &last.0) {
        (Some(RejoinEnd::GaveUp(why)), _) => format!("the rejoin gave up: {why}"),
        (Some(RejoinEnd::SignedOut), _) => "the login was revoked during a rejoin".to_string(),
        (None, Some(reason)) => reason.clone(),
        (None, None) => return,
    };
    // A session's end reason or the screens' line: never a secret.
    error!("[debug] online: the session is over: {why}");
    eprintln!("client debug online: the session is over: {why}");
    exit.write(AppExit::error());
}

/// `online`: the lost session rejoins (`login::flow`); its new session
/// starts again (a restored player is already adventuring).
fn restart_after_rejoin(mut launch: ResMut<DebugLaunch>) {
    if matches!(launch.scenario, DebugScenario::Online { .. }) && matches!(launch.phase, Phase::Starting(_) | Phase::Done) {
        info!("[debug] the session rejoins; starting again once it is in");
        launch.phase = Phase::Starting(Sent::Nothing);
    }
}

/// First frame: reuse the scenario's world, or forge it (`--reset` deletes it first).
fn boot(
    mut launch: ResMut<DebugLaunch>,
    mut commands: Commands,
    mut next: ResMut<NextState<AppState>>,
    mut error: ResMut<LastSessionError>,
    mut status: ResMut<StartingStatus>,
) {
    if launch.phase != Phase::Boot {
        return;
    }
    let (reset, port, join, prefab, town) = match &launch.scenario {
        DebugScenario::World { reset, port, join, prefab, town, .. } => (*reset, *port, *join, prefab.clone(), town.clone()),
        DebugScenario::Online { world } => {
            // The Play online flow from its login screen: `drive_online_login` presses its buttons.
            info!("[debug] logging in to join the login service's world {world}");
            launch.phase = Phase::Online(OnlineStep::Login);
            next.set(AppState::Login);
            return;
        }
    };
    if let Some(addr) = join {
        info!("[debug] joining the server on {addr}");
        commands.insert_resource(crate::session::JoinServer(addr));
        launch.phase = Phase::Starting(Sent::Nothing);
        next.set(AppState::Starting);
        return;
    }
    let prefab_given = prefab.is_some();
    if let Some(spec) = prefab {
        info!("[debug] --prefab {spec}: beside the landing of the home region");
        commands.insert_resource(crate::local_host::DevStructures(format!("home={spec}")));
    }
    let town_given = town.is_some();
    if let Some(spec) = town {
        info!("[debug] --town {spec}: a town beside the landing of the home region");
        commands.insert_resource(crate::local_host::DevStructures(format!("town={spec}")));
    }
    if let Some(port) = port {
        info!("[debug] hosting the world's server on UDP port {port}");
        commands.insert_resource(crate::session::HostPort(port));
    }
    let root = worlds_root();
    let fail = |why: String, launch: &mut DebugLaunch, error: &mut LastSessionError| {
        error!("[debug] {why}");
        error.0 = Some(why);
        launch.phase = Phase::Done;
    };
    let custom = debug_world_override();
    let dir = root.join(custom.as_deref().unwrap_or(DEBUG_WORLD_SLUG));
    if custom.is_some() && (reset || !dir.exists()) {
        let why = format!(
            "VOXEL_DEBUG_WORLD: {} must be an existing world and is never reset or forged",
            dir.display()
        );
        return fail(why, &mut launch, &mut error);
    }
    if prefab_given && !reset && dir.exists() {
        warn!(
            "[debug] --prefab on the existing world {}: regions saved earlier keep their old terrain, only regions never visited show the prefab (add --reset to re-forge)",
            dir.display()
        );
    }
    if town_given && !reset && dir.exists() {
        warn!(
            "[debug] --town on the existing world {}: regions saved earlier keep their old terrain, only regions never visited show the town (add --reset to re-forge)",
            dir.display()
        );
    }
    if reset && dir.exists() {
        info!("[debug] --reset: deleting {}", dir.display());
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            return fail(format!("could not delete {}: {e}", dir.display()), &mut launch, &mut error);
        }
    }
    if dir.exists() {
        let entry = world_store::list(&root).into_iter().find(|e| e.dir == dir);
        let Some(entry) = entry else {
            return fail(format!("{} is not a world", dir.display()), &mut launch, &mut error);
        };
        if let Err(why) = entry.loadable {
            return fail(
                format!(
                    "the debug world {} cannot be loaded ({why}); run with --reset (`just run --reset`) to re-forge it",
                    dir.display()
                ),
                &mut launch,
                &mut error,
            );
        }
        info!("[debug] reusing {}", dir.display());
        commands.insert_resource(PendingWorld {
            entry_dir: dir,
            terrain: entry.terrain,
            name: DEBUG_WORLD_NAME.into(),
        });
    } else {
        info!("[debug] forging the debug world into {}", dir.display());
        commands.insert_resource(DebugForge(ForgeJob::start(root, ForgeParams::default(), DEBUG_WORLD_NAME.into())));
        commands.insert_resource(PreparingWorld);
        status.0 = "Forging the debug world\u{2026}".into();
        launch.phase = Phase::Forging;
        next.set(AppState::Starting);
        return;
    }
    launch.phase = Phase::Starting(Sent::Nothing);
    next.set(AppState::Starting);
}

/// What `online`'s login step does next: `None` = wait, `Some(Ok)` = logged in (on the world
/// list), `Some(Err)` = give up. A kept token the service refuses is dropped silently by the
/// flow, so "back on the login screen, idle, not logged in" with no error line is a refusal.
fn online_login_verdict(state: AppState, busy: bool, logged_in: bool, error: &str) -> Option<Result<(), String>> {
    if busy {
        return None;
    }
    // The error first: a failed world-list fetch leaves the screen on the list,
    // logged in, with its line set — a login failure, not a join one.
    if !error.is_empty() {
        return Some(Err(format!("login failed: {error}")));
    }
    if state == AppState::Worlds && logged_in {
        return Some(Ok(()));
    }
    if state == AppState::Login && !logged_in {
        return Some(Err("login failed: the kept login was refused".into()));
    }
    None
}

/// `online`: logs in and joins through the login screens' own flow
/// (`login::flow`). A failure (the screens' error line, with nothing in
/// flight) ends the launch: printed, and the app exits non-zero.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn drive_online_login(
    mut launch: ResMut<DebugLaunch>,
    state: Res<State<AppState>>,
    login: Option<Res<LoginSession>>,
    mut intents: MessageWriter<LoginIntent>,
    mut exit: MessageWriter<AppExit>,
) {
    let Phase::Online(step) = launch.phase else { return };
    let DebugScenario::Online { world } = &launch.scenario else { return };
    let mut fail = |why: &str, launch: &mut DebugLaunch| {
        // The screens' line (a `LoginFailure`): never a secret.
        error!("[debug] online: {why}");
        eprintln!("client debug online: {why}");
        exit.write(AppExit::error());
        launch.phase = Phase::Done;
    };
    let Some(login) = login else {
        return fail("no login service configured", &mut launch);
    };
    let state = *state.get();
    if step == OnlineStep::Joining && !login.busy() && !login.error.is_empty() {
        let why = format!("join failed: {}", login.error);
        return fail(&why, &mut launch);
    }
    match step {
        OnlineStep::Login => {
            if state != AppState::Login {
                return; // the login screen first: its answer moves on only from there
            }
            // Entering the login screens by a state change sends nothing on native: the
            // menu's Play online is what refreshes the kept login, so press it.
            info!("[debug] starting from the kept login");
            intents.write(LoginIntent::PlayOnline);
            launch.phase = Phase::Online(OnlineStep::Presented);
        }
        OnlineStep::Presented => launch.phase = Phase::Online(OnlineStep::LoggingIn),
        OnlineStep::LoggingIn => {
            match online_login_verdict(state, login.busy(), login.logged_in(), &login.error) {
                None => {}
                Some(Err(why)) => return fail(&why, &mut launch),
                // On the list, the list fetched: a join sent now is not refused as busy.
                Some(Ok(())) => {
                    info!("[debug] logged in; joining world {world}");
                    intents.write(LoginIntent::JoinWorld(world.clone()));
                    launch.phase = Phase::Online(OnlineStep::Joining);
                }
            }
        }
        OnlineStep::Joining => {
            // The ticket's `PendingJoin` starts the session (`AppState::Starting`).
            if state == AppState::Starting {
                info!("[debug] joined world {world}; starting an adventure");
                launch.phase = Phase::Starting(Sent::Nothing);
            }
        }
    }
}

/// The forge's progress on the starting screen; its world, once written, starts the session.
fn poll_forge(
    mut launch: ResMut<DebugLaunch>,
    forge: Option<Res<DebugForge>>,
    preparing: Option<Res<PreparingWorld>>,
    mut commands: Commands,
    mut status: ResMut<StartingStatus>,
    mut ended: MessageWriter<SessionEnded>,
) {
    if launch.phase != Phase::Forging {
        return;
    }
    let Some(forge) = forge else { return };
    if preparing.is_none() {
        // The start was abandoned (window closed, failed) while forging.
        forge.0.cancel();
        commands.remove_resource::<DebugForge>();
        launch.phase = Phase::Done;
        return;
    }
    for event in forge.0.events.try_iter() {
        match forge.0.settle(event) {
            ForgeEvent::Progress(p) => status.0 = format!("Forging the debug world: {}", progress_text(p).1),
            ForgeEvent::Done(world) => {
                info!("[debug] forged {}", world.entry_dir.display());
                commands.remove_resource::<DebugForge>();
                commands.remove_resource::<PreparingWorld>();
                commands.insert_resource(world);
                status.0.clear();
                launch.phase = Phase::Starting(Sent::Nothing);
                return;
            }
            ForgeEvent::Failed(why) => {
                commands.remove_resource::<DebugForge>();
                ended.write(SessionEnded { reason: Some(why) });
                launch.phase = Phase::Done;
                return;
            }
            ForgeEvent::Cancelled => {
                commands.remove_resource::<DebugForge>();
                launch.phase = Phase::Done;
                return;
            }
        }
    }
}

/// In the world and not yet playing: start an adventure at `--site`, else at
/// the first populated site; a world with no populated site is played where
/// the player spawned. The result handling (`apply_adventure_result`) sets
/// `InPlay` on success.
fn auto_start(
    mut launch: ResMut<DebugLaunch>,
    state: Res<State<AppState>>,
    in_play: Option<ResMut<InPlay>>,
    mut history: ResMut<HistoryView>,
    requests: Option<Res<ClientRequests>>,
    cam: Option<ResMut<CommanderCamera>>,
) {
    let Phase::Starting(sent) = launch.phase else { return };
    if *state.get() != AppState::InWorld {
        return;
    }
    let Some(mut in_play) = in_play else { return };
    let wanted = launch.scenario.site();
    if in_play.0 {
        // `just run-two` reads this line to send client 2 to the same site.
        match history.adventuring {
            Some(site) => {
                info!("[debug] playing: adventure site={}", site.0);
                if let Some(want) = wanted.filter(|w| *w != site) {
                    warn!(
                        "[debug] --site {} asked for, but the server restored the player into the adventure at site {}",
                        want.0, site.0
                    );
                }
            }
            None => info!("[debug] playing"),
        }
        launch.phase = Phase::Done;
        return;
    }
    // A restored player (the server kept its adventure across the disconnect /
    // restart) is told so on connect, as the answer a start would have had:
    // wait for that to be applied, never start again.
    if sent == Sent::Nothing && (history.adventuring.is_some() || history.adventure_result.is_some()) {
        return;
    }
    let Some(requests) = requests else { return };
    if sent == Sent::Adventure {
        if let Some((why, _)) = history.adventure_refusal {
            error!("[debug] adventure refused ({why:?}); the arrival map is yours");
            launch.phase = Phase::Done;
        }
        return;
    }
    let site = match (wanted, adventure_start(&history.sites, history.sites_received)) {
        (Some(site), _) => site,
        (None, Start::Wait) => return,
        (None, Start::PlayHere) => {
            info!("[debug] playing: no populated site");
            in_play.0 = true;
            if let Some(mut cam) = cam {
                cam.placed = false;
            }
            launch.phase = Phase::Done;
            return;
        }
        (None, Start::Adventure(site)) => site,
    };
    if requests.0.send(ClientRequest::StartAdventure(site)).is_err() {
        warn!("[debug] adventure request not sent: no game thread");
        launch.phase = Phase::Done;
        return;
    }
    info!("[debug] starting an adventure at site {}", site.0);
    // As the Adventure tab does: a stale result must never drive the UI.
    history.adventure_result = None;
    history.adventure_refusal = None;
    history.adventure_pending = true;
    launch.phase = Phase::Starting(Sent::Adventure);
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::RegionCoords;

    #[test]
    fn no_arguments_is_the_main_menu() {
        assert_eq!(parse_args::<&str>(&[]), Ok(None));
    }

    #[test]
    fn debug_world_parses_with_and_without_reset() {
        let plain = |reset| Ok(Some(DebugScenario::World { reset, port: None, join: None, site: None, prefab: None, town: None }));
        assert_eq!(parse_args(&["debug", "world"]), plain(false));
        assert_eq!(parse_args(&["debug", "world", "--reset"]), plain(true));
    }

    #[test]
    fn debug_world_hosts_or_joins() {
        assert_eq!(
            parse_args(&["debug", "world", "--reset", "--port", "6470"]),
            Ok(Some(DebugScenario::World { reset: true, port: Some(6470), join: None, site: None, prefab: None, town: None }))
        );
        assert_eq!(
            parse_args(&["debug", "world", "--join", "127.0.0.1:6470", "--site", "3"]),
            Ok(Some(DebugScenario::World {
                reset: false,
                port: None,
                join: Some("127.0.0.1:6470".parse().unwrap()),
                site: Some(SiteId(3)),
                prefab: None,
                town: None,
            }))
        );
        let gone = parse_args(&["debug", "world", "--adventure"]).unwrap_err();
        assert!(gone.contains("--site") && gone.contains("usage: client"), "{gone}");
        for bad in [
            &["debug", "world", "--port"][..],
            &["debug", "world", "--port", "x"],
            &["debug", "world", "--join", "nowhere"],
            &["debug", "world", "--join", "127.0.0.1:1", "--reset"],
            &["debug", "world", "--site", "x"],
            &["debug", "world", "--site", "3", "--site", "4"],
        ] {
            assert!(parse_args(bad).unwrap_err().contains("usage: client"), "{bad:?}");
        }
    }

    #[test]
    fn debug_world_collects_prefabs() {
        let with = |p: &str| {
            Ok(Some(DebugScenario::World {
                reset: true,
                port: None,
                join: None,
                site: None,
                prefab: Some(p.into()),
                town: None,
            }))
        };
        assert_eq!(parse_args(&["debug", "world", "--reset", "--prefab", "dev-hut"]), with("dev-hut"));
        assert_eq!(
            parse_args(&["debug", "world", "--reset", "--prefab", "dev-hut", "--prefab", "statuen-06:40,0:1"]),
            with("dev-hut;statuen-06:40,0:1")
        );
        for bad in [
            &["debug", "world", "--prefab"][..],
            &["debug", "world", "--prefab", ""],
            &["debug", "world", "--prefab", "--reset"],
            &["debug", "world", "--join", "127.0.0.1:1", "--prefab", "dev-hut"],
        ] {
            assert!(parse_args(bad).unwrap_err().contains("usage: client"), "{bad:?}");
        }
    }

    #[test]
    fn debug_world_takes_a_town() {
        let with = |t: &str| {
            Ok(Some(DebugScenario::World {
                reset: true,
                port: None,
                join: None,
                site: None,
                prefab: None,
                town: Some(t.into()),
            }))
        };
        assert_eq!(parse_args(&["debug", "world", "--reset", "--town", "medieval"]), with("medieval"));
        assert_eq!(parse_args(&["debug", "world", "--reset", "--town", "medieval:2:64"]), with("medieval:2:64"));
        for bad in [
            &["debug", "world", "--town"][..],
            &["debug", "world", "--town", ""],
            &["debug", "world", "--town", "--reset"],
            &["debug", "world", "--town", "a", "--town", "b"],
            &["debug", "world", "--town", "a", "--prefab", "dev-hut"],
            &["debug", "world", "--prefab", "dev-hut", "--town", "a"],
            &["debug", "world", "--join", "127.0.0.1:1", "--town", "a"],
            &["debug", "online", "--town", "a"],
        ] {
            assert!(parse_args(bad).unwrap_err().contains("usage: client"), "{bad:?}");
        }
    }

    #[test]
    fn debug_online_parses_its_world() {
        let online = |world: &str| Ok(Some(DebugScenario::Online { world: world.into() }));
        assert_eq!(parse_args(&["debug", "online"]), online(DEFAULT_ONLINE_WORLD));
        assert_eq!(parse_args(&["debug", "online", "--world", "x"]), online("x"));
        for bad in [
            &["debug", "online", "--world"][..],
            &["debug", "online", "--world", "--reset"],
            &["debug", "online", "--reset"],
            &["debug", "online", "--port", "6470"],
            &["debug", "online", "--join", "127.0.0.1:6470"],
            &["debug", "online", "--site", "3"],
            &["debug", "online", "--wat"],
            &["debug", "world", "--world", "x"],
        ] {
            assert!(parse_args(bad).unwrap_err().contains("usage: client"), "{bad:?}");
        }
    }

    #[test]
    fn adventure_picks_the_first_populated_site_of_the_lowest_civ() {
        use game::history::CivId;
        let site = |id, civ, population, abandoned| SiteMarker {
            site: SiteId(id),
            civ: CivId(civ),
            region: RegionCoords::new(0, 0),
            name: String::new(),
            population,
            abandoned,
        };
        let sites = [site(0, 1, 5, false), site(1, 0, 0, false), site(2, 0, 4, true), site(3, 0, 2, false), site(4, 0, 9, false)];
        assert_eq!(choose_adventure_site(&sites), Some(SiteId(3)));
        assert_eq!(choose_adventure_site(&sites[1..3]), None, "empty or abandoned: nowhere to start");
    }

    #[test]
    fn first_populated_or_play_here() {
        let populated_site = |id| SiteMarker {
            site: SiteId(id),
            civ: game::history::CivId(0),
            region: RegionCoords::new(0, 0),
            name: String::new(),
            population: 4,
            abandoned: false,
        };
        assert_eq!(adventure_start(&[], true), Start::PlayHere);
        assert_eq!(adventure_start(&[populated_site(3)], true), Start::Adventure(SiteId(3)));
        assert_eq!(adventure_start(&[], false), Start::Wait, "Sites not received yet");
    }

    #[test]
    fn online_needs_a_login_url_and_a_kept_token() {
        let url = Some("http://127.0.0.1:8790");
        assert_eq!(online_login_from(url, Some("kept")), Ok(()));
        let no_url = online_login_from(None, Some("kept")).unwrap_err();
        assert!(no_url.contains("VOXEL_LOGIN_URL") && no_url.contains("usage: client"), "{no_url}");
        let no_kept = online_login_from(url, None).unwrap_err();
        assert!(
            no_kept.contains(
                "`debug online` needs a kept login: set VOXEL_LOGIN_REFRESH_FILE to a file made by `login account issue-refresh`"
            ),
            "{no_kept}"
        );
    }

    #[test]
    fn online_verdict_table() {
        use AppState::{Login, Worlds};
        assert_eq!(online_login_verdict(Login, true, false, ""), None, "busy: wait");
        assert_eq!(online_login_verdict(Worlds, false, true, ""), Some(Ok(())));
        assert_eq!(
            online_login_verdict(Login, false, false, ""),
            Some(Err("login failed: the kept login was refused".into()))
        );
        let net = online_login_verdict(Login, false, false, "network: connection refused").unwrap().unwrap_err();
        assert!(net.contains("network: connection refused"), "{net}");
        assert_eq!(online_login_verdict(Worlds, false, false, ""), None, "on the list, fetch pending: wait");
        // The world list could not be fetched: a login failure, on either screen.
        assert_eq!(
            online_login_verdict(Worlds, false, true, "worlds: HTTP 500"),
            Some(Err("login failed: worlds: HTTP 500".into()))
        );
        assert_eq!(
            online_login_verdict(Login, false, true, "worlds: HTTP 500"),
            Some(Err("login failed: worlds: HTTP 500".into()))
        );
    }

    #[test]
    fn bad_command_lines_explain_themselves() {
        for bad in [&["debug"][..], &["debug", "nope"], &["debug", "world", "--wat"], &["play"], &["--help"]] {
            let err = parse_args(bad).unwrap_err();
            assert!(err.contains("usage: client"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn the_debug_name_slugs_to_the_debug_dir() {
        let empty = std::env::temp_dir().join("mmo-debug-launch-no-such-root");
        assert_eq!(world_store::slug_for(&empty, DEBUG_WORLD_NAME), DEBUG_WORLD_SLUG);
    }
}
