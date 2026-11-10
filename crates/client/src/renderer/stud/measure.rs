//! Offline measurement of what the stud renderer is asked to draw (no GPU, no
//! ECS): real procedural stud regions through the real chunk mesh job, and the
//! far tiles through the real far mesher. Ignored tests that print a table —
//! run one with
//! `just test-one client "--lib renderer::stud::measure:: -- --ignored --nocapture"`.
//!
//! Test builds use 64 m regions; a 256 m play region is the same world area as
//! 4 x 4 of them (generation is a function of world coordinates), so the near
//! measurement meshes [`AREA_REGIONS`]² test regions and reports them as ONE
//! play region.

use std::collections::BTreeMap;
use std::sync::Arc;

use bevy::prelude::{Entity, Vec3};
use game::{RegionCoords, StudChunk, StudChunkCoords, StudShape, WorldChunkPos, MAT_LEAVES, MAT_WOOD};

use super::mesh::{mesh_chunk, StudMeshData, StudMeshJob, StudMeshOutput, STUD_EDGE_ALPHA};
use super::world::{world_chunk, world_chunk_origin, StudWorld, StudWorldMsg};

/// Test regions per edge of the measured area: one 256 m play region.
const AREA_REGIONS: i32 = 256 / game::REGION_BLOCKS as i32;
/// Regions the launch configuration loads (near radius 1).
const LAUNCH_REGIONS: usize = 9;
/// Bytes of one vertex of a stud mesh on the GPU: position + normal + colour.
const VERTEX_BYTES: usize = 12 + 12 + 16;
/// Gate: body triangles of one 256 m region. Measured 609 914 (the first launch
/// drew 8 822 468); the headroom is for worldgen tuning, not for a new mesher.
const MAX_BODY_TRIS_PER_REGION: usize = 900_000;
/// Gate: detail triangles around one viewer. Measured 749 640 in 28 chunks.
const MAX_DETAIL_TRIS: usize = 1_200_000;

#[derive(Default, Clone, Copy)]
struct Counts {
    chunks: usize,
    bricks: usize,
    fillers: usize,
    tree_bricks: usize,
    body_meshes: usize,
    glass_meshes: usize,
    body_verts: usize,
    body_tris: usize,
    /// Body triangles in the wood / leaf colours (trees), by face direction up /
    /// down / sideways; the rest are ground.
    tree_tris: usize,
    up_tris: usize,
    down_tris: usize,
    glass_tris: usize,
    /// As if every chunk were meshed with detail (the first launch's load).
    detail_verts_everywhere: usize,
    seam_tris_everywhere: usize,
    stud_tris_everywhere: usize,
    /// The chunks in the detail set of a viewer in the middle of the area.
    detail_meshes: usize,
    detail_verts: usize,
    seam_tris: usize,
    stud_tris: usize,
}

fn tris(d: &StudMeshData) -> usize {
    d.indices.len() / 3
}

/// Triangles of a detail mesh that are seam-line quads (the marker alpha, four
/// vertices and two triangles each); the rest are studs.
fn seam_tris(d: &StudMeshData) -> usize {
    d.colors.iter().filter(|c| c[3] == STUD_EDGE_ALPHA).count() / 2
}

/// The chunks a fresh region materialises, as `GameData::seed_stud_ground_genesis` does.
fn genesis_chunks(seed: &game::StudRegionSeed) -> Vec<(StudChunkCoords, StudChunk)> {
    let mut overrides = seed.feature_chunks();
    let mut chunks = Vec::new();
    for (col, g) in &seed.ground {
        for y in g.surface_layers() {
            let c = StudChunkCoords { x: col.x, y, z: col.z };
            if let Some(chunk) = overrides.remove(&c).or_else(|| g.pristine_chunk(y)) {
                chunks.push((c, chunk));
            }
        }
    }
    chunks.extend(overrides);
    chunks
}

/// Every chunk of the measured area, indexed as the client indexes them.
fn load_area() -> (StudWorld, Vec<(WorldChunkPos, StudChunk)>) {
    let mut world = StudWorld::default();
    let mut all = Vec::new();
    let mut n = 1u32;
    for rx in 0..AREA_REGIONS {
        for rz in 0..AREA_REGIONS {
            let rc = RegionCoords::new(rx, rz);
            let seed = worldgen::stud_gen::generate_stud_region(rc, worldgen::WORLD_SEED);
            let ground: BTreeMap<_, _> = seed.ground.iter().cloned().collect();
            world.apply(&StudWorldMsg::Region {
                region: rc,
                terrain: game::TerrainKind::Stud,
                ground: Arc::new(ground),
                halo: Vec::new(),
                halo_ground: Vec::new(),
            });
            for (c, chunk) in genesis_chunks(&seed) {
                let key = world_chunk(rc, c);
                world.set_chunk(Entity::from_raw_u32(n).unwrap(), key, &chunk);
                n += 1;
                all.push((key, chunk));
            }
        }
    }
    (world, all)
}

