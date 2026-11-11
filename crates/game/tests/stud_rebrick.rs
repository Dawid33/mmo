//! Re-bricking on exposure (`game::stud_rebrick`): a hole dug into filler ground has
//! real bricks behind every face; the rest stays filler.
use std::collections::BTreeSet;

use game::*;
use state_hash::StateHash;

use crate::common::{self, sc};

const CW: i32 = STUD_CHUNK_STUDS as i32;
const CH: i32 = STUD_CHUNK_PLATES as i32;

fn hash(r: &Rollback) -> u32 {
    let mut h = crc32fast::Hasher::new();
    r.state_hash(&mut h);
    h.finalize()
}

fn shape_hash<T: StateHash + ?Sized>(v: &T) -> u32 {
    let mut h = crc32fast::Hasher::new();
    v.state_hash(&mut h);
    h.finalize()
}

/// Deep flat ground (surface at plate 30 of layer 0) under the whole region.
fn world() -> Rollback {
    // A render sink nobody reads (a rollback needs one).
    let mut r = Rollback::new(Some(crossbeam::channel::unbounded().0));
    common::seed_studs(&mut r, common::uniform_seed());
    r
}

fn brick_at(r: &Rollback, cell: [i32; 3]) -> Option<StudBrick> {
    let c = sc((cell[0] / CW) as usize, cell[1].div_euclid(CH), (cell[2] / CW) as usize);
    let local = [(cell[0] % CW) as u8, cell[1].rem_euclid(CH) as u8, (cell[2] % CW) as u8];
    r.stud_chunk_or_pristine(c)?.brick_at(local).copied()
}

fn is_part(b: &StudBrick) -> bool {
    matches!(b.shape, StudShape::Part { .. })
}

/// Every real brick of the materialised chunks, by chunk.
fn parts(r: &Rollback) -> BTreeSet<(StudChunkCoords, [u8; 3], [u8; 3], u16, u16)> {
    let mut out = BTreeSet::new();
    for (c, _) in r.stud_index.iter() {
        for b in r.stud_chunk_at(*c).unwrap().bricks().filter(|b| is_part(b)) {
            out.insert((*c, b.min, b.size(), b.material, b.data));
        }
    }
    out
}

/// Every solid cell touching an open cell of `lo .. hi` across a face is a real brick
/// lying inside its slot.
fn assert_shell(r: &Rollback, lo: [i32; 3], hi: [i32; 3]) {
    for axis in 0..3 {
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        for (plane, inward) in [(lo[axis] - 1, 1), (hi[axis], -1)] {
            for a in lo[u]..hi[u] {
                for b in lo[v]..hi[v] {
                    let mut p = [0; 3];
                    p[axis] = plane;
                    p[u] = a;
                    p[v] = b;
                    let mut q = p;
                    q[axis] += inward;
                    if p[0] < 0 || p[2] < 0 || brick_at(r, q).is_some() {
                        continue;
                    }
                    let Some(brick) = brick_at(r, p) else { continue };
                    assert!(is_part(&brick), "cell {p:?} behind the hole is still {brick:?}");
                    let local = [(p[0] % CW) as u8, p[1].rem_euclid(CH) as u8, (p[2] % CW) as u8];
                    let (s_lo, s_hi) = rebrick_slot(p[1].div_euclid(CH), local);
                    let size = brick.size();
                    assert!((0..3).all(|i| brick.min[i] >= s_lo[i] && brick.min[i] + size[i] <= s_hi[i]), "{brick:?} leaves its slot");
                    assert_eq!(mining::stud_target_box(&brick, local), (brick.min, [0, 1, 2].map(|i| brick.min[i] + size[i])), "the target is the brick");
                }
            }
        }
    }
}

fn assert_colliders_fresh(r: &Rollback) {
    for (c, key) in r.stud_index.iter() {
        let h = r.ecs.rigidbody.try_get(*key).unwrap();
        let have: Vec<u32> = r
            .physics
            .bodies
            .get(h)
            .unwrap()
            .colliders()
            .iter()
            .map(|&ch| r.physics.colliders.get(ch).unwrap())
            .filter(|c| c.is_enabled())
            .map(|c| shape_hash(c.shared_shape()))
            .collect();
        let want: Vec<u32> = stud_collider_shapes(r.stud_chunk_at(*c).unwrap()).iter().map(|s| shape_hash(s)).collect();
        assert_eq!(have, want, "colliders of {c:?}");
    }
}

