//! The server on the world store: a world restarts from its store as it was
//! saved at exit; the forge file is imported into the store as tiles and
//! deleted; the served world samples those tiles, and a tile that will not
//! load stops its region instead of making terrain; old file saves refuse the
//! start; the procedural world keeps nothing.
//!
//! Its own test binary: `server_world_is_tiled` installs the process-global
//! forged world (`worldgen::forged::install`, once per process), so no other
//! test here may touch it (they sample explicit worlds).
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver};
use game::{
    BlockId, ClientId, InlineLodBuilder, InlineSpawner, LodBuilder, RegionCoords, RegionOutput, RegionSpawner,
    ServerEvent, ServerPacket, WorldManager, SPAWN_REGION,
};
use server::claim_on_touch::ClaimOnTouch;
use server::player_store::{PlayerStore, StorePlayers};
use server::world_file::{self, open_world, WorldSource};
use worldgen::forge::ForgeParams;
use worldgen::forged::ForgedWorld;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmo-store-world-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A small forged world: seconds to forge, still a real world file.
fn small_params() -> ForgeParams {
    ForgeParams { size_m: 2048.0, cell_m: 64.0, seed: 4, ..ForgeParams::default() }
}

fn forge_file(path: &Path) -> Vec<u8> {
    let params = small_params();
    worldgen::forge::forge(&params).artifact(&params).save(path).unwrap();
    std::fs::read(path).unwrap()
}

fn source_for(world_file: &Path) -> WorldSource {
    world_file::resolve(Some(world_file.to_str().unwrap()), None).unwrap()
}

/// One server life: the manager core on inline regions, as `server::run`
/// wires it to the store (stored regions start from the store).
struct Core {
    manager: WorldManager<InlineSpawner>,
    /// The rig's one fence (loads at open, the exit's commit), as `server::run` shares it.
    claims: ClaimOnTouch,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    _packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

impl Core {
    /// Regions generate from `world` (explicitly: the process-global world is
    /// `server_world_is_tiled`'s).
    fn open(store: &world_store::StoreHandle, world: Arc<ForgedWorld>) -> Core {
        let materials = materials();
        Core::open_with(store, Box::new(move |rc| worldgen::generate_region_forged(&world, rc, materials)))
    }

    /// [`Core::open`] on `generate`.
    fn open_with(
        store: &world_store::StoreHandle,
        generate: Box<dyn FnMut(RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> + Send>,
    ) -> Core {
        let claims = ClaimOnTouch::new(store.clone());
        let mut regions = std::collections::BTreeMap::new();
        for rc in store.stored_regions().unwrap().into_keys() {
            regions.insert(rc, claims.load_region(rc).unwrap().expect("listed, so stored"));
        }
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
            water: Box::new(game::lod::LodWater::dry),
            out: lod_result_send,
        });
        let mut manager = WorldManager::new(
            InlineSpawner::with_portable(regions),
            generate,
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        manager.set_stored_regions(store.stored_regions().unwrap());
        manager.restore_in_flight(store.load_in_flight().unwrap());
        Core { manager, claims, region_out, _packets: packets }
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, 0);
            }
            self.manager.pump_lod_results();
        }
    }

    fn tick(&mut self, n: usize) {
        for _ in 0..n {
            self.manager.spawner_mut().tick_all();
            self.settle();
        }
    }

    /// The orderly exit: players and regions parked, the last cut committed
    /// (the store is shut down after).
    fn exit(mut self) -> game::Tick {
        self.manager.park_all_players(0);
        self.manager.shutdown_all();
        self.settle();
        assert!(self.manager.running_regions().is_empty(), "every region parked");
        let now = self.manager.spawner_mut().world_tick();
        let writer = server::cuts::CutWriter::new(self.claims.clone());
        assert!(writer.finish(&mut self.manager, now, Duration::from_secs(10)), "the exit's save cut commits");
        now
    }
}

