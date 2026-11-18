//! Coordinator test helpers shared by `coordinator_rt.rs` and `roles.rs` (each test binary
//! includes this file as a module of its own): what a restart must resume, the row loaded as a
//! restart loads it, a host's `Attach`.
#![allow(dead_code)]

use game::history::CalendarTick;
use world_store::StoreHandle;

/// What a restart must resume exactly: the calendar (history tick and the calendar's anchor),
/// the site markers, the legends' length.
#[derive(Debug, PartialEq)]
pub struct Resumed {
    pub now: CalendarTick,
    pub anchor: ((u64, CalendarTick), u64),
    pub sites: Vec<game::SiteMarker>,
    pub legends: usize,
}

pub fn resumed(c: &game::Coordinator) -> Resumed {
    Resumed { now: c.history().now(), anchor: c.calendar_anchor(), sites: c.history().site_markers(), legends: c.history().log().len() }
}

/// The coordinator state the store holds, loaded as a restart loads it (as replica 0, at store
/// time 0); `None` before its first write.
pub fn stored_core(store: &StoreHandle, map: &game::WorldMap) -> Option<game::Coordinator> {
    let row = store.load_coordinator().unwrap()?;
    let grid = game::history::SocietyGrid::from_world_map(map);
    Some(game::Coordinator::load(&row.core, &row.history, grid, game::coordinator::ReplicaId(0), 0).expect("the projection loads"))
}

/// A 48 × 48 grassland with a sea border (a bootstrap settles it).
pub fn land_map() -> game::WorldMap {
    let mut tiles = Vec::new();
    for z in 0..48 {
        for x in 0..48 {
            let sea = x < 2 || z < 2 || x >= 46 || z >= 46;
            tiles.push(game::TileSummary {
                ground_min_m: 0.0,
                ground_mean_m: 10.0,
                ground_max_m: 10.0,
                relief_m: 10.0,
                slope: [0.0, 0.0],
                sea_fraction: if sea { 1.0 } else { 0.0 },
                lake_fraction: 0.0,
                lake_level_m: None,
                rivers: 1,
                river_max_discharge_m2: 0.0,
                river_max_width_m: 0.0,
                surface_rock: None,
                landing: Some([256, 256]),
                biome: if sea { game::Biome::Ocean } else { game::Biome::Grassland },
                region: 0,
                temperature_c: 15.0,
                precip_mm: 800.0,
                savagery: 0,
                alignment: 0,
                volcano: false,
            });
        }
    }
    game::WorldMap {
        origin: game::RegionCoords::new(0, 0),
        width: 48,
        height: 48,
        tiles,
        rock_names: vec![],
        region_types: vec![],
    }
}

/// A coordinator replica as `run_coordinator` starts one, on a SQLite store in `dir` (world
/// `world`, its listeners' identity in `pin_file` and beside it), listening for the login
/// service — and no host ever attaches to it: it has no gate host (Elastic 2b Task 8). Its
/// store (shut down after the replica) and the replica.
pub fn gateless_replica(
    dir: &std::path::Path,
    token: &str,
    world: &str,
    pin_file: &std::path::Path,
) -> (StoreHandle, server::coordinator_rt::ListeningCoordinator) {
    std::fs::create_dir_all(dir).unwrap();
    let url = world_store::StoreUrl::Sqlite(dir.join("w.sqlite"));
    let store = StoreHandle::open(&url, world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap();
    let start = server::world_file::ensure_coordinator_state(&store, &land_map(), None).unwrap().unwrap();
    let login = server::coordinator_rt::LoginListen {
        bind: "127.0.0.1:0".parse().unwrap(),
        token: token.into(),
        cert_hash_file: Some(pin_file.to_path_buf()),
    };
    let listening = server::coordinator_rt::start_listening_as(
        store.clone(),
        start,
        "127.0.0.1:0".parse().unwrap(),
        token.into(),
        Some(dir.join("coordinator.cert-sha256")),
        server::claims::Fenced::default(),
        None,
        game::CrashPolicy::Rollback,
        world,
        Some(login),
    )
    .expect("the coordinator listens");
    (store, listening)
}

/// Host `host`'s `Attach` as boot `boot` at `epoch` and world generation `generation`, `online`
/// online, nothing running or handed, `pending_cut` its last unanswered cut, under `policy`.
pub fn attach_msg(
    host: game::HostId,
    epoch: u64,
    online: Vec<game::OnlinePlayer>,
    generation: u64,
    boot: u64,
    pending_cut: Option<game::CutId>,
    policy: game::CrashPolicy,
) -> game::HostMsg {
    game::HostMsg::Attach {
        host,
        epoch,
        running: Vec::new(),
        online,
        generation,
        boot,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut,
        successions: Vec::new(),
        policy,
    }
}
