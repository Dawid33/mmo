//! Stud chunks as sim entities (`stud_ops.rs`): raw genesis, undo-safe
//! runtime create/set with collider sync, the rollback bar, and spawning
//! over stud terrain.
use game::parry::shape::ShapeType;
use game::stud_catalog::STUD_PARTS;
use game::*;
use state_hash::StateHash;

fn part(n: &str) -> u16 {
    STUD_PARTS.iter().position(|p| p.number == n).unwrap() as u16
}
fn brick(n: &str, min: [u8; 3]) -> StudBrick {
    StudBrick {
        min,
        shape: StudShape::Part {
            part: part(n),
            rot: 0,
        },
        material: 4,
        data: 0,
    }
}
fn filler(size: [u8; 3], min: [u8; 3]) -> StudBrick {
    StudBrick {
        min,
        shape: StudShape::Filler { size },
        material: 4,
        data: 0,
    }
}
fn chunk(b: impl IntoIterator<Item = StudBrick>) -> StudChunk {
    StudChunk::new(b).unwrap()
}
fn state_hash(r: &Rollback) -> u32 {
    let mut h = crc32fast::Hasher::new();
    r.state_hash(&mut h);
    h.finalize()
}
fn shape_hash<T: StateHash + ?Sized>(v: &T) -> u32 {
    let mut h = crc32fast::Hasher::new();
    v.state_hash(&mut h);
    h.finalize()
}

const C: StudChunkCoords = StudChunkCoords { x: 1, y: 0, z: 2 };

/// The colliders of `key`'s body, in body order: (shape type, enabled).
fn colliders(r: &Rollback, key: EntityKey) -> Vec<(ShapeType, bool)> {
    let h = r.ecs.rigidbody.try_get(key).expect("stud body");
    r.physics
        .bodies
        .get(h)
        .unwrap()
        .colliders()
        .iter()
        .map(|&ch| {
            let c = r.physics.colliders.get(ch).unwrap();
            (c.shape().shape_type(), c.is_enabled())
        })
        .collect()
}

fn genesis(content: StudChunk) -> (Rollback, crossbeam::channel::Receiver<GameDataUpdate>) {
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    r.seed_stud_genesis(vec![(C, content)]);
    (r, recv)
}

#[test]
fn genesis_builds_body_and_colliders() {
    let (r, _rx) = genesis(chunk([brick("3001", [0, 0, 0])]));
    let key = r.stud_key_at(C).expect("indexed");
    let h = r.ecs.rigidbody.try_get(key).unwrap();
    let body = r.physics.bodies.get(h).unwrap();
    assert!(body.is_fixed());
    assert_eq!(body.translation(), C.origin());
    assert_eq!(colliders(&r, key), vec![(ShapeType::Voxels, true)]);
    assert_eq!(r.stud_chunk_at(C).unwrap().len(), 1);
}

#[test]
fn genesis_empty_chunk_has_entity_and_no_colliders() {
    let (r, _rx) = genesis(StudChunk::default());
    let key = r.stud_key_at(C).expect("indexed");
    assert!(r.ecs.rigidbody.try_get(key).is_some());
    assert!(colliders(&r, key).is_empty());
    assert!(r.stud_chunk_at(C).unwrap().is_empty());
}

#[test]
fn create_rolls_back() {
    let (mut r, rx) = genesis(StudChunk::default());
    let before = state_hash(&r);
    r.new_transaction();
    let c = StudChunkCoords { x: 0, y: 1, z: 0 };
    let e = r.create_stud_chunk_safe(c, chunk([brick("3001", [4, 0, 4])]));
    assert_eq!(r.stud_key_at(c), Some(e));
    assert_eq!(colliders(&r, e), vec![(ShapeType::Voxels, true)]);
    let done: Vec<_> = rx.try_iter().map(|u| u.update_kind).collect();
    assert!(done
        .iter()
        .any(|k| matches!(k, GameDataUpdateKind::AddStudChunk(k2, cc, _) if *k2 == e && *cc == c)));
    r.rollback();
    assert_eq!(state_hash(&r), before);
    assert_eq!(r.stud_key_at(c), None);
    let undone: Vec<_> = rx.try_iter().map(|u| u.update_kind).collect();
    assert!(undone
        .iter()
        .any(|k| matches!(k, GameDataUpdateKind::RemoveEntity(k2) if *k2 == e)));
}

