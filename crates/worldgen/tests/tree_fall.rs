//! Felling GENERATED trees, end to end: a tree from `rasterize(skeleton(..))`
//! written into a sim region — real worldgen terrain (generated without trees
//! of its own) or a flat floor — chopped through the real `Chop` order +
//! `DevCompleteAll`, and the falling voxel body stepped by the region tick
//! until it is settled. What is checked is what a player sees: every voxel
//! of the body (wood AND leaves) stays out of the ground, the body comes to
//! rest on it, and it falls like a tree — pivoting about its base onto its
//! side, with the butt staying on the ground.
//!
//! Debugging: `TREE_FALL_SPECIES=<name>`, `TREE_FALL_SEED=<n>` and
//! `TREE_FALL_AGE=<n>` narrow the sweep; `TREE_FALL_TRACE=1` prints every
//! tick (tilt, butt, centre of mass, depth, velocities) with `--nocapture`.

use std::collections::BTreeMap;

use game::math::Quat;
use game::rapier3d::math::Vector;
use game::voxel_body::{collision_model, model_mass_properties, SETTLE_TIMEOUT_TICKS};
use game::{
    Block, BlockId, BlockRegistry, Chunk, ChunkCoords, Column, ColumnCoords, DesignationKind, EntityKey, GameEventKind,
    OpId, Order, Region, RegionCoords, Rollback, TileBox, TilePos, BLOCK_VOXELS, CHUNK_BLOCKS, DEFAULT_PRIORITY,
    REGION_BLOCKS, REGION_CHUNKS, TICK_RATE,
};
use worldgen::material::{Materials, TreeBlocks};
use worldgen::pipeline::SEA_TOP_BLOCK;
use worldgen::trees::{rasterize, skeleton, Species, TreeBlock, TreePart, MAX_AGE, SPECIES};

const MANIFEST: &str = include_str!("../../../assets/blocks/blocks.ron");
const B: f32 = BLOCK_VOXELS as f32;
const CB: i32 = CHUNK_BLOCKS as i32;
const RB: i32 = REGION_BLOCKS as i32;
const TREE: u32 = 7;

fn registry() -> BlockRegistry {
    let reg = BlockRegistry::from_ron(MANIFEST).unwrap();
    game::install_block_props(reg.props());
    reg
}

fn default_dials() -> bool {
    std::env::var_os(worldgen::forged::WORLD_FILE_ENV).is_none_or(|v| v.is_empty())
}

fn forest() -> RegionCoords {
    RegionCoords::from_world(-1536.0, 2560.0)
}

/// The ground a tree is planted on: its chunks, per-column ground recipes
/// (real terrain only) and the top of the ground per block column (units).
struct Ground {
    rc: RegionCoords,
    chunks: BTreeMap<ChunkCoords, Chunk>,
    recipes: Vec<(ColumnCoords, game::ColumnGround)>,
    /// Surface block y per block column (the block the tree may stand on).
    surface: BTreeMap<[i32; 2], i32>,
    /// Top of the ground, units: the surface block's top (a half block's
    /// cuboid top).
    top: BTreeMap<[i32; 2], f32>,
}

/// The forested region `trees_in_terrain.rs` uses, generated WITHOUT its own
/// trees: real ground (soil, rock, a sloping surface band, implied ground
/// below it), nothing else standing on it.
fn bare_forest() -> Ground {
    let reg = registry();
    let mats = Materials::from_names(|n| reg.id_of(n).unwrap());
    let rc = forest();
    let cols: Vec<(ColumnCoords, Column)> =
        worldgen::generate_region(rc, worldgen::WORLD_SEED, Materials { trees: TreeBlocks::NONE, ..mats });
    let mut chunks = BTreeMap::new();
    let mut surface = BTreeMap::new();
    for (cc, col) in &cols {
        for (sub_y, chunk) in col.materialized_sub_chunks() {
            chunks.insert(ChunkCoords::new(cc.x, sub_y, cc.z), chunk);
        }
        let g = col.ground().unwrap();
        for lx in 0..CB {
            for lz in 0..CB {
                let s = i32::from(g.surface[lx as usize * CHUNK_BLOCKS + lz as usize]);
                surface.insert([cc.x as i32 * CB + lx, cc.z as i32 * CB + lz], s);
            }
        }
    }
    let recipes = cols.iter().map(|(c, col)| (*c, col.ground().unwrap().clone())).collect();
    let mut g = Ground { rc, chunks, recipes, surface, top: BTreeMap::new() };
    g.top = g.surface.iter().map(|(&c, &s)| (c, g.block_top([c[0], s, c[1]]))).collect();
    g
}

