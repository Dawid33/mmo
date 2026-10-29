//! Server-authored session moves for Play: `CreateClientAt` lands a player
//! over a chosen block column, `ClientLeft` takes the player out again —
//! both ordinary undoable sim events.
use game::{Chunk, ChunkCoords, GameEvent, GameEventKind, Region, RegionId, Rollback, BLOCK_VOXELS};

/// The block column `CreateClientAt` targets in these tests.
const COLUMN: [u32; 2] = [5, 7];

// A floor whose top face is at Y = 128 under the spawn column (region-local
// voxel (128,128), as in spawn_surface.rs) and under `COLUMN`.
fn region_with_floor() -> Region {
    let cu = game::CHUNK_UNITS;
    let (cy, depth) = ((128 - 1) / cu, 128 - (128 - 1) / cu * cu);
    let floor = |x: usize, z: usize| (ChunkCoords::new(x / cu, cy as i32, z / cu), Chunk::flat_floor(depth as u32));
    let mut chunks = vec![floor(128, 128)];
    let col = floor(COLUMN[0] as usize * BLOCK_VOXELS, COLUMN[1] as usize * BLOCK_VOXELS);
    if col.0 != chunks[0].0 {
        chunks.push(col);
    }
    Region::from_chunks(RegionId::new(0, 0), chunks)
}

fn player_translation(region: &Region, client: game::ClientId) -> (f32, f32, f32) {
    let data = region.data();
    let e = *data.player_entites.get(&client).unwrap();
    let handle = *data.ecs.rigidbody.get(e);
    let t = data.physics.bodies.get(handle).unwrap().translation();
    (t.x, t.y, t.z)
}

#[test]
fn create_client_at_lands_over_the_column() {
    let mut region = region_with_floor();
    region.handle_event(GameEventKind::CreateClientAt(0, COLUMN)).unwrap();
    let (x, y, z) = player_translation(&region, 0);
    let bv = BLOCK_VOXELS as f32;
    let (cx, cz) = ((COLUMN[0] as f32 + 0.5) * bv, (COLUMN[1] as f32 + 0.5) * bv);
    assert!((x - cx).abs() <= 0.5 * bv, "x={x} not over column centre {cx}");
    assert!((z - cz).abs() <= 0.5 * bv, "z={z} not over column centre {cz}");
    let surface = region.data().surface_voxel_y(cx as i32, cz as i32).expect("floor under the column");
    assert!(y > surface as f32, "body y={y} must be above surface {surface}");
    let client = region.data().clients.get(&0).expect("client 0 created");
    assert_eq!(client.inventory, game::Inventory::seeded());
}

#[test]
fn client_left_removes_body_and_client() {
    let mut region = region_with_floor();
    let before = region.data().ecs.entities.len();
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    assert!(region.data().ecs.entities.len() > before);
    region.handle_event(GameEventKind::ClientLeft(0)).unwrap();
    let data = region.data();
    assert!(data.player_entites.get(&0).is_none(), "player entity mapping left behind");
    assert!(data.clients.get(&0).is_none(), "client entry left behind");
    assert_eq!(data.ecs.entities.len(), before, "entity count back to the pre-create count");
}

#[test]
fn client_left_undoes_bit_exact() {
    game::set_hash_mode(game::HashMode::Full);
    // Undo replays render events through a live sender (`from_chunks`
    // wires `None`, which the undo closures unwrap).
    let (send, _recv) = crossbeam::channel::unbounded();
    let built = region_with_floor();
    let mut region = Region::new(built.data().clone(), Some(send), RegionId::new(0, 0), None);
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();
    let before = game::state_hash(region.data());
    region.handle_event(GameEventKind::ClientLeft(0)).unwrap();
    assert_ne!(before, game::state_hash(region.data()));
    region.rollback_last_event();
    assert_eq!(before, game::state_hash(region.data()), "ClientLeft must invert bit-exact");
}

#[test]
fn client_left_of_a_missing_player_is_a_no_op() {
    let mut region = region_with_floor();
    let entities = region.data().ecs.entities.len();
    region.handle_event(GameEventKind::ClientLeft(3)).unwrap();
    assert!(region.data().player_entites.is_empty());
    assert_eq!(region.data().ecs.entities.len(), entities);
}

#[test]
fn client_left_reconciles_on_a_client_replica() {
    let id = RegionId::new(0, 0);
    let mut server = region_with_floor();
    server.handle_event(GameEventKind::CreateClient(0)).unwrap();
    server.commit_server_event();
    let snapshot: Rollback = server.data().clone();
    let left = server.handle_event(GameEventKind::ClientLeft(0)).unwrap();

    // Client replica built from the snapshot; it has a pending local
    // prediction at the same event id, so reconcile has to replace it.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut client = Region::new(snapshot, Some(send), id, Some(0));
    client.handle_event(GameEventKind::Tick).unwrap();
    client
        .reconcile(GameEvent::new(left.kind.clone(), left.id, id))
        .expect("authoritative ClientLeft reconciles without a GameError");
    assert!(client.data().player_entites.get(&0).is_none(), "body for 0 must be gone");
    assert!(client.data().clients.get(&0).is_none(), "client 0 must be gone");
}