fn materials() -> worldgen::material::Materials {
    let registry = game::BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    game::install_block_props(registry.props());
    worldgen::material::Materials::from_names(|name| registry.id_of(name).expect("block in the manifest"))
}

/// The world restarts from its store: a dug block stays dug, a slime is
/// there once, the player comes back once, and the world tick never goes
/// back.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn restart_restores_world() {
    let dir = scratch("restart");
    let world_path = dir.join("terrain.world");
    forge_file(&world_path);
    let source = source_for(&world_path);
    let opened = open_world(&source, &small_params()).expect("the world opens");
    let crc = opened.world.as_ref().unwrap().content_crc();
    let world = Arc::new(opened.world.expect("a forged world"));
    let store = opened.store.expect("a forged world has a store");
    let token = server::player_store::PlayerKey::Online(game::PlayerToken::generate([9; game::PLAYER_TOKEN_BYTES]));
    let (player, none) = StorePlayers(store.clone()).resolve(&token).unwrap();
    assert!(none.is_none());

    // Life 1: a player, a dug block and a slime in the spawn region.
    let mut core = Core::open(&store, world.clone());
    core.manager.handle_server_event(ServerEvent::ClientConnected { client: 0, player, record: None }, 0);
    core.settle();
    let centre = game::REGION_BLOCKS as i32 / 2;
    let (mut dug, mut slime) = (None, None);
    core.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        let v = centre * game::BLOCK_VOXELS as i32 + game::BLOCK_VOXELS as i32 / 2;
        let top = region.data().surface_voxel_y(v, v).expect("ground at the centre");
        let pos = [centre, top.div_euclid(game::BLOCK_VOXELS as i32), centre];
        assert_ne!(region.data().block_at(pos).map(|b| b.id), Some(BlockId::AIR), "a solid surface block");
        region.with_data(|d| assert!(d.set_block_safe(pos, game::Block::new(BlockId::AIR)), "dug"));
        dug = Some(pos);
        let y = (top + 3 * game::BLOCK_VOXELS as i32) as f32;
        let at = game::parry::math::Vector::new(v as f32 + 64.0, y, v as f32 + 64.0);
        let mut key = None;
        region.with_data(|d| key = Some(d.spawn_slime_safe(at, 1, 0.0f32.into(), 0)));
        slime = *region.data().ecs.id.try_get(key.unwrap());
    });
    let (dug, slime) = (dug.unwrap(), slime.expect("the slime has an EntityId"));
    core.tick(5);
    let exit_tick = core.exit();
    drop(world);

    // Life 2: the forge file was imported and is gone; the store has it all.
    assert!(!world_path.exists(), "the imported forge file was deleted");
    let opened = open_world(&source, &small_params()).expect("the world opens again");
    assert_eq!(opened.world.as_ref().unwrap().content_crc(), crc, "the same world, from the store");
    assert!(opened.world_tick >= exit_tick, "the world tick never goes back: {} < {exit_tick}", opened.world_tick);
    let world = Arc::new(opened.world.unwrap());
    let store = opened.store.unwrap();
    let (again, record) = StorePlayers(store.clone()).resolve(&token).unwrap();
    assert_eq!(again, player);
    let record = record.expect("the player's record was saved by the exit's cut");
    assert_eq!(record.region, SPAWN_REGION);
    let mut core = Core::open(&store, world.clone());
    assert!(core.manager.stored_regions().contains_key(&SPAWN_REGION));
    core.manager.handle_server_event(ServerEvent::ClientConnected { client: 0, player, record: Some(record) }, 0);
    // One tick: the region reports what it holds to the manager's directory.
    core.tick(1);
    let mut seen = None;
    core.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        let d = region.data();
        seen = Some((d.block_at(dug).map(|b| b.id), d.id_index.contains_key(&slime), d.player_entites.len()));
    });
    let (block, has_slime, bodies) = seen.expect("the spawn region runs");
    assert_eq!(block, Some(BlockId::AIR), "the dug block is still dug");
    assert!(has_slime, "the slime was restored");
    assert_eq!(core.manager.location_of(slime), Some(SPAWN_REGION), "and is in one place");
    assert_eq!(core.manager.pending_arrival_total(), 0, "not also waiting in a mailbox");
    assert_eq!(core.manager.in_flight_count(), 0, "nor in flight");
    assert_eq!(bodies, 1, "the player is back, once");
    drop(core);
    drop(world);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A world with file saves from before the world store and an empty store
