//! Exports one generated region's terrain colliders, exactly as the legacy
//! (fully materialized, non-cell) genesis path builds them
//! (`Region::from_columns` → `seed_terrain_genesis`), for the rapier 0.36
//! clone/hash spike in `crates/physics-next/examples/clone_cost.rs`.
//!
//! ```sh
//! cargo run --release -p worldgen --example export_region_colliders [-- <x> <z>]
//! ```
//!
//! (Its former `--hash-cost` mode timed the old nalgebra-era forks' std
//! `Hash` as the baseline for `StateHash`; it went with those forks at the
//! switch. The numbers are recorded in the upgrade plan, Task 4.4 "Step 4 cost
//! check".)
//!
//! Writes `target/physics-next/region_<x>_<z>.bin` (bincode [`RegionExport`];
//! `physics-next` mirrors the types, so keep the two in sync). One entry per
//! terrain body — including bodies with no collider, since the game inserts a
//! fixed body for every chunk — in body-set order, each with its colliders in
//! attach order: a full-chunk cuboid collapse, or `Voxels` grids at block
//! (16), half-block (8) and voxel (1) scale.
//!
//! Lives in `worldgen` because `game` can't depend on it (worldgen depends
//! on game). Build it with the default region size and cells off: that is
//! the production collider set the spike measures.
use game::RegionCoords;
use serde::Serialize;
use worldgen::material::Materials;

#[derive(Serialize)]
pub struct RegionExport {
    pub region: [i32; 2],
    pub region_chunks: u32,
    /// `game::REGION_SIZE` (world units).
    pub region_size: f32,
    /// `game::UNITS_PER_METER`.
    pub units_per_meter: f32,
    /// The player body: `(PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS)`.
    pub player_capsule: [f32; 2],
    /// `WALK_SPEED` in world units per second.
    pub walk_speed: f32,
    pub chunks: Vec<ExportedChunk>,
}

#[derive(Serialize)]
pub struct ExportedChunk {
    /// The chunk body's translation (world units, region-local).
    pub origin: [f32; 3],
    pub colliders: Vec<ExportedCollider>,
}

#[derive(Serialize)]
pub enum ExportedCollider {
    /// Solid cells of one `Voxels` grid, in the shape's iteration order.
    Voxels { voxel_size: [f32; 3], keys: Vec<[i32; 3]> },
    /// The full-chunk collapse; `translation` is body-local.
    Cuboid { half_extents: [f32; 3], translation: [f32; 3] },
}

fn main() {
    let mut args = std::env::args().skip(1).map(|a| a.parse::<i32>().expect("region coords are integers"));
    let (x, z) = (args.next().unwrap_or(0), args.next().unwrap_or(0));
    let mats = Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
        biome: worldgen::material::BiomeBlocks::NONE,
    };
    let rc = RegionCoords::new(x, z);
    let region = game::Region::from_columns(rc, worldgen::generate_region(rc, worldgen::WORLD_SEED, mats));
    let physics = &region.data().data.physics;

    let (mut voxel_grids, mut cells, mut cuboids) = ([0usize; 3], [0usize; 3], 0usize);
    let mut chunks = Vec::new();
    for (_, body) in physics.bodies.iter() {
        assert!(body.is_fixed(), "a fresh region holds only terrain bodies");
        let t = body.translation();
        let mut colliders = Vec::new();
        for &h in body.colliders() {
            let c = physics.colliders.get(h).unwrap();
            let local = c.position_wrt_parent().copied().unwrap_or_default();
            if let Some(v) = c.shape().as_voxels() {
                let s = v.voxel_size();
                let keys: Vec<[i32; 3]> = v
                    .voxels()
                    .filter(|d| !d.state.is_empty())
                    .map(|d| [d.grid_coords.x, d.grid_coords.y, d.grid_coords.z])
                    .collect();
                let scale = match s.x as u32 {
                    16 => 0,
                    8 => 1,
                    1 => 2,
                    other => panic!("unexpected voxel size {other}"),
                };
                voxel_grids[scale] += 1;
                cells[scale] += keys.len();
                colliders.push(ExportedCollider::Voxels { voxel_size: s.to_array(), keys });
            } else if let Some(cub) = c.shape().as_cuboid() {
                let he = cub.half_extents;
                let tr = local.translation;
                cuboids += 1;
                colliders.push(ExportedCollider::Cuboid {
                    half_extents: he.to_array(),
                    translation: tr.to_array(),
                });
            } else {
                panic!("unexpected terrain collider shape {:?}", c.shape().shape_type());
            }
        }
        chunks.push(ExportedChunk { origin: t.to_array(), colliders });
    }

    let export = RegionExport {
        region: [x, z],
        region_chunks: game::REGION_CHUNKS as u32,
        region_size: game::REGION_SIZE,
        units_per_meter: game::movement::UNITS_PER_METER,
        player_capsule: [game::movement::PLAYER_CAPSULE_HALF_HEIGHT, game::movement::PLAYER_CAPSULE_RADIUS],
        walk_speed: game::movement::WALK_SPEED * game::movement::UNITS_PER_METER,
        chunks,
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/physics-next");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("region_{x}_{z}.bin"));
    let bytes = bincode::serialize(&export).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    println!("region ({x}, {z}), REGION_BLOCKS {}", game::REGION_BLOCKS);
    println!("bodies (one per chunk): {}", export.chunks.len());
    println!("cuboid collapses:       {cuboids}");
    for (i, size) in [16, 8, 1].iter().enumerate() {
        println!("voxels grids @ {size:>2}:     {} ({} solid cells)", voxel_grids[i], cells[i]);
    }
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
}