/// A tile well inside chunk (0, 0, 0)'s filler: studs 8..10 x 4..6, plates 6..12.
const HOLE: ([i32; 3], [i32; 3]) = ([8, 6, 4], [10, 12, 6]);

#[test]
fn the_slot_lattice_is_the_surface_bond() {
    // One full layer of one material, as the surface lays it: every brick is a slot.
    let g = StudColumnGround::flat(30, StudProfile { cover: MAT_GRASS, cover_plates: 0, soil: MAT_DIRT, soil_plates: 6 }, MAT_STONE, -2000);
    let chunk = g.pristine_chunk(0).unwrap();
    let mut seen = 0;
    for b in chunk.bricks().filter(|b| is_part(b) && b.material == MAT_DIRT && b.min[1] == 27) {
        let size = b.size();
        assert_eq!(rebrick_slot(0, b.min), (b.min, [0, 1, 2].map(|i| b.min[i] + size[i])), "{b:?}");
        seen += 1;
    }
    assert_eq!(seen, 16 * 4 + 8, "the top course: 8 plain strips of 4 bricks, 8 staggered ones of 5");
    // Slots tile the chunk, on every layer parity.
    for layer in [-1, 0] {
        for y in [0u8, 3, 87] {
            for z in 0..CW as u8 {
                for x in 0..CW as u8 {
                    let (lo, hi) = rebrick_slot(layer, [x, y, z]);
                    assert!((0..3).all(|i| lo[i] <= [x, y, z][i] && [x, y, z][i] < hi[i]));
                    assert_eq!(rebrick_slot(layer, lo), (lo, hi));
                    assert_eq!(rebrick_slot(layer, [hi[0] - 1, hi[1] - 1, hi[2] - 1]), (lo, hi));
                }
            }
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_dug_tile_has_brick_walls_and_floor_and_undoes() {
    let mut r = world();
    let pristine = r.stud_chunk_at(sc(0, 0, 0)).unwrap().clone();
    assert!(!is_part(&brick_at(&r, [8, 5, 4]).unwrap()), "filler before the dig");
    let before = hash(&r);
    r.new_transaction();
    assert!(!r.carve_stud_safe(HOLE.0, HOLE.1).is_empty());
    assert_shell(&r, HOLE.0, HOLE.1);
    // Interior ground stays filler; occupancy is the pristine one less the hole.
    assert!(!is_part(&brick_at(&r, [24, 8, 24]).unwrap()));
    assert!(!is_part(&brick_at(&r, [8, 2, 4]).unwrap()), "a course below the floor's bricks");
    let mut carved = pristine.clone();
    carved.carve(HOLE.0.map(|v| v as u8), HOLE.1.map(|v| v as u8));
    let now = r.stud_chunk_at(sc(0, 0, 0)).unwrap().clone();
    assert!(now.occupancy() == carved.occupancy(), "re-bricking moves no cell");
    for b in now.bricks() {
        assert_eq!(pristine.brick_at(b.min).map(|p| (p.material, p.data)), Some((b.material, b.data)));
    }
    // The chunk is what the brute-force rule gives.
    common::rebrick_reference(&mut carved, 0, |_| false);
    assert!(now == carved, "the planned re-brick is the rule's");
    assert_colliders_fresh(&r);
    // Idempotent.
    let after = hash(&r);
    r.rebrick_stud_exposed_safe(&[(HOLE.0.map(i64::from), HOLE.1.map(i64::from))]);
    assert_eq!(hash(&r), after);
    // Snapshot round trip.
    let wire = encode_packet(&ServerPacket::Region(RegionCoords::new(0, 0), r.clone())).unwrap();
    let ServerPacket::Region(_, back) = decode_packet(&wire).unwrap() else { panic!("a region packet") };
    assert_eq!(hash(&back), after);
    r.rollback();
    assert_eq!(hash(&r), before, "the rollback bar");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn digging_on_keeps_the_bricks_already_there() {
    let mut r = world();
    r.carve_stud_safe(HOLE.0, HOLE.1);
    let first = parts(&r);
    // The next tile east.
    let (lo, hi) = ([10, 6, 4], [12, 12, 6]);
    let before = hash(&r);
    r.new_transaction();
    r.carve_stud_safe(lo, hi);
    let second = parts(&r);
    let cut = |b: &(StudChunkCoords, [u8; 3], [u8; 3], u16, u16)| (0..3).all(|i| (b.1[i] as i32) < hi[i] && lo[i] < (b.1[i] + b.2[i]) as i32);
    for b in first.iter().filter(|b| !cut(b)) {
        assert!(second.contains(b), "{b:?} was re-shuffled");
    }
    assert!(second.len() > first.len());
    assert_shell(&r, HOLE.0, hi);
    assert_colliders_fresh(&r);
    r.rollback();
    assert_eq!(hash(&r), before);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn the_bricks_do_not_depend_on_the_order_of_digging() {
    // Three tiles: two neighbours sharing slots and one a few studs off, whose shells
    // cut the same slabs — in every order the real bricks come out the same.
    let tiles = [HOLE, ([10, 6, 4], [12, 12, 6]), ([8, 6, 12], [10, 12, 14])];
    let dig = |order: [usize; 3]| {
        let mut r = world();
        for i in order {
            r.new_transaction();
            assert!(!r.carve_stud_safe(tiles[i].0, tiles[i].1).is_empty());
        }
        parts(&r)
    };
    let want = dig([0, 1, 2]);
    for order in [[1, 0, 2], [2, 1, 0], [2, 0, 1]] {
        assert!(dig(order) == want, "order {order:?}");
    }
    // ...and as one batch.
    let mut r = world();
    r.carve_stud_boxes_safe(&tiles);
    assert!(parts(&r) == want, "one batch");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_hole_at_a_chunk_edge_and_a_layer_boundary_re_bricks_the_neighbours() {
    let mut r = world();
    assert!(r.stud_chunk_at(sc(0, -1, 0)).is_none(), "the layer below is implied");
    let before = hash(&r);
    r.new_transaction();
    // Against chunk (1, 0, 0) and the layer below.
    let (lo, hi) = ([CW - 2, 0, 4], [CW, 6, 6]);
    assert!(!r.carve_stud_safe(lo, hi).is_empty());
    assert_shell(&r, lo, hi);
    assert!(is_part(&brick_at(&r, [CW, 1, 4]).unwrap()), "the next chunk's wall");
    assert!(r.stud_chunk_at(sc(0, -1, 0)).is_some(), "the layer below is materialised");
    assert!(is_part(&brick_at(&r, [CW - 2, -1, 4]).unwrap()), "the floor in the layer below");
    assert!(!is_part(&brick_at(&r, [CW - 2, -4, 4]).unwrap()), "a course further down stays filler");
    assert_colliders_fresh(&r);
    r.rollback();
    assert_eq!(hash(&r), before);
    assert!(r.stud_chunk_at(sc(0, -1, 0)).is_none());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn removing_a_brick_re_bricks_what_it_uncovers() {
    let mut r = world();
    r.carve_stud_safe(HOLE.0, HOLE.1);
    // The floor brick under the hole goes by `RemoveStud`: the filler under it shows.
    let floor = brick_at(&r, [8, 5, 4]).unwrap();
    assert!(is_part(&floor));
    let before = hash(&r);
    r.new_transaction();
    assert!(r.remove_stud_safe(sc(0, 0, 0), floor.min));
    let s = floor.size();
    let lo = floor.min.map(i32::from);
    assert_shell(&r, lo, [0, 1, 2].map(|i| lo[i] + s[i] as i32));
    assert_colliders_fresh(&r);
    r.rollback();
    assert_eq!(hash(&r), before);
}

/// The cost of the shell: real bricks and fillers a dig adds to the chunks it edits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn census_of_bricks_added() {
    let count = |r: &Rollback| {
        let (mut real, mut filler, mut chunks) = (0, 0, 0);
        for (c, _) in r.stud_index.iter() {
            let chunk = r.stud_chunk_at(*c).unwrap();
            let pristine = r.stud_ground.get(&ColumnCoords::new(c.x, c.z)).and_then(|g| g.pristine_chunk(c.y));
            if pristine.as_ref() == Some(chunk) {
                continue;
            }
            chunks += 1;
            real += chunk.bricks().filter(|b| is_part(b)).count() as i64 - pristine.as_ref().map_or(0, |p| p.bricks().filter(|b| is_part(b)).count() as i64);
            filler += chunk.bricks().filter(|b| !is_part(b)).count() as i64 - pristine.as_ref().map_or(0, |p| p.bricks().filter(|b| !is_part(b)).count() as i64);
        }
        (real, filler, chunks)
    };
    let wire = |r: &Rollback| encode_packet(&ServerPacket::Region(RegionCoords::new(0, 0), r.clone())).unwrap().len() as i64;
    let base = wire(&world());
    let run = |name: &str, lo: [i32; 3], hi: [i32; 3]| {
        let mut r = world();
        // Tile by tile, as digging goes.
        let (ts, tp) = (TILE_STUDS as i32, TILE_PLATES as i32);
        let mut tiles = 0;
        for y in (lo[1]..hi[1]).step_by(tp as usize) {
            for z in (lo[2]..hi[2]).step_by(ts as usize) {
                for x in (lo[0]..hi[0]).step_by(ts as usize) {
                    r.carve_stud_safe([x, y, z], [x + ts, y + tp, z + ts]);
                    tiles += 1;
                }
            }
        }
        let (real, filler, chunks) = count(&r);
        println!("rebrick census: {name}: {tiles} tiles, {chunks} edited chunks, +{real} real bricks, +{filler} fillers, wire +{} bytes", wire(&r) - base);
        (real, filler)
    };
    let (real, _) = run("one tile", HOLE.0, HOLE.1);
    assert!(real > 0 && real < 40, "{real}");
    let (real, filler) = run("10 x 10 x 3 tile room", [40, 0, 40], [60, 18, 60]);
    assert!(real < 600 && filler < 2000, "{real} {filler}");
    run("32-tile corridor", [0, 6, 40], [64, 12, 42]);
}

// --- Natural cliffs: the pristine chunk is exposure-aware -------------------------

/// A step between two stud columns INSIDE a chunk column: the face is real bricks
/// (the surface's own courses reach down every face inside a chunk).
#[test]
fn a_cliff_inside_a_chunk_is_bricks() {
    let g = common::stud_ground(|x, _| if x < 16 { 12 } else { 80 });
    let chunk = g.pristine_chunk(0).unwrap();
    for z in 0..CW as u8 {
        for y in 12..80 {
            let b = chunk.brick_at([16, y, z]).expect("solid");
            assert!(is_part(b), "the face at plate {y}, z {z} is {b:?}");
        }
    }
}

/// A cliff at a chunk column's side face, known from the recipe's apron: the pristine
/// chunk is exactly the closed one with the dig rule applied to the open face — a
/// natural wall and a dug one are the same bricks — and it stays a function of the
/// recipe (the same chunk however often and wherever it is derived).
#[test]
fn a_cliff_at_a_chunk_face_is_the_dug_wall() {
    let closed = common::stud_flat(2 * CH + 30);
    let mut open = closed.clone();
    // West: a plain at plate 30. North: a ragged slope from plate 100 down to 40.
    open.set_apron_side(StudSide::West, &[30; STUD_CHUNK_STUDS]);
    let north: Vec<i32> = (0..CW).map(|x| 100 - 2 * x + (x % 3)).collect();
    open.set_apron_side(StudSide::North, &north);
    assert_eq!(closed.genesis_layers(), 2..=2);
    assert_eq!(open.genesis_layers(), 0..=2, "the wall's layers are genesis layers");
    for layer in 0..=2 {
        let base = layer * CH;
        let mut want = closed.pristine_chunk(layer).unwrap();
        common::rebrick_reference(&mut want, layer, |q| {
            let plate = base + q[1];
            (q[0] == -1 && (0..CW).contains(&q[2]) && plate >= 30) || (q[2] == -1 && (0..CW).contains(&q[0]) && plate >= north[q[0] as usize])
        });
        let have = open.pristine_chunk(layer).unwrap();
        assert!(have == want, "layer {layer}: the cliff is the dug wall");
        assert!(have == open.clone().pristine_chunk(layer).unwrap());
        assert!(have.occupancy() == closed.pristine_chunk(layer).unwrap().occupancy());
        if layer < 2 {
            assert!(have != closed.pristine_chunk(layer).unwrap());
            for (cell, part) in [([0u8, 45, 9], base + 45 >= 30), ([16, 45, 16], false)] {
                assert_eq!(is_part(have.brick_at(cell).unwrap()), part, "layer {layer} cell {cell:?}");
            }
        }
    }
}
