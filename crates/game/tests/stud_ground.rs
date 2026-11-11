//! The stud ground recipe and its pristine chunks (`stud_ground.rs`).
use game::*;

const SOIL: StudProfile = StudProfile { cover: MAT_GRASS, cover_plates: 1, soil: MAT_DIRT, soil_plates: 5 };
const N: usize = STUD_CHUNK_STUDS;

/// A recipe from a per-column surface height, dry, bedrock at plate -200.
fn ground(surface: impl Fn(usize, usize) -> i32) -> StudColumnGround {
    let s: Vec<i32> = (0..N * N).map(|i| surface(i % N, i / N)).collect();
    StudColumnGround::new(s.into(), vec![STUD_NO_WATER; N * N].into(), vec![0; N * N].into(), vec![SOIL], vec![0; N * N].into(), vec![(i32::MAX, MAT_STONE)], -200).unwrap()
}

fn hills(x: usize, z: usize) -> i32 {
    // 20..=64 plates, uneven in both directions, crossing course boundaries.
    20 + ((x * 7 + z * 13) % 23) as i32 + ((x / 5 + z / 3) % 4) as i32 * 7
}

/// Every cell of the chunk agrees with the recipe: occupancy and material.
fn assert_matches(g: &StudColumnGround, y: i32) {
    let base = y * STUD_CHUNK_PLATES as i32;
    let Some(chunk) = g.pristine_chunk(y) else {
        for i in 0..N * N {
            assert!(g.surface(i % N, i / N) <= base, "layer {y} is not all air");
        }
        return;
    };
    // Valid, in-chunk, disjoint, canonical.
    assert_eq!(StudChunk::new(chunk.bricks().copied()).unwrap(), chunk);
    let mut got = vec![None; N * STUD_CHUNK_PLATES * N];
    for b in chunk.bricks() {
        let top = b.min[1] + b.size()[1] - 1;
        let filler = matches!(b.shape, StudShape::Filler { .. });
        for c in b.cells() {
            got[(c[1] as usize * N + c[2] as usize) * N + c[0] as usize] = Some((b.material, top, filler));
        }
    }
    for p in 0..STUD_CHUNK_PLATES {
        for z in 0..N {
            for x in 0..N {
                let want = g.material_at(x, base + p as i32, z);
                let have = got[(p * N + z) * N + x];
                assert_eq!(have.is_some(), want.is_some(), "occupancy at {x},{p},{z} of layer {y}");
                // A filler is its material throughout; a part carries its top plate's.
                if let Some((m, top, filler)) = have {
                    let at = if filler { base + p as i32 } else { base + top as i32 };
                    assert_eq!(Some(m), g.material_at(x, at, z), "material at {x},{p},{z} (filler {filler})");
                }
            }
        }
    }
}

#[test]
fn pristine_chunks_match_the_recipe() {
    let g = ground(hills);
    for y in -4..=2 {
        assert_matches(&g, y);
    }
    assert!(g.pristine_chunk(1).is_none());
    assert_eq!(g.surface_layers(), 0..=0);
}

#[test]
fn surface_crossing_layers_matches_too() {
    // 70..=115 plates: the surface crosses the layer 0 / 1 boundary at plate 90.
    let g = ground(|x, z| 70 + ((x * 3 + z * 5) % 46) as i32);
    assert_eq!(g.surface_layers(), 0..=1);
    for y in -1..=2 {
        assert_matches(&g, y);
    }
}

#[test]
fn buried_chunk_is_fillers_and_bedrock() {
    let g = ground(hills);
    let deep = g.pristine_chunk(-1).unwrap();
    assert_eq!(deep.len(), 1);
    let b = deep.bricks().next().unwrap();
    assert_eq!((b.shape, b.material), (StudShape::Filler { size: [32, 90, 32] }, MAT_STONE));
    // Bedrock starts at plate -200 = local plate 70 of layer -3.
    let bed = g.pristine_chunk(-3).unwrap();
    let got: Vec<_> = bed.bricks().map(|b| (b.min[1], b.size()[1], b.material)).collect();
    assert_eq!(got, vec![(0, 70, MAT_BEDROCK), (70, 20, MAT_STONE)]);
    assert_eq!(g.pristine_chunk(-4).unwrap().bricks().next().unwrap().material, MAT_BEDROCK);
}