/// A stone floor `depth` blocks deep over the whole region.
fn flat_floor(depth: i32) -> Ground {
    registry();
    let mut chunks = BTreeMap::new();
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            chunks.insert(ChunkCoords::new(x, 0, z), Chunk::flat_floor_with(depth as u32 * BLOCK_VOXELS as u32, BlockId(2)));
        }
    }
    let mut surface = BTreeMap::new();
    let mut top = BTreeMap::new();
    for x in 0..RB {
        for z in 0..RB {
            surface.insert([x, z], depth - 1);
            top.insert([x, z], depth as f32 * B);
        }
    }
    Ground { rc: RegionCoords::new(0, 0), chunks, recipes: Vec::new(), surface, top }
}

impl Ground {
    /// Top (units) of the block at `p`: its chisel's highest cuboid, or the
    /// whole block.
    fn block_top(&self, p: TilePos) -> f32 {
        let (coords, (x, y, z)) = game::edit::locate(p).unwrap();
        let chisel = self.chunks.get(&coords).and_then(|c| c.chisel.get(&game::BlockIndex::from_xyz(x, y, z)));
        match chisel {
            Some(c) => p[1] as f32 * B + c.cuboids().map(|(_, max, _)| max[1]).max().unwrap_or(0) as f32,
            None => (p[1] + 1) as f32 * B,
        }
    }

    /// The dry block column nearest the region centre whose surface within
    /// `r` blocks varies by at most one block.
    fn flat_spot(&self, r: i32) -> [i32; 2] {
        let mid = RB / 2;
        let mut best: Option<(i32, [i32; 2])> = None;
        for x in r..RB - r {
            for z in r..RB - r {
                let s = self.surface[&[x, z]];
                if s <= SEA_TOP_BLOCK && !self.recipes.is_empty() {
                    continue;
                }
                let (mut lo, mut hi) = (s, s);
                for dx in -r..=r {
                    for dz in -r..=r {
                        let v = self.surface[&[x + dx, z + dz]];
                        lo = lo.min(v);
                        hi = hi.max(v);
                    }
                }
                let d = (x - mid).abs() + (z - mid).abs();
                if hi - lo <= 1 && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, [x, z]));
                }
            }
        }
        best.expect("a flat dry spot").1
    }

    /// Lowest ground top (units) of the block columns within `slack` units
    /// of the world point's column: a point resting against the side of a
    /// step (pushed `slack` into it sideways) is measured against the lower
    /// column, not as sunk the whole step deep.
    fn top_at(&self, x: f32, z: f32, slack: f32) -> f32 {
        let col = |v: f32| ((v / B).floor() as i32).clamp(0, RB - 1);
        let mut t = f32::INFINITY;
        for cx in col(x - slack)..=col(x + slack) {
            for cz in col(z - slack)..=col(z + slack) {
                t = t.min(self.top[&[cx, cz]]);
            }
        }
        t
    }
}

struct Scene {
    region: Region,
    ground: Ground,
    /// Tree blocks by region-local position.
    tree: BTreeMap<TilePos, TreeBlock>,
}