fn measure_chunk(chunk: &StudChunk, near: bool, out: &StudMeshOutput, c: &mut Counts) {
    c.chunks += 1;
    c.bricks += chunk.len();
    c.fillers += chunk.bricks().filter(|b| matches!(b.shape, StudShape::Filler { .. })).count();
    c.tree_bricks += chunk.bricks().filter(|b| b.material == MAT_WOOD || b.material == MAT_LEAVES).count();
    c.body_meshes += usize::from(!out.body.is_empty());
    c.glass_meshes += usize::from(!out.glass.is_empty());
    c.body_verts += out.body.positions.len();
    c.body_tris += tris(&out.body);
    let assets = super::mesh::stud_assets();
    let tree_colors = [MAT_WOOD, MAT_LEAVES].map(|m| assets.brick_colors(game::stud_material_color(m)).0);
    for t in out.body.indices.chunks(3) {
        let i = t[0] as usize;
        c.tree_tris += usize::from(tree_colors.contains(&out.body.colors[i]));
        c.up_tris += usize::from(out.body.normals[i][1] > 0.5);
        c.down_tris += usize::from(out.body.normals[i][1] < -0.5);
    }
    c.glass_tris += tris(&out.glass);
    let (seams, studs) = (seam_tris(&out.top), tris(&out.top) - seam_tris(&out.top));
    c.detail_verts_everywhere += out.top.positions.len();
    c.seam_tris_everywhere += seams;
    c.stud_tris_everywhere += studs;
    if near && !out.top.is_empty() {
        c.detail_meshes += 1;
        c.detail_verts += out.top.positions.len();
        c.seam_tris += seams;
        c.stud_tris += studs;
    }
}

fn mb(verts: usize, tris: usize) -> f64 {
    (verts * VERTEX_BYTES + tris * 12) as f64 / (1024.0 * 1024.0)
}

fn report(label: &str, c: &Counts) {
    let per = |n: usize| n as f64 / c.chunks.max(1) as f64;
    println!("== {label} ==");
    println!(
        "chunks {}  bricks {} ({} fillers, {} tree bricks, {:.0}/chunk)",
        c.chunks,
        c.bricks,
        c.fillers,
        c.tree_bricks,
        per(c.bricks)
    );
    println!("mesh entities: body {}  detail {} (near the viewer)  glass {}", c.body_meshes, c.detail_meshes, c.glass_meshes);
    println!(
        "body (merged faces): {} verts, {} tris ({:.0} tris/chunk, {:.2} verts/tri, {:.1} MB)",
        c.body_verts,
        c.body_tris,
        per(c.body_tris),
        c.body_verts as f64 / c.body_tris.max(1) as f64,
        mb(c.body_verts, c.body_tris)
    );
    println!(
        "  of the body: trees {} / ground {};  facing up {} / down {} / sideways {}",
        c.tree_tris,
        c.body_tris - c.tree_tris,
        c.up_tris,
        c.down_tris,
        c.body_tris - c.up_tris - c.down_tris
    );
    println!(
        "detail within {} m of a viewer mid-area: {} chunks, {} verts, {} seam-line tris + {} stud tris ({:.1} MB)",
        super::systems::STUD_TOPS_RADIUS_M,
        c.detail_meshes,
        c.detail_verts,
        c.seam_tris,
        c.stud_tris,
        mb(c.detail_verts, c.seam_tris + c.stud_tris)
    );
    println!(
        "detail if meshed everywhere (not done): {} verts, {} seam-line tris + {} stud tris ({:.1} MB)",
        c.detail_verts_everywhere,
        c.seam_tris_everywhere,
        c.stud_tris_everywhere,
        mb(c.detail_verts_everywhere, c.seam_tris_everywhere + c.stud_tris_everywhere)
    );
    println!("glass: {} tris;  bytes per vertex {VERTEX_BYTES} (+ 4 per index)", c.glass_tris);
    println!(
        "launch ({LAUNCH_REGIONS} regions, one viewer): chunks {}  body tris {} ({:.0} MB)  + detail tris {} ({:.0} MB)",
        c.chunks * LAUNCH_REGIONS,
        c.body_tris * LAUNCH_REGIONS,
        mb(c.body_verts, c.body_tris) * LAUNCH_REGIONS as f64,
        c.seam_tris + c.stud_tris,
        mb(c.detail_verts, c.seam_tris + c.stud_tris)
    );
}