#[test]
fn flat_ground_is_few_big_bricks_with_running_bond() {
    let g = StudColumnGround::flat(30, SOIL, MAT_STONE, -200);
    let c = g.pristine_chunk(0).unwrap();
    let is_filler = |b: &&StudBrick| matches!(b.shape, StudShape::Filler { .. });
    // 24 plates of stone in one slab. The course under the top one is hidden dirt:
    // 4 x 4 fillers inside, real bricks only in the ring along the chunk edge.
    let slab = c.brick_at([10, 0, 10]).unwrap();
    assert_eq!((slab.size(), slab.material), ([32, 24, 32], MAT_STONE));
    let inner = c.brick_at([10, 25, 10]).unwrap();
    assert_eq!((inner.shape, inner.material), (StudShape::Filler { size: [4, 3, 4] }, MAT_DIRT));
    assert!(matches!(c.brick_at([0, 25, 10]).unwrap().shape, StudShape::Part { .. }));
    assert_eq!(c.bricks().filter(is_filler).count(), 1 + 6 * 6);
    // The top course is all 2 x 8 bricks (2 x 4 at the start of odd strips): 8 x 4 + 8 x 5.
    let top: Vec<_> = c.bricks().filter(|b| b.min[1] == 27).collect();
    assert_eq!(top.len(), 8 * 4 + 8 * 5);
    assert!(top.iter().all(|b| b.material == MAT_GRASS && matches!(b.shape, StudShape::Part { .. })));
    let long = |s: [u8; 3]| s[0].max(s[2]);
    let first = |x, z| long(c.brick_at([x, 27, z]).unwrap().size());
    let firsts = [first(0, 0), first(0, 2), first(2, 0)];
    assert!(firsts.contains(&4) && firsts.contains(&8), "running bond: {firsts:?}");
}

#[test]
fn hidden_ground_is_fillers_and_the_shell_is_parts() {
    let g = ground(hills);
    let c = g.pristine_chunk(0).unwrap();
    // Every top cell of every column is a real part (studs, seams), never a filler.
    for z in 0..N {
        for x in 0..N {
            let top = c.top_plate(x as u8, z as u8).unwrap();
            assert!(matches!(c.brick_at([x as u8, top - 1, z as u8]).unwrap().shape, StudShape::Part { .. }));
        }
    }
    // A side face of a filler never shows: wherever a filler cell has a lower
    // neighbouring column in the chunk, that neighbour still covers it.
    for b in c.bricks().filter(|b| matches!(b.shape, StudShape::Filler { .. })) {
        let top = (b.min[1] + b.size()[1]) as i32;
        for cell in b.cells().filter(|cell| cell[1] as i32 == top - 1) {
            for (dx, dz) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                let (nx, nz) = (cell[0] as i32 + dx, cell[2] as i32 + dz);
                if (0..N as i32).contains(&nx) && (0..N as i32).contains(&nz) {
                    assert!(g.surface(nx as usize, nz as usize) >= top, "filler top at {cell:?} shows beside {nx},{nz}");
                }
            }
        }
    }
}

#[test]
fn pristine_is_a_pure_function_of_the_recipe() {
    let (a, b) = (ground(hills), ground(hills));
    assert_eq!(a.pristine_chunk(0), b.pristine_chunk(0));
    assert_eq!(state_hash_of(&a.pristine_chunk(0)), state_hash_of(&b.pristine_chunk(0)));
}