/// `sp`'s tree (`seed`, `age`) standing on `ground` at its flattest spot
/// near the centre, as region genesis.
fn scene(mut ground: Ground, sp: &Species, seed: u64, age: u16) -> Scene {
    let reg = registry();
    let [x, z] = ground.flat_spot(3);
    let base = [x, ground.surface[&[x, z]] + 1, z];
    let id = |part: &str| reg.id_of(&format!("{}_{part}", sp.name)).unwrap();
    let mut tree = BTreeMap::new();
    for b in rasterize(&skeleton(sp, seed, age)) {
        let p = [base[0] + b.offset[0], base[1] + b.offset[1], base[2] + b.offset[2]];
        let Some((coords, (bx, by, bz))) = game::edit::locate(p) else { continue };
        if p[1] <= ground.surface[&[p[0], p[2]]] {
            continue; // trees only fill air (as worldgen places them)
        }
        let block_id = match b.part {
            TreePart::Log => id("log"),
            TreePart::Branch => id("branch"),
            TreePart::Leaves => id("leaves"),
        };
        let chunk = ground.chunks.entry(coords).or_default();
        chunk.set_block(bx, by, bz, Block::with_tree(block_id, TREE, b.limb));
        if let Some(mask) = &b.chisel {
            let c = game::ChiselData::from_fn(vec![block_id], |vx, vy, vz| {
                mask[vx + BLOCK_VOXELS * (vy + BLOCK_VOXELS * vz)].then_some(0)
            });
            chunk.chisel.insert(game::BlockIndex::from_xyz(bx, by, bz), c);
        }
        tree.insert(p, b);
    }
    let (send, recv) = crossbeam::channel::unbounded();
    std::mem::forget(recv);
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), ground.rc, None);
    let chunks: Vec<_> = ground.chunks.iter().map(|(c, k)| (*c, k.clone())).collect();
    let recipes = ground.recipes.clone();
    region.with_data(|r| {
        r.seed_terrain_genesis(chunks);
        if !recipes.is_empty() {
            r.seed_ground_genesis(recipes);
        }
    });
    Scene { region, ground, tree }
}

impl Scene {
    fn base_y(&self) -> i32 {
        self.tree.iter().filter(|(_, b)| b.part != TreePart::Leaves).map(|(p, _)| p[1]).min().unwrap()
    }
    fn top_log(&self) -> i32 {
        self.tree.iter().filter(|(_, b)| b.part == TreePart::Log).map(|(p, _)| p[1]).max().unwrap()
    }
    /// The trunk log (limb 0) at level `y` nearest the trunk's base.
    fn trunk_at(&self, y: i32) -> Option<TilePos> {
        let axis = self.tree.iter().find(|(p, b)| p[1] == self.base_y() && b.part == TreePart::Log).map(|(p, _)| *p)?;
        self.tree
            .iter()
            .filter(|(p, b)| p[1] == y && b.part == TreePart::Log && b.limb == 0)
            .map(|(p, _)| *p)
            .min_by_key(|p| ((p[0] - axis[0]).pow(2) + (p[2] - axis[2]).pow(2), *p))
    }

    /// Chops `cut` through the real orders; the felled body, if any.
    fn chop(&mut self, cut: TilePos) -> Option<EntityKey> {
        let op = OpId { client: 0, seq: 1 };
        let designate =
            Order::Designate { op, kind: DesignationKind::Chop, priority: DEFAULT_PRIORITY, area: TileBox::new(cut, cut) };
        for order in [designate, Order::DevCompleteAll] {
            self.region.handle_event(GameEventKind::Order(0, order)).unwrap();
            self.region.forget_last_event();
        }
        let r = self.region.data();
        r.ecs.voxel_body.iter().find_map(|(k, v)| v.as_ref().map(|_| k))
    }
}

/// How far (units) a point of a body may sit in the ground: one voxel for
/// whole blocks and leaves (their colliders are the blocks), and a further
/// half block for chiseled wood, whose collider is on the half-block grid —
/// an octant less than a quarter full has none, so its voxels can rest up to
/// a half block deep (`block_collider_cells`, the terrain's own rule).
const WHOLE_TOLERANCE: f32 = 1.0;
const CHISELED_TOLERANCE: f32 = 1.0 + game::HALF_BLOCK_VOXELS as f32;