#[test]
fn set_place_rolls_back() {
    let base = chunk([brick("3001", [0, 0, 0])]);
    let (mut r, rx) = genesis(base.clone());
    let key = r.stud_key_at(C).unwrap();
    let before = state_hash(&r);
    r.new_transaction();
    let mut next = base.clone();
    next.place(brick("3001", [0, 3, 0])).unwrap();
    r.set_stud_chunk_safe(key, next.clone());
    assert_eq!(r.stud_chunk_at(C), Some(&next));
    let done: Vec<_> = rx.try_iter().map(|u| u.update_kind).collect();
    assert!(done
        .iter()
        .any(|k| matches!(k, GameDataUpdateKind::SetStudChunk(_, c) if **c == next)));
    r.rollback();
    assert_eq!(state_hash(&r), before);
    let undone: Vec<_> = rx.try_iter().map(|u| u.update_kind).collect();
    assert!(undone
        .iter()
        .any(|k| matches!(k, GameDataUpdateKind::SetStudChunk(_, c) if **c == base)));
}

#[test]
fn set_remove_rolls_back() {
    let base = chunk([
        filler([32, 15, 32], [0, 0, 0]),
        brick("3001", [0, 15, 0]),
        brick("3040", [10, 15, 10]),
    ]);
    let (mut r, _rx) = genesis(base.clone());
    let key = r.stud_key_at(C).unwrap();
    let before = state_hash(&r);
    r.new_transaction();
    let mut next = base.clone();
    next.remove_at([10, 15, 10]).unwrap();
    r.set_stud_chunk_safe(key, next);
    assert_ne!(state_hash(&r), before);
    r.rollback();
    assert_eq!(state_hash(&r), before);
}

#[test]
fn remove_to_empty_then_place_rolls_back() {
    let base = chunk([brick("3001", [0, 0, 0])]);
    let (mut r, _rx) = genesis(base.clone());
    let key = r.stud_key_at(C).unwrap();
    let b0 = state_hash(&r);
    r.new_transaction();
    r.set_stud_chunk_safe(key, StudChunk::default());
    assert_eq!(
        r.stud_key_at(C),
        Some(key),
        "an empty chunk keeps its entity"
    );
    assert_eq!(colliders(&r, key), vec![(ShapeType::Voxels, false)]);
    let b1 = state_hash(&r);
    r.new_transaction();
    r.set_stud_chunk_safe(key, base);
    assert_eq!(colliders(&r, key), vec![(ShapeType::Voxels, true)]);
    r.rollback();
    assert_eq!(state_hash(&r), b1);
    r.rollback();
    assert_eq!(state_hash(&r), b0);
}

#[test]
fn runtime_and_genesis_colliders_agree() {
    let content = chunk([
        filler([32, 15, 32], [0, 0, 0]),
        brick("3001", [0, 15, 0]),
        brick("3040", [10, 15, 10]),
    ]);
    let shapes = |r: &Rollback, key: EntityKey| -> Vec<u32> {
        let h = r.ecs.rigidbody.try_get(key).unwrap();
        r.physics
            .bodies
            .get(h)
            .unwrap()
            .colliders()
            .iter()
            .map(|&ch| shape_hash(r.physics.colliders.get(ch).unwrap().shared_shape()))
            .collect()
    };
    let (g, _rx) = genesis(content.clone());
    let mut rt = Rollback::new(None);
    rt.new_transaction();
    let e = rt.create_stud_chunk_safe(C, content);
    let (a, b) = (shapes(&g, g.stud_key_at(C).unwrap()), shapes(&rt, e));
    assert_eq!(a.len(), 3, "coarse, fine, one hull");
    assert_eq!(a, b);
}

