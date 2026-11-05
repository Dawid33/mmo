//! Debug launches: `client debug <preset>` skips the manual setup and starts in
//! a prepared scenario. A plain `client` opens on the main menu.
//!
//! Presets:
//! - `embarked` — a small generated world (`ForgeParams::default()`: the 16 km /
//!   32 m continent) in `worlds/debug-embarked/`, played with a fortress embarked.
//!   The world and its fortress are REUSED between runs: an existing world is
//!   loaded (not re-forged) and the client returns to the fortress it founded
//!   there (the server keeps it in the world's `terrain.history`). Only deleting
//!   the world directory or `--reset` starts over (re-forge + a fresh embark).
//!   `VOXEL_DEBUG_WORLD=<slug>` (native) plays an existing `worlds/<slug>/`
//!   instead — e.g. a big benchmark world — and is never forged or reset.
//!   Several clients on one server (`just run-two`, native): `--port N` hosts
//!   the world's server on UDP port N instead of a free one, and
//!   `--join HOST:PORT` skips the world (no forge, no server of its own) and
//!   joins that server; either way the client embarks automatically, returning
//!   to the first player-founded fortress no other connected player holds.
//!   `--adventure-at SITE` starts an adventure at that site instead of
//!   embarking (`just run-two`: client 2 adventures in client 1's fortress,
//!   whose id client 1 logs as `[debug] embarked: fortress site=N`).
//!
//! The scenario drives the ordinary flow, nothing is faked: the world goes
//! through the generator's forge job and `PendingWorld` → `AppState::Starting`
//! (its own local server), and the embark is the arrival map's requests
//! (`ReturnTo` / `Embark`), answered by the server as for a player.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use game::history::rules::EMBARK_PLAYER_PARTY_MAX;
use game::history::{SiteId, SocietyGrid};
use game::{EmbarkOptions, EmbarkRequest, RegionCoords, SiteMarker, WorldMap};
use worldgen::forge::ForgeParams;
use worldgen::{forged, world_store};

use crate::frontend::generator::{progress_text, ForgeEvent, ForgeJob};
use crate::instance::ClientRequest;
use crate::renderer::commander::CommanderCamera;
use crate::renderer::history_view::{embark_tile_check, HistoryView};
use crate::renderer::world_map::WorldMapState;
use crate::renderer::ClientRequests;
use crate::session::{AppState, Embarked, LastSessionError, PendingWorld, PreparingWorld, SessionEnded, StartingStatus};

/// `worlds/<this>/`: the `embarked` preset's world.
pub const DEBUG_EMBARKED_SLUG: &str = "debug-embarked";
/// Its display name (`world_store::slug_for` turns it into [`DEBUG_EMBARKED_SLUG`]).
pub const DEBUG_EMBARKED_NAME: &str = "Debug: embarked";

/// `VOXEL_DEBUG_WORLD=<slug>`: play the `embarked` preset in an existing
/// `worlds/<slug>/` instead of [`DEBUG_EMBARKED_SLUG`] (benchmarks on a bigger
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

pub const USAGE: &str = "usage: client [debug <preset> [--reset | --port N | --join HOST:PORT] [--adventure-at SITE]]\n\
    \n\
    With no arguments the client opens on the main menu.\n\
    \n\
    presets:\n  \
      embarked   a small generated world with a fortress embarked (worlds/debug-embarked/);\n             \
                 reused between runs, --reset re-forges it and embarks afresh;\n             \
                 --port N hosts its server on UDP port N, --join HOST:PORT joins a\n             \
                 running server instead (several clients on one world); --adventure-at SITE\n             \
                 starts an adventure at site SITE (a number) instead of embarking";

/// A debug launch picked on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugScenario {
    /// `port`: host the world's server there. `join`: play on that running
    /// server instead of the world's own (never with `reset` or `port`).
    /// `adventure_at`: start an adventure at that site instead of embarking.
    Embarked { reset: bool, port: Option<u16>, join: Option<std::net::SocketAddr>, adventure_at: Option<SiteId> },
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
            let mut adventure_at = None;
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
                    "--adventure-at" => {
                        let v = value()?;
                        let site = v.parse::<u32>().map_err(|e| format!("bad --adventure-at `{v}`: {e}\n\n{USAGE}"))?;
                        adventure_at = Some(SiteId(site));
                    }
                    other => return Err(format!("unknown flag `{other}` for `debug {preset}`\n\n{USAGE}")),
                }
            }
            if join.is_some() && (reset || port.is_some()) {
                return Err(format!("--join plays on another client's world: not with --reset or --port\n\n{USAGE}"));
            }
            match *preset {
                "embarked" => Ok(Some(DebugScenario::Embarked { reset, port, join, adventure_at })),
                other => Err(format!("unknown debug preset `{other}`\n\n{USAGE}")),
            }
        }
        [other, ..] => Err(format!("unknown argument `{other}`\n\n{USAGE}")),
    }
}