/// is refused, naming the cleanup recipe — and nothing is imported.
#[test]
fn legacy_saves_refuse_start() {
    let dir = scratch("legacy");
    let world_path = dir.join("terrain.world");
    std::fs::write(&world_path, b"a forge file").unwrap();
    std::fs::create_dir_all(dir.join("terrain.regions/b64-c16")).unwrap();
    let err = open_world(&source_for(&world_path), &small_params()).err().expect("refused");
    assert!(err.contains(world_file::LEGACY_SAVES_REFUSAL), "{err}");
    assert!(err.contains("just clean-old-worlds") && err.contains("terrain.regions"), "{err}");
    assert_eq!(std::fs::read(&world_path).unwrap(), b"a forge file", "nothing imported");
    // The refused open released the store: once the old saves are gone, the
    // same store opens (here: the bad forge file is refused for itself).
    std::fs::remove_dir_all(dir.join("terrain.regions")).unwrap();
    let err = open_world(&source_for(&world_path), &small_params()).err().expect("not a world file");
    assert!(!err.contains(world_file::LEGACY_SAVES_REFUSAL) && err.contains("terrain.world"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The forge file goes into the store as tiles (its bytes kept as the
/// archive row) and is deleted; the store then serves the world, with the
/// tiled world's `content_crc` (Decision 8), not the file's crc32.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn import_deletes_forge_file() {
    let dir = scratch("import");
    let world_path = dir.join("terrain.world");
    let bytes = forge_file(&world_path);
    let opened = open_world(&source_for(&world_path), &small_params()).unwrap();
    assert!(!world_path.exists(), "the forge file is deleted once imported");
    assert!(dir.join("terrain.sqlite").exists(), "the store is the world file's .sqlite");
    let crc = opened.world.as_ref().unwrap().content_crc();
    assert!(opened.world.as_ref().unwrap().tile_cache().is_some(), "served from tiles");
    let store = opened.store.unwrap();
    let info = store.world_info().unwrap();
    assert!(info.has_artifact);
    assert_eq!(info.content_crc, Some(crc));
    assert_eq!(info.world_tick, 0);
    assert_eq!(store.forged_archive().unwrap().as_deref(), Some(&bytes[..]), "the file is the archive row");
    drop(opened.world);
    store.shutdown();
    // Opened again: from the store alone.
    let opened = open_world(&source_for(&world_path), &small_params()).unwrap();
    assert_eq!(opened.world.as_ref().unwrap().content_crc(), crc);
    drop(opened.world);
    opened.store.unwrap().shutdown();
    // A crash between the import's commit and the delete left the file: the
    // same bytes as the archive are deleted at the next start; another file
    // is left alone.
    std::fs::write(&world_path, &bytes).unwrap();
    open_world(&source_for(&world_path), &small_params()).unwrap().store.unwrap().shutdown();
    assert!(!world_path.exists(), "an already imported forge file is deleted");
    std::fs::write(&world_path, b"another file").unwrap();
    open_world(&source_for(&world_path), &small_params()).unwrap().store.unwrap().shutdown();
    assert_eq!(std::fs::read(&world_path).unwrap(), b"another file", "a different file is kept");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Runs raw SQL against a closed store.
fn raw_exec(url: &world_store::StoreUrl, sql: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (pool, _guard) = world_store::open_pool(url, world_store::Dials::BUILT, world_store::OpenMode::Writer).await.unwrap();
        match &pool {
            world_store::Pool::Sqlite(p) => {
                sqlx::query(sql).execute(p).await.unwrap();
                p.close().await;
            }
            world_store::Pool::Pg(p) => {
                sqlx::query(sql).execute(p).await.unwrap();
                p.close().await;
            }
        }
    });
}

/// The served world: installed as the process's forged world, sampled
/// through store tiles, with the world map read from the store (never
/// rebuilt from rasters at start).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn server_world_is_tiled() {
    let dir = scratch("tiled");
    let world_path = dir.join("terrain.world");
    forge_file(&world_path);
    let mut opened = open_world(&source_for(&world_path), &small_params()).expect("the world opens");
    let store = opened.store.clone().expect("a forged world has a store");
    let map = world_file::install_world(&mut opened).expect("a forged world has a world map");
    assert!(opened.world.is_none(), "installed: moved into the process");
    let world = worldgen::forged::active().expect("installed");
    assert!(world.tile_cache().is_some(), "the served world samples tiles");
    assert_eq!(Some(world.content_crc()), store.world_info().unwrap().content_crc);
    let (_, stored) = world_store::load_forged(&store).unwrap().expect("stored");
    assert_eq!(map, stored, "the world map is the stored one");
    // Region generation on the installed world pins its tiles first.
    let pins = worldgen::prefetch_region(SPAWN_REGION).expect("the tiles load");
    assert!(!pins.is_empty(), "a tiled world pins tiles");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A store imported under Plan A alone (the forge file as the archive row,
/// no tiles): tiled from its archive at start, not refused. A tile format
/// this build does not read is re-tiled from the archive at start too; with
/// no archive it is refused, naming re-forge.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn untiled_store_is_tiled_at_start() {
    let dir = scratch("untiled");
    let world_path = dir.join("terrain.world");
    let bytes = forge_file(&world_path);
    let source = source_for(&world_path);
    let opened = open_world(&source, &small_params()).unwrap();
    let crc = opened.world.as_ref().unwrap().content_crc();
    drop(opened.world);
    opened.store.unwrap().shutdown();
    let url = world_store::StoreUrl::Sqlite(dir.join("terrain.sqlite"));
    raw_exec(&url, "DELETE FROM forged_meta");
    raw_exec(&url, "DELETE FROM forged_tiles");
    raw_exec(&url, "DELETE FROM forged_world_map");
    let opened = open_world(&source, &small_params()).expect("an archive-only store opens");
    assert!(opened.world.as_ref().unwrap().tile_cache().is_some(), "tiled from its archive");
    assert_eq!(opened.world.as_ref().unwrap().content_crc(), crc, "the same tiles as the import made");
    assert!(opened.map.is_some());
    let store = opened.store.unwrap();
    assert_eq!(store.forged_archive().unwrap().as_deref(), Some(&bytes[..]), "the archive is kept");
    drop(opened.world);
    store.shutdown();
    raw_exec(&url, "UPDATE forged_meta SET format_version = 0");
    let opened = open_world(&source, &small_params()).expect("another tile format is re-tiled at start");
    assert_eq!(opened.world.as_ref().unwrap().content_crc(), crc, "re-tiled to the same world");
    drop(opened.world);
    opened.store.unwrap().shutdown();
    raw_exec(&url, "UPDATE forged_meta SET format_version = 0");
    raw_exec(&url, "DELETE FROM forged_archive");
    let err = open_world(&source, &small_params()).err().expect("no archive to re-tile from: refused");
    assert!(err.contains("re-forge"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stored world map this build does not serve (here the unversioned
/// format-1 row) is rebuilt at start from the archive — the same world, its
/// map as this build makes it — and the store's saves (the world tick and
/// history of the last cut) are untouched.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stale_world_map_is_rebuilt_at_start() {
    let dir = scratch("stale-map");
    let world_path = dir.join("terrain.world");
    forge_file(&world_path);
    let source = source_for(&world_path);
    let opened = open_world(&source, &small_params()).unwrap();
    let crc = opened.world.as_ref().unwrap().content_crc();
    let map = opened.map.clone().unwrap();
    drop(opened.world);
    let store = opened.store.unwrap();
    let history = b"history bytes of the last cut".to_vec();
    let cut = game::WorldCut {
        host: 0,
        id: game::CutId(1),
        tick: 77,
        regions: Vec::new(),
        in_flight: Vec::new(),
        players: Vec::new(),
        history: Some(history.clone()),
    };
    let fence = ClaimOnTouch::new(store.clone()).fence_for_cut(&cut).unwrap();
    store.commit_cut(cut, fence).recv().unwrap().expect("the cut commits");
    store.shutdown();
    let url = world_store::StoreUrl::Sqlite(dir.join("terrain.sqlite"));
    let old_row: String = world_store::encode(1, &map).iter().map(|b| format!("{b:02x}")).collect();
    raw_exec(&url, &format!("UPDATE forged_world_map SET bytes = X'{old_row}' WHERE id = 1"));
    let opened = open_world(&source, &small_params()).expect("a stale world map is rebuilt at start");
    assert_eq!(opened.world.as_ref().unwrap().content_crc(), crc, "the same world");
    assert_eq!(opened.map.as_ref(), Some(&map), "the map as this build makes it");
    assert_eq!(opened.world_tick, 77, "the save cut is kept");
    let store = opened.store.unwrap();
    assert_eq!(store.load_history().unwrap(), Some(history), "the history is kept");
    drop(opened.world);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A forge file the tiled store cannot hold (4 m cells: sampling reads past
/// `MAX_READ_RADIUS_CELLS`) is refused at import with the reason, before
/// anything is written — and the file is KEPT (no world is lost).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn unsupported_world_file_is_kept() {
    let dir = scratch("unsupported");
    let world_path = dir.join("terrain.world");
    let params = ForgeParams { size_m: 512.0, cell_m: 4.0, seed: 4, ..ForgeParams::default() };
    worldgen::forge::forge(&params).artifact(&params).save(&world_path).unwrap();
    let bytes = std::fs::read(&world_path).unwrap();
    let err = open_world(&source_for(&world_path), &small_params()).err().expect("refused");
    assert!(err.contains("cell size 4 m is not supported by the tiled store"), "{err}");
    assert!(err.contains("the file is kept"), "{err}");
    assert_eq!(std::fs::read(&world_path).unwrap(), bytes, "the forge file is left in place");
    let url = world_store::StoreUrl::Sqlite(dir.join("terrain.sqlite"));
    let store = world_store::StoreHandle::open(&url, world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap();
    assert!(!store.world_info().unwrap().has_artifact, "nothing imported");
    assert!(world_store::load_forged(&store).unwrap().is_none(), "no tiles either");
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The default 16 km / 32 m forge (2x2 tiles, so regions on other tiles
/// exist), forged once per process.
fn default_world_bytes() -> Vec<u8> {
    static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BYTES
        .get_or_init(|| {
            let params = ForgeParams::default();
            worldgen::forge::forge(&params).artifact(&params).to_bytes()
        })
        .clone()
}

type Tile = worldgen::forged_tiles::TileCoords;

/// The tiles generating `rc` pins.
fn own_tiles(world: &ForgedWorld, rc: RegionCoords) -> Vec<Tile> {
    worldgen::prefetch_region_forged(world, rc).unwrap().coords().collect()
}

/// The tiles a region thread at `rc` pins: its 3x3 neighbourhood's.
fn neighbourhood_tiles(world: &ForgedWorld, rc: RegionCoords) -> Vec<Tile> {
    let mut tiles: Vec<Tile> = rc.window_3x3().into_iter().flat_map(|n| own_tiles(world, n)).collect();
    tiles.sort_by_key(|t| (t.tz, t.tx));
    tiles.dedup();
    tiles
}

/// Regions out from the spawn region along both axes, nearest first.
fn candidate_regions() -> impl Iterator<Item = RegionCoords> {
    (0..1000).flat_map(|k: i32| {
        [RegionCoords::new(k, 0), RegionCoords::new(-k, 0), RegionCoords::new(0, k), RegionCoords::new(0, -k)]
    })
}

/// A tiled store on a temp sqlite whose tile `pick` chooses (from the healthy
/// world) has one byte flipped; then a region thread spawner over it like the
/// server's (store + `prefetch_region` on that world) is started for
/// `failing` and `healthy`: `failing` answers `LoadFailed` naming the tile and
/// never generates, `healthy` generates and serves its terrain.
fn damaged_tile_run(tag: &str, pick: impl Fn(&ForgedWorld) -> (RegionCoords, Tile)) {
    use game::{RegionInput, RegionSeed};
    use server::region_threads::ThreadRegionSpawner;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = scratch(tag);
    let url = world_store::StoreUrl::Sqlite(dir.join("w.sqlite"));
    let store = world_store::StoreHandle::open(&url, world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap();
    world_store::write_tiled(&store, default_world_bytes()).unwrap();
    let (world, _) = world_store::load_forged(&store).unwrap().unwrap();
    let (failing, bad) = pick(&world);
    let healthy = candidate_regions()
        .find(|rc| !neighbourhood_tiles(&world, *rc).contains(&bad))
        .expect("a region whose neighbourhood avoids the damaged tile");
    let good = worldgen::forged_tiles::TileLoader::load(&world_store::StoreTiles(store.clone()), bad).unwrap();
    drop(world);
    store.shutdown();
    let mut damaged = good.clone();
    let mid = damaged.len() / 2;
    damaged[mid] ^= 0x10;
    let hex: String = damaged.iter().map(|b| format!("{b:02x}")).collect();
    raw_exec(&url, &format!("UPDATE forged_tiles SET bytes = X'{hex}' WHERE tx = {} AND tz = {}", bad.tx, bad.tz));

    let store = world_store::StoreHandle::open(&url, world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap();
    let (world, _) = world_store::load_forged(&store).unwrap().unwrap();
    let world = Arc::new(world);
    let materials = materials();
    let generated = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (gen_world, log) = (world.clone(), generated.clone());
    let generate: server::region_threads::SharedRegionGenerator = Arc::new(move |rc| {
        log.lock().unwrap().push(rc);
        worldgen::generate_region_forged(&gen_world, rc, materials)
    });
    let pre_world = world.clone();
    let prefetches = Arc::new(AtomicUsize::new(0));
    let counted = prefetches.clone();
    let mut spawner = ThreadRegionSpawner::with_region_generator(generate)
        .with_prefetch(Arc::new(move |rc| {
            counted.fetch_add(1, Ordering::SeqCst);
            worldgen::prefetch_region_forged(&pre_world, rc)
        }))
        .with_store(ClaimOnTouch::new(store.clone()));
    let (out, outputs) = unbounded();
    let to_bad = spawner.spawn(failing, RegionSeed::Deferred(None), out.clone());
    let to_good = spawner.spawn(healthy, RegionSeed::Deferred(None), out);
    let _ = to_good.send(RegionInput::RequestTerrain(0));
    let (mut failed, mut terrain) = (None, None);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while (failed.is_none() || terrain.is_none()) && std::time::Instant::now() < deadline {
        let Ok((rc, output)) = outputs.recv_timeout(Duration::from_millis(100)) else { continue };
        match output {
            RegionOutput::LoadFailed(why) => {
                assert_eq!(rc, failing, "only the region over the damaged tile fails: {why}");
                failed = Some(why);
            }
            RegionOutput::Terrain(_, snap) => {
                assert_eq!(rc, healthy);
                terrain = Some(snap);
            }
            _ => {}
        }
    }
    let why = failed.expect("the region over the damaged tile answers LoadFailed");
    assert!(why.contains(&format!("({}, {})", bad.tx, bad.tz)), "names the tile: {why}");
    let terrain = terrain.expect("the healthy region runs");
    assert!(!terrain.chunks.is_empty(), "with generated terrain");
    assert_eq!(*generated.lock().unwrap(), vec![healthy], "the failing region never generated");
    assert!(prefetches.load(Ordering::SeqCst) >= 9, "the healthy region pinned its 3x3 neighbourhood");
    let _ = to_good.send(RegionInput::Shutdown);
    drop(to_bad);
    drop(to_good);
    spawner.reap(failing);
    spawner.reap(healthy);
    drop(world);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A store whose tile under region (0,0) is damaged: that region's thread
/// answers `LoadFailed` and never generates; a region on healthy tiles runs.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn tile_load_failure_stops_region() {
    damaged_tile_run("tile-failure", |world| (SPAWN_REGION, own_tiles(world, SPAWN_REGION)[0]));
}

/// A damaged tile that only a NEIGHBOUR of the region reads (the region's
/// halo seed of that never-run neighbour would): the region answers
/// `LoadFailed` before it runs — never with an empty mirror past that seam.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn neighbour_tile_failure_stops_region() {
    damaged_tile_run("neighbour-tile-failure", |world| {
        candidate_regions()
            .find_map(|rc| {
                let own = own_tiles(world, rc);
                neighbourhood_tiles(world, rc).into_iter().find(|t| !own.contains(t)).map(|t| (rc, t))
            })
            .expect("a region next to a tile seam that its own generation does not cross")
    });
}

/// `VOXEL_WORLD_FILE=procedural`: no store, nothing written anywhere — even
/// with a `VOXEL_WORLD_DB` named.
#[test]
fn procedural_persists_nothing() {
    let dir = scratch("procedural");
    let db = dir.join("w.sqlite");
    let source = world_file::resolve(Some("procedural"), Some(db.to_str().unwrap())).unwrap();
    assert_eq!(source, WorldSource::Procedural);
    let opened = open_world(&source, &small_params()).unwrap();
    assert!(opened.store.is_none() && opened.world.is_none());
    assert_eq!(opened.world_tick, 0);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "no file was written");
    let _ = std::fs::remove_dir_all(&dir);
}

/// One block of floor under every chunk column: ground without worldgen.
fn flat_floor(_rc: RegionCoords) -> Vec<(game::ColumnCoords, game::Column)> {
    let air = game::Block::new(BlockId::AIR);
    let mut out = Vec::new();
    for x in 0..game::REGION_CHUNKS {
        for z in 0..game::REGION_CHUNKS {
            let sub = game::SubChunk::Dense(Box::new(game::Chunk::flat_floor(game::BLOCK_VOXELS as u32)));
            out.push((game::ColumnCoords::new(x, z), game::Column::new(0, air, air, vec![sub])));
        }
    }
    out
}

/// A fresh store on a temp file (no world artifact: these tests only commit
/// cuts) and a player resolved in it.
fn bare_store(dir: &Path) -> (world_store::StoreHandle, server::player_store::PlayerKey, game::PlayerId) {
    materials();
    let url = world_store::StoreUrl::Sqlite(dir.join("w.sqlite"));
    let store = world_store::StoreHandle::open(&url, world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap();
    let token = server::player_store::PlayerKey::Online(game::PlayerToken::generate([7; game::PLAYER_TOKEN_BYTES]));
    let (player, _) = StorePlayers(store.clone()).resolve(&token).unwrap();
    (store, token, player)
}

/// The exit's park (`server::cuts::park_for_exit`) waits past the old fixed
/// 2 s for a region that parks late (its inputs unread until then): its
/// state and the player's record still go into the exit's cut.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn exit_saves_a_region_that_parks_late() {
    let dir = scratch("late-park");
    let (store, token, player) = bare_store(&dir);
    let mut core = Core::open_with(&store, Box::new(flat_floor));
    core.manager.handle_server_event(ServerEvent::ClientConnected { client: 0, player, record: None }, 0);
    core.tick(3);
    assert_eq!(core.manager.running_regions(), vec![SPAWN_REGION]);
    let started = std::time::Instant::now();
    let late = Duration::from_millis(2_200);
    let Core { mut manager, claims, region_out, _packets } = core;
    let stuck = server::cuts::park_for_exit(&mut manager, &region_out, || 0, server::cuts::EXIT_PARK_TIMEOUT, |m| {
        if started.elapsed() >= late {
            m.spawner_mut().pump();
        }
    });
    assert!(started.elapsed() >= late, "it parked only once pumped");
    assert!(stuck.is_empty(), "nothing given up: {stuck:?}");
    assert!(manager.running_regions().is_empty());
    let now = manager.spawner_mut().world_tick();
    let writer = server::cuts::CutWriter::new(claims);
    assert!(writer.finish(&mut manager, now, Duration::from_secs(10)), "the exit's cut commits");
    let store = world_store::StoreHandle::open(
        &world_store::StoreUrl::Sqlite(dir.join("w.sqlite")),
        world_store::Dials::BUILT,
        world_store::OpenMode::Writer,
    )
    .unwrap();
    assert!(ClaimOnTouch::new(store.clone()).load_region(SPAWN_REGION).unwrap().is_some(), "the late region was saved");
    let (again, record) = StorePlayers(store.clone()).resolve(&token).unwrap();
    assert_eq!(again, player);
    assert_eq!(record.expect("the player's record was saved").region, SPAWN_REGION);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A region that never parks at exit is given up after the park timeout and
/// does not stop the rest from being saved.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn exit_saves_the_rest_when_a_region_never_parks() {
    use game::ClientPacket;
    let dir = scratch("never-parks");
    let (store, token, player) = bare_store(&dir);
    let mut core = Core::open_with(&store, Box::new(flat_floor));
    core.manager.handle_server_event(ServerEvent::ClientConnected { client: 0, player, record: None }, 0);
    let east = RegionCoords::new(SPAWN_REGION.x + 1, SPAWN_REGION.z);
    core.manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(east), 0), 0);
    core.tick(3);
    let mut running = core.manager.running_regions();
    running.sort();
    assert!(running.contains(&SPAWN_REGION) && running.contains(&east), "{running:?}");
    // East never reads another input: it never parks.
    core.manager.spawner_mut().hold_inputs(east, u32::MAX);
    let Core { mut manager, claims, region_out, _packets } = core;
    let stuck = server::cuts::park_for_exit(&mut manager, &region_out, || 0, Duration::from_millis(300), |m| {
        m.spawner_mut().pump();
    });
    assert_eq!(stuck, vec![east]);
    let now = manager.spawner_mut().world_tick();
    let writer = server::cuts::CutWriter::new(claims);
    assert!(writer.finish(&mut manager, now, Duration::from_secs(10)), "the rest is saved");
    let store = world_store::StoreHandle::open(
        &world_store::StoreUrl::Sqlite(dir.join("w.sqlite")),
        world_store::Dials::BUILT,
        world_store::OpenMode::Writer,
    )
    .unwrap();
    assert!(ClaimOnTouch::new(store.clone()).load_region(SPAWN_REGION).unwrap().is_some(), "the region that parked was saved");
    let (_, record) = StorePlayers(store.clone()).resolve(&token).unwrap();
    assert_eq!(record.expect("the player's record was saved").region, SPAWN_REGION);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