fn state_hash_of<T: state_hash::StateHash>(v: &T) -> u32 {
    let mut h = crc32fast::Hasher::new();
    v.state_hash(&mut h);
    h.finalize()
}

#[test]
fn recipe_validates_its_inputs() {
    let full = |n: i32| vec![n; N * N].into_boxed_slice();
    let make = |profile: Box<[u8]>, profiles: Vec<StudProfile>, strata: Vec<(i32, u16)>| {
        StudColumnGround::new(full(0), full(STUD_NO_WATER), profile, profiles, full(0), strata, 0).err()
    };
    let stone = || vec![(i32::MAX, MAT_STONE)];
    let zeros = || vec![0u8; N * N].into_boxed_slice();
    assert_eq!(make(zeros(), vec![SOIL], stone()), None);
    assert_eq!(make(vec![1; N * N].into(), vec![SOIL], stone()), Some(StudGroundError::UnknownProfile));
    assert_eq!(make(vec![0; 5].into(), vec![SOIL], stone()), Some(StudGroundError::WrongFootprint));
    assert_eq!(make(zeros(), vec![StudProfile { soil: 9999, ..SOIL }], stone()), Some(StudGroundError::UnknownMaterial));
    assert_eq!(make(zeros(), vec![SOIL], vec![(0, 9999)]), Some(StudGroundError::UnknownMaterial));
    assert_eq!(make(zeros(), vec![SOIL], vec![]), Some(StudGroundError::BadStrata));
    assert_eq!(make(zeros(), vec![SOIL], vec![(0, MAT_STONE), (5, MAT_SAND)]), Some(StudGroundError::BadStrata));
}

/// Formations: granite above s = -30, shale down to -100, basalt below.
fn layered(geo: impl Fn(usize, usize) -> i32) -> StudColumnGround {
    let g: Vec<i32> = (0..N * N).map(|i| geo(i % N, i / N)).collect();
    let granite = stud_material_id("granite").unwrap();
    let shale = stud_material_id("shale").unwrap();
    let basalt = stud_material_id("basalt").unwrap();
    StudColumnGround::new(
        vec![40; N * N].into(),
        vec![STUD_NO_WATER; N * N].into(),
        vec![0; N * N].into(),
        vec![SOIL],
        g.into(),
        vec![(i32::MAX, granite), (-30, shale), (-100, basalt)],
        -100_000,
    )
    .unwrap()
}

/// Every cell's material is the recipe's (fillers and bricks alike), for a rock recipe
/// where bricks are single-material by construction of the test (cells checked exactly).
fn assert_rock_exact(g: &StudColumnGround, y: i32) {
    let base = y * STUD_CHUNK_PLATES as i32;
    let chunk = g.pristine_chunk(y).unwrap();
    assert_eq!(StudChunk::new(chunk.bricks().copied()).unwrap(), chunk);
    let mut cells = 0;
    for b in chunk.bricks() {
        let top = b.min[1] + b.size()[1] - 1;
        for c in b.cells() {
            cells += 1;
            let want = g.material_at(c[0] as usize, base + c[1] as i32, c[2] as usize);
            assert!(want.is_some(), "brick in air at {c:?}");
            // A filler is one rock throughout; a part carries its top plate's material.
            if matches!(b.shape, StudShape::Filler { .. }) {
                assert_eq!(want, Some(b.material), "filler cell {c:?} of layer {y}");
            } else {
                assert_eq!(g.material_at(c[0] as usize, base + top as i32, c[2] as usize), Some(b.material));
            }
        }
    }
    assert_eq!(cells, N * STUD_CHUNK_PLATES * N, "a buried chunk is full");
}