/// `VOXEL_DEBUG_EMBARK_AT=x,z` (region coords, native): the automatic embark
/// takes the legal tile nearest this region instead of nearest the civ's
/// first site — a benchmark can put its fortress in the middle of the land.
fn embark_target_override() -> Option<RegionCoords> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let v = std::env::var("VOXEL_DEBUG_EMBARK_AT").ok()?;
        let (x, z) = v.split_once(',')?;
        Some(RegionCoords::new(x.trim().parse().ok()?, z.trim().parse().ok()?))
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

fn chebyshev(a: RegionCoords, b: RegionCoords) -> u64 {
    (i64::from(a.x) - i64::from(b.x)).unsigned_abs().max((i64::from(a.z) - i64::from(b.z)).unsigned_abs())
}

/// The automatic embark: the first civ offered, its first people (up to
/// `EMBARK_PLAYER_PARTY_MAX`, in the order offered), on the tile nearest that
/// civ's first offered site that the server's tile rule allows (ties: map
/// order). `None` without a civ, people or a legal tile.
pub fn choose_embark(options: &EmbarkOptions, map: &WorldMap, sites: &[SiteMarker]) -> Option<EmbarkRequest> {
    let civ = options.civs.first()?;
    let people: Vec<_> = civ
        .sites
        .iter()
        .flat_map(|s| s.people.iter().map(|p| p.person))
        .take(EMBARK_PLAYER_PARTY_MAX as usize)
        .collect();
    let home = embark_target_override().unwrap_or(civ.sites.first()?.region);
    if people.is_empty() {
        return None;
    }
    let grid = SocietyGrid::from_world_map(map);
    let region = (0..map.tiles.len())
        .map(|i| map.coords(i))
        .filter(|rc| embark_tile_check(&grid, sites, *rc).is_ok())
        .min_by_key(|rc| chebyshev(*rc, home))?;
    Some(EmbarkRequest { civ: civ.civ, people, region })
}

/// What the automatic embark does next, from what the server has sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmbarkMove {
    /// Return to this fortress (founded by an earlier run).
    Return(SiteId),
    Embark(EmbarkRequest),
    /// A world without history (nobody to embark): play where we spawned.
    PlayHere,
    /// Nothing legal: leave the arrival map to the player.
    GiveUp(String),
}

/// `tried`: fortresses a ReturnTo was already refused for this session (e.g.
/// held by another client on the same server): the next returnable one is
/// tried, then a new fortress is embarked.
pub fn next_move(
    options: &EmbarkOptions,
    map: &WorldMap,
    sites: &[SiteMarker],
    tried: &[SiteId],
) -> EmbarkMove {
    if options.civs.is_empty() && !sites.iter().any(|s| s.returnable) {
        return EmbarkMove::PlayHere;
    }
    if let Some(site) = sites.iter().find(|s| s.returnable && !tried.contains(&s.site)) {
        return EmbarkMove::Return(site.site);
    }
    match choose_embark(options, map, sites) {
        Some(req) => EmbarkMove::Embark(req),
        None => EmbarkMove::GiveUp("no civ, people or legal tile to embark with".into()),
    }
}

/// Where the scenario is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Not started (the first frame).
    Boot,
    /// The world is being forged ([`DebugForge`]).
    Forging,
    /// Starting / in the world, embark not settled yet.
    Embarking(Sent),
    /// Finished (or abandoned): the app behaves as after a manual start.
    Done,
}

/// The embark request out, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Nothing,
    Return,
    /// A ReturnTo was refused, then an Embark sent.
    Embark,
    /// A StartAdventure (`--adventure-at`).
    Adventure,
}

#[derive(Resource, Debug)]
pub struct DebugLaunch {
    scenario: DebugScenario,
    phase: Phase,
    /// Fortresses returned to (and refused) so far this session.
    tried: Vec<SiteId>,
}