/// One 256 m play region of the procedural stud world, with its trees, through
/// the real mesh job. First launch, same area, everything from baked triangles:
/// 437 chunks, 325 076 bricks; body 21.3 M verts / 8.82 M tris (ground faces
/// 0.54 M, tree faces 1.25 M, undersides 1.91 M, seam-line quads 5.13 M; 915 MB);
/// stud tops 23.7 M verts / 7.90 M tris (995 MB, uploaded for every chunk).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn measure_a_procedural_stud_region() {
    let (mut world, all) = load_area();
    // A viewer on the ground in the middle of the area.
    let mid = AREA_REGIONS as f32 * game::REGION_SIZE as f32 / 2.0;
    let ground_y = {
        let centre_key = |k: &WorldChunkPos| {
            let o = world_chunk_origin(*k);
            ((o.x - mid).abs() + (o.z - mid).abs()) as i64
        };
        let (key, _) = all.iter().min_by_key(|(k, _)| (centre_key(k), -k.y)).expect("a chunk");
        world_chunk_origin(*key).y
    };
    let radius = super::systems::STUD_TOPS_RADIUS_M * game::movement::UNITS_PER_METER;
    world.set_detail_centre(Some((Vec3::new(mid, ground_y, mid), radius)));
    let mut c = Counts::default();
    for (key, chunk) in &all {
        let StudMeshJob::Chunk { chunk, neighbours, detail } = world.job(*key, chunk) else { unreachable!() };
        // Meshed with detail to count what it would be everywhere; the body is the same.
        let out = mesh_chunk(&chunk, &neighbours, true);
        measure_chunk(&chunk, detail, &out, &mut c);
    }
    report(&format!("one 256 m region = {0} x {0} regions of {1} m", AREA_REGIONS, game::REGION_BLOCKS), &c);
    // The gates the fix is held to (the first launch: 8.82 M body tris a region).
    assert!(c.body_tris < MAX_BODY_TRIS_PER_REGION, "body tris {}", c.body_tris);
    assert!(c.seam_tris + c.stud_tris < MAX_DETAIL_TRIS, "detail tris {}", c.seam_tris + c.stud_tris);
}

/// The far field of the launch configuration: the 256 m stud ring table's keys
/// (counted per ring around a 3 x 3 near window) times the real far mesh of a
/// tile with that ring's cell count. A tile's lattice (`side`) is the same at
/// 64 m and 256 m regions ring for ring — both tables are cut to one pixel
/// budget — so the test build's tiles stand in for the play build's.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn measure_the_far_field() {
    /// The 256 m stud ring table (`game::stud_ring_table` at 256 m regions) as
    /// `(bound, factor, tile edge in regions, cells per tile edge)`.
    const PLAY_RINGS: [(i32, u32, i32, u16); 5] = [(2, 32, 1, 128), (3, 64, 1, 64), (6, 128, 1, 32), (12, 256, 1, 16), (60, 512, 4, 32)];
    let table = game::stud_ring_table(game::lod::LOD_RING_TABLE);
    println!("== far field: stud ring table here {table:?} ==");
    let (mut inner, mut keys_total, mut verts_total, mut tris_total, mut water_total) = (1i32, 0usize, 0usize, 0usize, 0usize);
    for (bound, factor, span, side) in PLAY_RINGS {
        // Regions of the ring (Chebyshev distance inner+1 ..= bound), as tiles.
        let regions = ((2 * bound + 1).pow(2) - (2 * inner + 1).pow(2)) as usize;
        let keys = regions / (span * span) as usize;
        inner = bound;
        // A tile of this build with the same lattice, on real terrain.
        let here = table
            .iter()
            .map(|&(_, f)| game::lod::LodKey { factor: f, coords: [3, 2] })
            .find(|k| game::StudLodTile::side_for(k) == side);
        let (verts, tris_n, water) = match here {
            Some(key) => {
                let tile = worldgen::stud_gen::stud_lod_tile(&key, worldgen::WORLD_SEED);
                let out = super::far::mesh_tile(tile.side, &tile.decode().unwrap(), key.factor as f32);
                (out.terrain.positions.len(), tris(&out.terrain), out.water.indices.len() / 3)
            }
            // No such lattice in this build's table: the sheet's own arithmetic.
            None => {
                let s = usize::from(side);
                ((s + 1) * (s + 1) + 8 * (s + 1), 2 * s * s + 8 * s, 0)
            }
        };
        println!("ring <= {bound:>2} factor {factor:>3}: {keys:>4} keys x ({verts} verts, {tris_n} tris, {water} water tris) per tile of {side} cells");
        keys_total += keys;
        verts_total += keys * verts;
        tris_total += keys * tris_n;
        water_total += keys * water;
    }
    println!(
        "far total (square rings; the live table clips to its view circle): {keys_total} keys, {verts_total} verts, {tris_total} terrain tris, {water_total} water tris, {:.1} MB",
        (verts_total * VERTEX_BYTES + tris_total * 12) as f64 / (1024.0 * 1024.0)
    );
}