/// A colliding point may sit this much deeper than its tolerance for the
/// tick of an impact (units: half a block) before the solver pushes it back
/// out; at rest it must be within its tolerance. Before the fix the crown of
/// every young tree sank 2–4.5 blocks and stayed there.
const IMPACT_SLACK: f32 = 8.0;

/// One corner of a voxel box of a body's model.
struct Point {
    /// Relative to the centre of mass (the body frame).
    at: Vector,
    /// How deep it may sink ([`WHOLE_TOLERANCE`], [`CHISELED_TOLERANCE`]).
    tol: f32,
    /// Whether its block collides (`collision_model`: a toppling tree's
    /// skirt does not).
    collides: bool,
    /// Whether it is trunk wood on the trunk's lowest layer (the butt).
    butt: bool,
}

fn body_points(model: &game::VoxelModel, toppling: bool) -> Vec<Point> {
    let com = model_mass_properties(model).local_com;
    let colliding = collision_model(model, toppling);
    let trunk = |b: &Block| b.tree_id() != 0 && b.limb() == 0 && game::block_props().is_solid(b.id);
    let butt_y = model.blocks.iter().filter(|(_, (b, _))| trunk(b)).map(|(o, _)| o[1]).min();
    let mut out = Vec::new();
    for (o, (block, chisel)) in &model.blocks {
        let base = Vector::new(o[0] as f32, o[1] as f32, o[2] as f32) * B;
        let (boxes, tol): (Vec<([f32; 3], [f32; 3])>, f32) = match chisel {
            None if block.id != BlockId::AIR => (vec![([0.0; 3], [B; 3])], WHOLE_TOLERANCE),
            None => (Vec::new(), WHOLE_TOLERANCE),
            // Leaves collide as whole blocks, chisel or not.
            Some(_) if game::is_foliage(block.id) => (vec![([0.0; 3], [B; 3])], WHOLE_TOLERANCE),
            Some(c) => (
                c.cuboids()
                    .filter(|(_, _, m)| *m != BlockId::AIR)
                    .map(|(lo, hi, _)| (lo.map(|v| v as f32), hi.map(|v| v as f32)))
                    .collect(),
                CHISELED_TOLERANCE,
            ),
        };
        let collides = colliding.blocks.contains_key(o);
        let butt = trunk(block) && Some(o[1]) == butt_y;
        for (lo, hi) in boxes {
            for i in 0..8 {
                let pick = |a: usize| if (i >> a) & 1 == 0 { lo[a] } else { hi[a] };
                out.push(Point { at: base + Vector::new(pick(0), pick(1), pick(2)) - com, tol, collides, butt });
            }
        }
    }
    out
}

/// One tick of a fall, sampled before it is stepped.
struct Sample {
    /// How far the deepest colliding point sits below the ground at its
    /// column past its tolerance (units; ≤ 0 is fine).
    depth: f32,
    /// The same for the points that do not collide (a toppling skirt).
    clip: f32,
    /// The lowest butt point's y.
    butt: f32,
    rotation: Quat,
}

/// One fall.
struct Fall {
    ticks: Vec<Sample>,
    /// Ticks until it was settled (`u64::MAX`: still there at the timeout).
    settled_after: u64,
    /// Linear and angular speed on the last tick seen.
    last_speed: (f32, f32),
}