#[test]
fn level_strata_become_slabs() {
    let g = layered(|_, _| 0);
    // Layer -1 (plates -90..0) holds basalt below -100... no: s = plate, so shale is
    // -100..-30 and granite above: plates -90..-30 shale, -30..0 granite.
    let c = g.pristine_chunk(-1).unwrap();
    let got: Vec<_> = c.bricks().map(|b| (b.min[1], b.size()[1], stud_material(b.material).unwrap().name)).collect();
    assert_eq!(got, vec![(0, 60, "shale"), (60, 30, "granite")]);
    assert_rock_exact(&g, -1);
    // Layer -2 (plates -180..-90): basalt below -100, shale above.
    let c = g.pristine_chunk(-2).unwrap();
    let got: Vec<_> = c.bricks().map(|b| (b.min[1], b.size()[1], stud_material(b.material).unwrap().name)).collect();
    assert_eq!(got, vec![(0, 80, "basalt"), (80, 10, "shale")]);
}

#[test]
fn tilted_strata_are_exact_and_single_formation_chunks_stay_one_filler() {
    let g = layered(|x, z| (x + z) as i32 / 4);
    // Deep inside the basalt: one filler however the beds tilt.
    assert_eq!(g.pristine_chunk(-5).unwrap().len(), 1);
    // Where tilted beds cross the chunk, every filler cell still has its own rock.
    assert_rock_exact(&g, -1);
    assert_rock_exact(&g, -2);
}

#[test]
fn extreme_layers_do_not_overflow() {
    let g = ground(hills);
    assert!(g.pristine_chunk(i32::MAX).is_none());
    assert!(g.pristine_chunk(i32::MIN).is_none());
    assert!(g.pristine_chunk(STUD_LAYER_LIMIT).is_none());
    assert_eq!(g.pristine_chunk(-STUD_LAYER_LIMIT).unwrap().len(), 1);
}

// ---- the recipe in the sim (`stud_ops.rs`) ----

fn col(x: usize, z: usize) -> ColumnCoords {
    ColumnCoords::new(x, z)
}

/// Two chunk columns of flat ground at plate 30 (layer 0), bedrock far below.
fn sim() -> Rollback {
    // Updates are not read here; the receiver is dropped.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let flat = || StudColumnGround::flat(30, SOIL, MAT_STONE, -2000);
    r.seed_stud_ground_genesis(vec![(col(0, 0), flat()), (col(1, 0), flat())], Default::default());
    r
}

#[test]
fn genesis_materialises_only_the_surface_layer() {
    let r = sim();
    let at = |x, y| StudChunkCoords { x, y, z: 0 };
    assert!(r.stud_chunk_at(at(0, 0)).is_some() && r.stud_chunk_at(at(1, 0)).is_some());
    assert!(r.stud_chunk_at(at(0, -1)).is_none() && r.stud_chunk_at(at(0, 1)).is_none());
    assert_eq!(r.stud_chunk_at(at(0, 0)), StudColumnGround::flat(30, SOIL, MAT_STONE, -2000).pristine_chunk(0).as_ref());
    // Implied ground still answers reads; air and outside the region do not.
    assert_eq!(r.stud_material_at([5, 29, 5]), Some(MAT_GRASS));
    assert_eq!(r.stud_material_at([5, 10, 5]), Some(MAT_STONE));
    assert_eq!(r.stud_material_at([5, -500, 5]), Some(MAT_STONE));
    assert_eq!(r.stud_material_at([5, -2001, 5]), Some(MAT_BEDROCK));
    assert_eq!(r.stud_material_at([5, 30, 5]), None);
    assert_eq!(r.stud_material_at([-1, 10, 5]), None);
    assert_eq!(r.stud_material_at([5, 10, 3 * 32]), None, "no recipe there");
    assert_eq!(r.stud_surface_y(4.0, 4.0), Some(plate_units(30)));
}

