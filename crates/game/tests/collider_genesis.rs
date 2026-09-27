//! Genesis collider build (Region::from_chunks -> seed_terrain_genesis):
//! fully-solid chunks collapse to a single cuboid; partial chunks get up to
//! two parry Voxels shapes (block-scale for whole solid blocks, voxel-scale
//! for chisel detail); the build is deterministic (bit-exact hash).
use game::parry::shape::ShapeType;
use game::{Chunk, ChunkCoords, Region, RegionId};
use game::StateHash;

fn hash_region(r: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    r.data().data.state_hash(&mut h);
    h.finalize()
}

// flat_floor(depth) fills the bottom `depth` voxel-layers. 32 -> every one of
// the 32^3 cells solid (full chunk); 8 -> a partial (surface-like) chunk.
fn full_chunk() -> Chunk {
    Chunk::flat_floor(32)
}
fn partial_chunk() -> Chunk {
    Chunk::flat_floor(8)
}

fn build() -> Region {
    Region::from_chunks(
        RegionId::new(0, 0),
        vec![
            (ChunkCoords::new(0, 0, 0), full_chunk()),
            (ChunkCoords::new(1, 0, 0), partial_chunk()),
        ],
    )
}

#[test]
fn full_chunk_becomes_cuboid_partial_stays_voxels() {
    let region = build();
    let shapes: Vec<ShapeType> = region
        .data()
        .physics
        .colliders
        .iter()
        .map(|(_, c)| c.shape().shape_type())
        .collect();
    assert!(
        shapes.contains(&ShapeType::Cuboid),
        "fully-solid chunk should collapse to a cuboid; got {shapes:?}"
    );
    assert!(
        shapes.contains(&ShapeType::Voxels),
        "partial chunk should keep a voxels shape; got {shapes:?}"
    );
    // Exactly one collider per non-empty chunk (both chunks are non-empty).
    assert_eq!(shapes.len(), 2, "one collider per chunk");
}

#[test]
fn genesis_is_deterministic() {
    assert_eq!(
        hash_region(&build()),
        hash_region(&build()),
        "independent genesis builds must hash bit-exactly identical"
    );
}

/// Voxel sizes of the `Voxels` colliders genesis builds for one chunk.
fn voxels_collider_sizes(chunk: Chunk) -> Vec<f32> {
    let region = Region::from_chunks(RegionId::new(0, 0), vec![(ChunkCoords::new(0, 0, 0), chunk)]);
    let mut sizes: Vec<f32> = region
        .data()
        .physics
        .colliders
        .iter()
        .filter_map(|(_, c)| c.shape().as_voxels().map(|v| v.voxel_size().x))
        .collect();
    sizes.sort_by(f32::total_cmp);
    sizes
}

#[test]
fn mixed_chunk_gets_block_and_voxel_scale_colliders() {
    // flat_floor(20): whole solid by=0 block layer + a 4-voxel chiseled slab
    // in by=1, which fills no half-block octant, so it needs voxel scale.
    assert_eq!(
        voxels_collider_sizes(Chunk::flat_floor(20)),
        vec![1.0, 16.0],
        "one chisel-scale and one block-scale voxels collider"
    );
}

#[test]
fn half_block_slab_gets_half_block_scale_collider() {
    // flat_floor(24): the chiseled slab is exactly the lower half of by=1,
    // i.e. whole half-block octants (worldgen's half blocks), so it needs no
    // voxel-scale collider.
    assert_eq!(
        voxels_collider_sizes(Chunk::flat_floor(24)),
        vec![8.0, 16.0],
        "one half-block-scale and one block-scale voxels collider"
    );
}

#[test]
fn fixed_terrain_bodies_skip_collider_mass_properties() {
    let region = build();
    let physics = &region.data().physics;
    let mut checked = 0;
    for (_, body) in physics.bodies.iter() {
        assert!(body.is_fixed(), "genesis bodies are fixed terrain");
        // Mass properties of fixed bodies are discarded (effective inverse
        // mass/inertia is zeroed), so the expensive from_voxels computation
        // must be skipped entirely: local mass stays at its default (0).
        assert_eq!(
            body.mass(),
            0.0,
            "fixed terrain body computed collider mass properties"
        );
        checked += 1;
    }
    assert_eq!(checked, 2, "one body per chunk");
}