/// The `embarked` preset's world forge.
#[derive(Resource)]
struct DebugForge(ForgeJob);

/// Runs the scenario once, from the first frame; the menu flow is untouched
/// otherwise (leaving the world returns to the main menu as usual).
pub struct DebugLaunchPlugin {
    pub scenario: DebugScenario,
}

impl Plugin for DebugLaunchPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DebugLaunch { scenario: self.scenario, phase: Phase::Boot, tried: Vec::new() })
            .add_systems(Update, (boot, poll_forge, auto_embark).chain())
            .add_systems(OnTransition { exited: AppState::Starting, entered: AppState::MainMenu }, abandon)
            .add_systems(OnTransition { exited: AppState::InWorld, entered: AppState::MainMenu }, abandon);
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
    let DebugScenario::Embarked { reset, port, join, .. } = launch.scenario;
    if let Some(addr) = join {
        info!("[debug] joining the server on {addr}");
        commands.insert_resource(crate::session::JoinServer(addr));
        launch.phase = Phase::Embarking(Sent::Nothing);
        next.set(AppState::Starting);
        return;
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
    let dir = root.join(custom.as_deref().unwrap_or(DEBUG_EMBARKED_SLUG));
    if custom.is_some() && (reset || !dir.exists()) {
        let why = format!(
            "VOXEL_DEBUG_WORLD: {} must be an existing world and is never reset or forged",
            dir.display()
        );
        return fail(why, &mut launch, &mut error);
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
            name: DEBUG_EMBARKED_NAME.into(),
        });
    } else {
        info!("[debug] forging the debug world into {}", dir.display());
        commands.insert_resource(DebugForge(ForgeJob::start(root, ForgeParams::default(), DEBUG_EMBARKED_NAME.into())));
        commands.insert_resource(PreparingWorld);
        status.0 = "Forging the debug world\u{2026}".into();
        launch.phase = Phase::Forging;
        next.set(AppState::Starting);
        return;
    }
    launch.phase = Phase::Embarking(Sent::Nothing);
    next.set(AppState::Starting);
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
                launch.phase = Phase::Embarking(Sent::Nothing);
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

