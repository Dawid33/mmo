//! The server on the world store: a world restarts from its store as it was
//! saved at exit; the forge file is imported into the store and deleted; old
//! file saves refuse the start; the procedural world keeps nothing.
//!
//! Its own test binary: `restart_restores_world` installs the process-global
//! forged world (`worldgen::forged::install`, once per process).
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver};
use game::{
    BlockId, ClientId, InlineLodBuilder, InlineSpawner, LodBuilder, RegionCoords, RegionOutput, RegionSpawner,
    ServerEvent, ServerPacket, WorldManager, SPAWN_REGION,
};
use server::player_store::{PlayerStore, StorePlayers};
use server::world_file::{self, open_world, WorldSource};
use worldgen::forge::ForgeParams;

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
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    _packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

impl Core {
    fn open(store: &world_store::StoreHandle) -> Core {
        let mut regions = std::collections::BTreeMap::new();
        for rc in store.stored_regions().unwrap().into_keys() {
            regions.insert(rc, store.load_region(rc).unwrap().expect("listed, so stored"));
        }
        let materials = materials();
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
            Box::new(move |rc| worldgen::generate_region(rc, worldgen::WORLD_SEED, materials)),
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        manager.set_stored_regions(store.stored_regions().unwrap());
        manager.restore_in_flight(store.load_in_flight().unwrap());
        Core { manager, region_out, _packets: packets }
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

    /// The orderly exit: players and regions parked, the last cut committed.
    fn exit_into(mut self, store: world_store::StoreHandle) -> game::Tick {
        self.manager.park_all_players(0);
        self.manager.shutdown_all();
        self.settle();
        assert!(self.manager.running_regions().is_empty(), "every region parked");
        let now = self.manager.spawner_mut().world_tick();
        let writer = server::cuts::CutWriter::new(store);
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
    worldgen::forged::install(opened.world);
    let store = opened.store.expect("a forged world has a store");
    let token = game::PlayerToken::generate([9; game::PLAYER_TOKEN_BYTES]);
    let (player, none) = StorePlayers(store.clone()).resolve(&token).unwrap();
    assert!(none.is_none());

    // Life 1: a player, a dug block and a slime in the spawn region.
    let mut core = Core::open(&store);
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
    let exit_tick = core.exit_into(store);

    // Life 2: the forge file was imported and is gone; the store has it all.
    assert!(!world_path.exists(), "the imported forge file was deleted");
    let opened = open_world(&source, &small_params()).expect("the world opens again");
    assert_eq!(opened.world.as_ref().unwrap().content_crc(), crc, "the same world, from the store");
    assert_eq!(worldgen::forged::active().unwrap().content_crc(), crc);
    assert!(opened.world_tick >= exit_tick, "the world tick never goes back: {} < {exit_tick}", opened.world_tick);
    let store = opened.store.unwrap();
    let (again, record) = StorePlayers(store.clone()).resolve(&token).unwrap();
    assert_eq!(again, player);
    let record = record.expect("the player's record was saved by the exit's cut");
    assert_eq!(record.region, SPAWN_REGION);
    let mut core = Core::open(&store);
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

/// The forge file goes into the store with its crc (the same `content_crc`
/// as loading the file) and is deleted; the store then serves the world.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn import_deletes_forge_file() {
    let dir = scratch("import");
    let world_path = dir.join("terrain.world");
    let bytes = forge_file(&world_path);
    let from_file = worldgen::forged::ForgedWorld::load(&world_path).unwrap().content_crc();
    assert_eq!(from_file, crc32fast::hash(&bytes));
    let opened = open_world(&source_for(&world_path), &small_params()).unwrap();
    assert!(!world_path.exists(), "the forge file is deleted once imported");
    assert!(dir.join("terrain.sqlite").exists(), "the store is the world file's .sqlite");
    assert_eq!(opened.world.as_ref().unwrap().content_crc(), from_file);
    let store = opened.store.unwrap();
    let info = store.world_info().unwrap();
    assert!(info.has_artifact);
    assert_eq!(info.content_crc, Some(from_file));
    assert_eq!(info.world_tick, 0);
    store.shutdown();
    // Opened again: from the store alone.
    let opened = open_world(&source_for(&world_path), &small_params()).unwrap();
    assert_eq!(opened.world.as_ref().unwrap().content_crc(), from_file);
    opened.store.unwrap().shutdown();
    let _ = std::fs::remove_dir_all(&dir);
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