impl Fall {
    fn deepest(&self) -> f32 {
        self.ticks.iter().map(|t| t.depth).fold(f32::NEG_INFINITY, f32::max)
    }
    fn resting_depth(&self) -> f32 {
        self.ticks.last().unwrap().depth
    }
    fn clip(&self) -> f32 {
        self.ticks.iter().map(|t| t.clip).fold(f32::NEG_INFINITY, f32::max)
    }
    /// Largest rotation from the starting orientation, degrees.
    fn max_turn_deg(&self) -> f32 {
        let q0 = self.ticks[0].rotation;
        self.ticks.iter().map(|t| (q0.inverse() * t.rotation).to_axis_angle().1.to_degrees()).fold(0.0, f32::max)
    }
    /// How far the butt rose above where it started, units.
    fn butt_rise(&self) -> f32 {
        let y0 = self.ticks[0].butt;
        self.ticks.iter().map(|t| t.butt - y0).fold(f32::NEG_INFINITY, f32::max)
    }
    /// Fastest the butt ever rose, units/s.
    fn butt_kick(&self) -> f32 {
        let dt = TICK_RATE as f32 / 1000.0;
        self.ticks.windows(2).map(|w| (w[1].butt - w[0].butt) / dt).fold(0.0, f32::max)
    }
    /// Tilt of the model's up axis from vertical at the end, degrees.
    fn final_tilt_deg(&self) -> f32 {
        tilt(self.ticks.last().unwrap().rotation)
    }
    /// Seconds until it was down: tilted to within 10 % of where it ended.
    fn down_after(&self) -> Option<f32> {
        let end = self.final_tilt_deg();
        self.ticks.iter().position(|t| tilt(t.rotation) >= 0.9 * end).map(|t| t as f32 * TICK_RATE as f32 / 1000.0)
    }
}

fn tilt(q: Quat) -> f32 {
    (q * Vector::Y).y.clamp(-1.0, 1.0).acos().to_degrees()
}

fn run_fall(scene: &mut Scene, e: EntityKey) -> Fall {
    let points = body_points(&scene.region.data().ecs.voxel_body.get(e).model, true);
    let mut fall = Fall { ticks: Vec::new(), settled_after: u64::MAX, last_speed: (0.0, 0.0) };
    for t in 0..SETTLE_TIMEOUT_TICKS + 5 {
        let r = scene.region.data();
        if !r.ecs.entities.contains_key(e) {
            fall.settled_after = t;
            break;
        }
        let body = r.physics.bodies.get(*r.ecs.rigidbody.get(e)).unwrap();
        let pose = *body.position();
        let mut sample = Sample {
            depth: f32::NEG_INFINITY,
            clip: f32::NEG_INFINITY,
            butt: f32::INFINITY,
            rotation: pose.rotation,
        };
        for p in &points {
            let w = pose.translation + pose.rotation * p.at;
            let d = scene.ground.top_at(w.x, w.z, p.tol + 0.5) - w.y - p.tol;
            if p.collides {
                sample.depth = sample.depth.max(d);
            } else {
                sample.clip = sample.clip.max(d);
            }
            if p.butt {
                sample.butt = sample.butt.min(w.y);
            }
        }
        if std::env::var_os("TREE_FALL_TRACE").is_some() {
            let (v, w) = (body.linvel(), body.angvel());
            println!(
                "  t {t:3} tilt {:5.1} butt {:7.1} com {:7.1} depth {:5.1} clip {:5.1} v [{:6.1} {:6.1} {:6.1}] w [{:5.2} {:5.2} {:5.2}]",
                tilt(pose.rotation), sample.butt, pose.translation.y, sample.depth, sample.clip, v.x, v.y, v.z, w.x, w.y, w.z
            );
        }
        fall.ticks.push(sample);
        fall.last_speed = (body.linvel().length(), body.angvel().length());
        scene.region.handle_event(GameEventKind::Tick).unwrap();
        scene.region.forget_last_event();
    }
    fall
}

/// Whether a fall broke the ground rule: a colliding point deeper than its
/// tolerance plus [`IMPACT_SLACK`] on any tick, or deeper than its tolerance
/// at rest, or never settling.
fn ground_failure(fall: &Fall) -> Option<String> {
    let mut bad = Vec::new();
    if fall.deepest() > IMPACT_SLACK {
        bad.push(format!("{:.1} u past its tolerance on impact", fall.deepest()));
    }
    if fall.resting_depth() > 0.0 {
        bad.push(format!("rests {:.1} u past its tolerance", fall.resting_depth()));
    }
    (!bad.is_empty()).then(|| bad.join(", "))
}