/// In the world and not embarked: return to the fortress an earlier run
/// founded, else embark one. The arrival map's own result handling
/// (`slint_overlay::apply_embark_result`) closes the map on success.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
fn auto_embark(
    mut launch: ResMut<DebugLaunch>,
    state: Res<State<AppState>>,
    embarked: Option<ResMut<Embarked>>,
    mut history: ResMut<HistoryView>,
    world_map: Res<WorldMapState>,
    requests: Option<Res<ClientRequests>>,
    cam: Option<ResMut<CommanderCamera>>,
) {
    let Phase::Embarking(sent) = launch.phase else { return };
    if *state.get() != AppState::InWorld {
        return;
    }
    let Some(mut embarked) = embarked else { return };
    if embarked.0 {
        // `just run-two` reads the fortress line to send client 2 there.
        match (history.adventuring, history.fortress) {
            (Some(site), _) => info!("[debug] embarked: adventure at site={}", site.0),
            (None, Some(site)) => info!("[debug] embarked: fortress site={}", site.0),
            (None, None) => info!("[debug] embarked"),
        }
        launch.phase = Phase::Done;
        return;
    }
    // A restored player (the server kept its fortress or adventure across the
    // disconnect / restart) is told so on connect, as the answer an embark
    // would have had: wait for that to be applied, never embark again.
    if sent == Sent::Nothing
        && (history.fortress.is_some() || history.embark_result.is_some() || history.adventure_result.is_some())
    {
        return;
    }
    let (Some(map), Some(requests)) = (world_map.map.as_ref(), requests) else { return };
    let DebugScenario::Embarked { adventure_at, .. } = launch.scenario;
    if let Some(site) = adventure_at {
        // `apply_adventure_result` sets `Embarked` on success (seen next frame).
        if sent == Sent::Adventure {
            if let Some((why, _)) = history.adventure_refusal {
                error!("[debug] adventure at site {} refused ({why:?}); the arrival map is yours", site.0);
                launch.phase = Phase::Done;
            }
            return;
        }
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
        launch.phase = Phase::Embarking(Sent::Adventure);
        return;
    }
    if !history.options_requested && requests.0.send(ClientRequest::RequestEmbarkOptions).is_ok() {
        history.options_requested = true;
    }
    if history.embark_pending {
        return; // an answer is on its way
    }
    // Sites came with the connect, before the options answer: both are in now.
    let Some(options) = history.embark_options.clone() else { return };
    let refused = history.embark_refusal.map(|(why, _)| why);
    match (sent, refused) {
        (Sent::Nothing, _) => {}
        (Sent::Return, Some(why)) => {
            warn!("[debug] return to the fortress refused ({why:?}); trying another or embarking a new one");
        }
        (Sent::Embark | Sent::Adventure, Some(why)) => {
            error!("[debug] embark refused ({why:?}); the arrival map is yours");
            launch.phase = Phase::Done;
            return;
        }
        // Sent, answered Ok: `apply_embark_result` sets `Embarked` (seen next frame).
        (_, None) => return,
    }
    let request = match next_move(&options, map, &history.sites, &launch.tried) {
        EmbarkMove::Return(site) => {
            info!("[debug] returning to fortress {site:?}");
            launch.tried.push(site);
            launch.phase = Phase::Embarking(Sent::Return);
            ClientRequest::ReturnTo(site)
        }
        EmbarkMove::Embark(req) => {
            info!("[debug] embarking {} people of {:?} in region ({}, {})", req.people.len(), req.civ, req.region.x, req.region.z);
            launch.phase = Phase::Embarking(Sent::Embark);
            ClientRequest::Embark(req)
        }
        EmbarkMove::PlayHere => {
            info!("[debug] this world has no history: playing the spawn region");
            embarked.0 = true;
            if let Some(mut cam) = cam {
                cam.placed = false;
            }
            launch.phase = Phase::Done;
            return;
        }
        EmbarkMove::GiveUp(why) => {
            error!("[debug] cannot embark automatically: {why}; the arrival map is yours");
            launch.phase = Phase::Done;
            return;
        }
    };
    if requests.0.send(request).is_err() {
        warn!("[debug] embark request not sent: no game thread");
        launch.phase = Phase::Done;
        return;
    }
    // As the panel does: a stale result must never drive the UI.
    history.embark_result = None;
    history.embark_refusal = None;
    history.embark_pending = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_is_the_main_menu() {
        assert_eq!(parse_args::<&str>(&[]), Ok(None));
    }

    #[test]
    fn debug_embarked_parses_with_and_without_reset() {
        let plain = |reset| Ok(Some(DebugScenario::Embarked { reset, port: None, join: None, adventure_at: None }));
        assert_eq!(parse_args(&["debug", "embarked"]), plain(false));
        assert_eq!(parse_args(&["debug", "embarked", "--reset"]), plain(true));
    }

    #[test]
    fn debug_embarked_hosts_or_joins() {
        assert_eq!(
            parse_args(&["debug", "embarked", "--reset", "--port", "6470"]),
            Ok(Some(DebugScenario::Embarked { reset: true, port: Some(6470), join: None, adventure_at: None }))
        );
        assert_eq!(
            parse_args(&["debug", "embarked", "--join", "127.0.0.1:6470", "--adventure-at", "3"]),
            Ok(Some(DebugScenario::Embarked {
                reset: false,
                port: None,
                join: Some("127.0.0.1:6470".parse().unwrap()),
                adventure_at: Some(SiteId(3)),
            }))
        );
        for bad in [
            &["debug", "embarked", "--port"][..],
            &["debug", "embarked", "--port", "x"],
            &["debug", "embarked", "--join", "nowhere"],
            &["debug", "embarked", "--join", "127.0.0.1:1", "--reset"],
            &["debug", "embarked", "--adventure-at", "x"],
        ] {
            assert!(parse_args(bad).unwrap_err().contains("usage: client"), "{bad:?}");
        }
    }

    #[test]
    fn bad_command_lines_explain_themselves() {
        for bad in [&["debug"][..], &["debug", "nope"], &["debug", "embarked", "--wat"], &["play"], &["--help"]] {
            let err = parse_args(bad).unwrap_err();
            assert!(err.contains("usage: client"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn the_debug_name_slugs_to_the_debug_dir() {
        let empty = std::env::temp_dir().join("mmo-debug-launch-no-such-root");
        assert_eq!(world_store::slug_for(&empty, DEBUG_EMBARKED_NAME), DEBUG_EMBARKED_SLUG);
    }
}