#[test]
fn carving_into_implied_ground_materialises_it_and_rolls_back() {
    let mut r = sim();
    let before = state_hash(&r);
    let below = StudChunkCoords { x: 0, y: -1, z: 0 };
    // A shaft from the surface down through the layer boundary at plate 0.
    r.new_transaction();
    let removed = r.carve_stud_safe([4, -6, 4], [6, 30, 6]);
    let cells: u32 = removed.iter().map(|&(_, n)| n).sum();
    assert_eq!(cells, 2 * 36 * 2);
    assert!(removed.iter().any(|&(m, _)| m == MAT_GRASS) && removed.iter().any(|&(m, _)| m == MAT_STONE));
    let deep = r.stud_chunk_at(below).expect("the layer under the shaft now exists");
    assert!(!deep.occupancy().get([4, 89, 4]) && deep.occupancy().get([4, 83, 4]) && deep.occupancy().get([3, 89, 4]));
    assert_eq!(r.stud_material_at([4, -1, 4]), None, "the materialised chunk wins over the recipe");
    assert_eq!(r.stud_material_at([4, -7, 4]), Some(MAT_STONE));
    r.rollback();
    assert!(r.stud_chunk_at(below).is_none());
    assert_eq!(state_hash(&r), before);
}

#[test]
fn a_carve_at_a_chunk_face_materialises_the_neighbour() {
    let mut r = sim();
    // Deep in column 0, against the x face shared with column 1, wholly inside layer -1.
    let removed = r.carve_stud_safe([30, -40, 4], [32, -34, 6]);
    assert_eq!(removed, vec![(MAT_STONE, 2 * 6 * 2)]);
    let side = StudChunkCoords { x: 1, y: -1, z: 0 };
    assert!(r.stud_chunk_at(StudChunkCoords { x: 0, y: -1, z: 0 }).is_some());
    // The exposed neighbour has its ground and collider: the pristine cells, the wall
    // behind the hole as real bricks (`stud_rebrick`).
    let pristine = r.stud_ground.get(&ColumnCoords::new(1, 0)).unwrap().pristine_chunk(-1).unwrap();
    let wall = r.stud_chunk_at(side).expect("materialised");
    assert!(wall.occupancy() == pristine.occupancy());
    assert!(matches!(wall.brick_at([0, 52, 4]).map(|b| b.shape), Some(StudShape::Part { .. })));
    assert!(matches!(wall.brick_at([0, 46, 4]).map(|b| b.shape), Some(StudShape::Filler { .. })));
    // Not the ones the hole does not touch.
    assert!(r.stud_chunk_at(StudChunkCoords { x: 0, y: -2, z: 0 }).is_none());
}

#[test]
fn carving_air_or_bedrock_touches_nothing() {
    let mut r = sim();
    let before = state_hash(&r);
    assert!(r.carve_stud_safe([4, 40, 4], [6, 46, 6]).is_empty());
    assert!(r.carve_stud_safe([4, -2100, 4], [6, -2094, 6]).is_empty());
    assert!(r.stud_chunk_at(StudChunkCoords { x: 0, y: -24, z: 0 }).is_none());
    assert_eq!(state_hash(&r), before);
}

#[test]
fn features_merge_into_the_seeded_chunk() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(None), Some(send), RegionCoords::new(0, 0), None);
    let c = StudChunkCoords { x: 0, y: 0, z: 0 };
    let post = StudBrick::part_at([10, 30, 10], box_part([1, 3, 1]).unwrap().0, 0, 4);
    let buried = StudBrick::part_at([10, 20, 10], box_part([1, 3, 1]).unwrap().0, 0, 4);
    region.seed_stud_world(StudRegionSeed {
        ground: vec![(col(0, 0), StudColumnGround::flat(30, SOIL, MAT_STONE, -2000))],
        features: vec![(c, vec![post, buried])],
        ..Default::default()
    });
    let chunk = region.data().stud_chunk_at(c).unwrap();
    assert_eq!(chunk.brick_at([10, 31, 10]).map(|b| b.material), Some(4));
    assert_eq!(chunk.brick_at([10, 20, 10]).map(|b| b.material), Some(MAT_STONE), "an overlapping feature brick is dropped");
    assert_eq!(region.data().body_scale(), movement::BodyScale::STUD);
}