/// The join snapshot's wire form (`region_wire.rs`) leaves derivable stud collider
/// shapes off the wire; what the receiver rebuilds is the sender's state byte for
/// byte — after genesis, and after an incremental (patched in place) edit, whose
/// shape has another layout than a fresh build and so must travel whole.
#[test]
fn region_snapshot_wire_restores_the_state_exactly() {
    let content = chunk([
        filler([32, 15, 32], [0, 0, 0]),
        brick("3001", [0, 15, 0]),
        brick("3001", [8, 15, 8]),
        brick("3040", [10, 15, 10]),
    ]);
    let (mut g, _rx) = genesis(content);
    let round_trip = |g: &Rollback| -> usize {
        let wire = encode_packet(&ServerPacket::Region(RegionCoords::new(0, 0), g.clone())).unwrap();
        let ServerPacket::Region(_, back) = decode_packet(&wire).unwrap() else { panic!("a region packet") };
        assert_eq!(state_hash(&back), state_hash(g));
        assert!(bincode::serialize(&back).unwrap() == bincode::serialize(g).unwrap(), "the restored state differs");
        bincode::serialize(&ServerPacket::Region(RegionCoords::new(0, 0), g.clone())).unwrap().len()
    };
    let plain = bincode::serialize(&g).unwrap().len();
    let slim = round_trip(&g);
    assert!(slim * 4 < plain * 3, "genesis voxel shapes stay off the wire: {slim} of {plain} bytes");
    g.new_transaction();
    assert!(g.remove_stud_safe(C, [8, 15, 8]));
    round_trip(&g);
}

#[test]
fn player_spawns_above_stud_surface() {
    let mut region = Region::new(Rollback::new(None), None, RegionCoords::new(0, 0), None);
    let (sx, sz) = region.data().spawn_xz();
    let c = StudChunkCoords {
        x: sx as usize / CHUNK_UNITS,
        y: 0,
        z: sz as usize / CHUNK_UNITS,
    };
    region.seed_studs(vec![(c, chunk([filler([32, 50, 32], [0, 0, 0])]))]);
    assert_eq!(region.data().body_scale(), movement::BodyScale::STUD);
    let y = region.data().player_spawn_y((sx, sz));
    // On the bricks' top: the stud capsule's half extent, then the spawn
    // clearance (3.6 units) scaled with the body.
    let s = movement::BodyScale::STUD;
    let want = plate_units(50) + s.capsule_half_height() + s.capsule_radius() + 3.6 * s.factor;
    assert!((y - want).abs() < 1e-3, "spawn y {y} vs {want}");
}

/// Shape hashes of `key`'s ENABLED colliders, in body order.
fn enabled_shape_hashes(r: &Rollback, key: EntityKey) -> Vec<u32> {
    let h = r.ecs.rigidbody.try_get(key).unwrap();
    r.physics
        .bodies
        .get(h)
        .unwrap()
        .colliders()
        .iter()
        .map(|&ch| r.physics.colliders.get(ch).unwrap())
        .filter(|c| c.is_enabled())
        .map(|c| shape_hash(c.shared_shape()))
        .collect()
}
fn fresh_shape_hashes(chunk: &StudChunk) -> Vec<u32> {
    stud_collider_shapes(chunk).iter().map(|s| shape_hash(s)).collect()
}

/// splitmix64: a seeded test stream.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn patched_shapes_hash_like_a_fresh_build() {
    // 8 x 8 fillers, one per coarse column, so removing one breaks a single
    // coarse cell and the coarse grid gets patched too.
    let mut base = Vec::new();
    for x in 0..8u8 {
        for z in 0..8u8 {
            base.push(filler([4, 15, 4], [x * 4, 0, z * 4]));
        }
    }
    let (mut r, _rx) = genesis(chunk(base));
    let key = r.stud_key_at(C).unwrap();
    let parts = ["3001", "3003", "3024", "3008", "3040"];
    let mut rng = Rng(0x5eed);
    let rebuilds0 = Rollback::stud_collider_rebuilds();
    let mut edits = 0;
    while edits < 200 {
        let mut next = r.stud_chunk_at(C).unwrap().clone();
        let changed = if rng.below(3) == 0 && !next.is_empty() {
            let n = rng.below(next.len() as u64) as usize;
            let min = next.bricks().nth(n).unwrap().min;
            next.remove_at(min).is_some()
        } else {
            let b = StudBrick {
                min: [
                    rng.below(29) as u8,
                    rng.below(30) as u8,
                    rng.below(29) as u8,
                ],
                shape: StudShape::Part {
                    part: part(parts[rng.below(parts.len() as u64) as usize]),
                    rot: rng.below(4) as u8,
                },
                material: 4,
                data: 0,
            };
            next.place(b).is_ok()
        };
        if !changed {
            continue;
        }
        edits += 1;
        r.new_transaction();
        r.set_stud_chunk_safe(key, next.clone());
        assert_eq!(
            enabled_shape_hashes(&r, key),
            fresh_shape_hashes(&next),
            "edit {edits}"
        );
    }
    let rebuilds = Rollback::stud_collider_rebuilds() - rebuilds0;
    assert!(rebuilds < 200, "most edits patch in place: {rebuilds} rebuilds");
}