fn settled(fall: &Fall) -> bool {
    fall.settled_after < SETTLE_TIMEOUT_TICKS
}

/// Chops each young tree (all species, low ages) on real terrain at its
/// base and at mid-trunk: no colliding voxel of the body — wood or leaves —
/// goes into the ground at its column past its tolerance (one voxel; a half
/// block more for chiseled wood) except for half a block on the tick of
/// an impact, and nearly every body comes to rest (settled before the
/// timeout).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn young_trees_land_on_the_ground() {
    if !default_dials() {
        return;
    }
    let ground = bare_forest();
    let mut failures = Vec::new();
    let mut unsettled = Vec::new();
    let mut falls = 0;
    for sp in SPECIES.iter().filter(|s| std::env::var("TREE_FALL_SPECIES").map_or(true, |n| n == s.name)) {
        for seed in std::env::var("TREE_FALL_SEED").ok().map_or(0..3u64, |s| { let s: u64 = s.parse().unwrap(); s..s + 1 }) {
            for age in std::env::var("TREE_FALL_AGE").ok().map_or(vec![5u16, 15, 30], |a| vec![a.parse().unwrap()]) {
                let probe = scene(clone_ground(&ground), sp, seed, age);
                let (base, top) = (probe.base_y(), probe.top_log());
                drop(probe);
                for (what, y) in [("base", base), ("mid", (base + top) / 2)] {
                    let mut s = scene(clone_ground(&ground), sp, seed, age);
                    let Some(cut) = s.trunk_at(y) else { continue };
                    let label = format!("{} seed {seed} age {age} {what} cut y {y}", sp.name);
                    let Some(e) = s.chop(cut) else {
                        println!("{label}: no body");
                        continue;
                    };
                    let fall = run_fall(&mut s, e);
                    falls += 1;
                    println!(
                        "{label}: settled after {} ticks, depth {:.1} u (resting {:.1}), skirt clip {:.1} u, last speed {:.1} u/s {:.2} rad/s",
                        fall.settled_after,
                        fall.deepest(),
                        fall.resting_depth(),
                        fall.clip(),
                        fall.last_speed.0,
                        fall.last_speed.1
                    );
                    if let Some(why) = ground_failure(&fall) {
                        failures.push(format!("{label}: {why}"));
                    }
                    if !settled(&fall) {
                        unsettled.push(label);
                    }
                }
            }
        }
    }
    assert!(falls > 20, "only {falls} falls");
    assert!(failures.is_empty(), "{} of {falls} falls went into the ground:\n{}", failures.len(), failures.join("\n"));
    // A top cut off mid-trunk can land upright on its own stump and teeter
    // there until the settle timeout (it is above ground, checked above);
    // nearly every fall must come to rest.
    assert!(unsettled.len() * 10 <= falls, "{} of {falls} falls never came to rest: {unsettled:?}", unsettled.len());
}

fn clone_ground(g: &Ground) -> Ground {
    Ground { rc: g.rc, chunks: g.chunks.clone(), recipes: g.recipes.clone(), surface: g.surface.clone(), top: g.top.clone() }
}

/// Seconds a mature tree (14–19 blocks of trunk) takes from the chop until
/// it is down (`Fall::down_after`) at the world's gravity.
const MIN_LANDING_SECS: f32 = 0.3;
const MAX_LANDING_SECS: f32 = 3.0;
/// Most the butt may rise above where it started, blocks. The crown is
/// rigid: a pine lying on its side is a cone on its side, its trunk propped
/// up by its whorls (measured 2.3–3.3 blocks at the butt), and an oak that
/// lands on its crown rocks on it, so this bounds the crown's geometry more
/// than the topple (before the fix, spun about its centre of mass at 9.81
/// m/s², the same trees measured 0–3.5 blocks, with up to 119° of turn).
const MAX_BUTT_RISE_BLOCKS: f32 = 3.5;