#[test]
fn box_edits_patch_without_rebuild() {
    let base = chunk([
        filler([4, 15, 4], [0, 0, 0]),
        filler([4, 15, 4], [4, 0, 0]),
        brick("3001", [20, 0, 20]),
    ]);
    let (mut r, _rx) = genesis(base.clone());
    let key = r.stud_key_at(C).unwrap();
    let rebuilds0 = Rollback::stud_collider_rebuilds();
    let mut cur = base;
    let steps: [&dyn Fn(&mut StudChunk); 5] = [
        &|c| c.place(brick("3001", [0, 15, 0])).unwrap(),
        &|c| c.place(brick("3003", [20, 3, 20])).unwrap(),
        &|c| { c.remove_at([0, 15, 0]).unwrap(); },
        // Breaks one coarse cell (the coarse grid keeps the other).
        &|c| { c.remove_at([4, 0, 0]).unwrap(); },
        &|c| { c.remove_at([20, 3, 20]).unwrap(); },
    ];
    for (i, step) in steps.iter().enumerate() {
        step(&mut cur);
        r.new_transaction();
        r.set_stud_chunk_safe(key, cur.clone());
        assert_eq!(enabled_shape_hashes(&r, key), fresh_shape_hashes(&cur), "step {i}");
    }
    assert_eq!(Rollback::stud_collider_rebuilds(), rebuilds0, "every edit patched");
}

#[test]
fn patch_rolls_back() {
    let base = chunk([filler([4, 15, 4], [0, 0, 0]), brick("3001", [10, 0, 10])]);
    let (mut r, _rx) = genesis(base.clone());
    let key = r.stud_key_at(C).unwrap();
    let rebuilds0 = Rollback::stud_collider_rebuilds();
    let b0 = state_hash(&r);
    r.new_transaction();
    let mut placed = base.clone();
    placed.place(brick("3001", [0, 15, 0])).unwrap();
    r.set_stud_chunk_safe(key, placed.clone());
    let b1 = state_hash(&r);
    assert_ne!(b1, b0);
    r.new_transaction();
    let mut removed = placed;
    removed.remove_at([10, 0, 10]).unwrap();
    r.set_stud_chunk_safe(key, removed);
    assert_ne!(state_hash(&r), b1);
    assert_eq!(Rollback::stud_collider_rebuilds(), rebuilds0, "both edits patched");
    r.rollback();
    assert_eq!(state_hash(&r), b1);
    r.rollback();
    assert_eq!(state_hash(&r), b0);
    assert_eq!(enabled_shape_hashes(&r, key), fresh_shape_hashes(&base));
}

/// A key that is not a stud entity is a caller bug: debug builds assert,
/// release builds write nothing.
#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "not a stud chunk entity"))]
fn set_on_a_non_stud_key_writes_nothing() {
    let (mut r, _rx) = genesis(chunk([brick("3001", [0, 0, 0])]));
    r.new_transaction();
    let plain = r.ecs.create_entity_safe();
    assert!(r.ecs.stud.try_get(plain).is_none());
    let before = state_hash(&r);
    r.set_stud_chunk_safe(plain, chunk([brick("3001", [0, 0, 0])]));
    assert_eq!(state_hash(&r), before);
    assert!(r.ecs.stud.try_get(plain).is_none());
}

/// `set_stud_chunk_safe` on a stud entity that was itself created at runtime
/// (create in tx1, set in tx2): each rollback restores its boundary hash.
#[test]
fn set_on_a_runtime_created_stud_rolls_back() {
    let (send, _rx) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let c = StudChunkCoords { x: 0, y: 0, z: 0 };
    let b0 = state_hash(&r);
    r.new_transaction();
    let key = r.create_stud_chunk_safe(c, chunk([brick("3001", [0, 0, 0])]));
    let b1 = state_hash(&r);
    assert_ne!(b1, b0);
    r.new_transaction();
    r.set_stud_chunk_safe(key, chunk([brick("3001", [0, 0, 0]), brick("3001", [10, 0, 10])]));
    assert_ne!(state_hash(&r), b1);
    r.rollback();
    assert_eq!(state_hash(&r), b1);
    r.rollback();
    assert_eq!(state_hash(&r), b0);
    assert!(r.stud_key_at(c).is_none());
}