/// A mature oak and pine chopped at the base on flat ground fall like trees:
/// they pivot about the butt onto their side or crown (no more than ~110° of
/// turn, the butt never lifting more than its crown props it), are down
/// within 0.3–3 s and
/// keep out of the floor as [`young_trees_land_on_the_ground`] does.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn mature_trees_topple_about_their_base() {
    let mut failures = Vec::new();
    for name in std::env::var("TREE_FALL_SPECIES").map_or(vec!["oak".to_string(), "pine".to_string()], |s| vec![s]) {
        let name = name.as_str();
        let sp = SPECIES.iter().find(|s| s.name == name).unwrap();
        for seed in std::env::var("TREE_FALL_SEED").ok().map_or(0..3u64, |s| { let s: u64 = s.parse().unwrap(); s..s + 1 }) {
            let mut s = scene(flat_floor(4), sp, seed, MAX_AGE);
            let height = (s.top_log() - s.base_y() + 1) as f32;
            let cut = s.trunk_at(s.base_y()).unwrap();
            let e = s.chop(cut).expect("a felled body");
            if std::env::var_os("TREE_FALL_TRACE").is_some() {
                let model = &s.region.data().ecs.voxel_body.get(e).model;
                let floor = model.blocks.keys().map(|o| o[1]).min().unwrap();
                for dy in 0..4 {
                    let layer: Vec<_> = model.blocks.iter().filter(|(o, _)| o[1] == floor + dy).collect();
                    let leaves = layer.iter().filter(|(_, (b, _))| game::is_foliage(b.id)).count();
                    let xs: Vec<i16> = layer.iter().map(|(o, _)| o[0]).collect();
                    println!("  layer +{dy}: {} blocks, {leaves} leaves, x {:?}..{:?}", layer.len(), xs.iter().min(), xs.iter().max());
                }
                println!("  cut {cut:?}, origin {:?}", s.region.data().ecs.voxel_body.get(e).origin);
            }
            let fall = run_fall(&mut s, e);
            let land = fall.down_after().unwrap_or(f32::INFINITY);
            let label = format!("{name} seed {seed} ({height} blocks of trunk)");
            println!(
                "{label}: turn {:.0}°, butt rise {:.1} u (fastest {:.0} u/s), final tilt {:.0}°, down after {land:.2} s, depth {:.1} u (resting {:.1}), skirt clip {:.1} u, settled after {}",
                fall.max_turn_deg(),
                fall.butt_rise(),
                fall.butt_kick(),
                fall.final_tilt_deg(),
                fall.deepest(),
                fall.resting_depth(),
                fall.clip(),
                fall.settled_after
            );
            let mut bad: Vec<String> = ground_failure(&fall).into_iter().collect();
            if fall.max_turn_deg() > 110.0 {
                bad.push(format!("turned {:.0}°", fall.max_turn_deg()));
            }
            if !settled(&fall) {
                bad.push("never came to rest".into());
            }
            if fall.butt_rise() > MAX_BUTT_RISE_BLOCKS * B {
                bad.push(format!("butt rose {:.1} u", fall.butt_rise()));
            }
            if !(MIN_LANDING_SECS..=MAX_LANDING_SECS).contains(&land) {
                bad.push(format!("down after {land:.2} s"));
            }
            // On its side, or propped on its crown (an oak's is as wide as
            // the tree is tall): well over, not standing, not upside down.
            if !(30.0..=120.0).contains(&fall.final_tilt_deg()) {
                bad.push(format!("ended tilted {:.0}°", fall.final_tilt_deg()));
            }
            if !bad.is_empty() {
                failures.push(format!("{label}: {}", bad.join(", ")));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
