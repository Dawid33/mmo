/// Regression: cell-mode region LOD went empty outside generated cells; seen in play: user report "LOD wasn't loading as I moved" quoted in fix 8941e9b9
#[test]
fn a_partial_overlay_keeps_pristine_terrain_where_it_has_no_columns() {
    // Cell-mode regions only hold generated cells: the LOD of such a region
    // must fall back to pristine coarse terrain everywhere else, not go empty.
    use game::lod::{LodCell, LodKey};
    use game::{BRICK_VOXEL_COUNT, BlockId, Chunk, ChunkCoords, Voxel};
    let key = LodKey::for_region(16, game::RegionCoords::new(0, 0));
    assert_eq!(key.region_span(), 1, "fixture: a one-region factor-16 key");
    let per_side = (game::lod::cells_per_region(16) / 32).max(1) as i32;
    let coarse = move |_: &LodKey| -> Vec<LodCell> {
        let mut cells = Vec::new();
        for x in 0..per_side {
            for z in 0..per_side {
                cells.push(LodCell {
                    group: [x, 0, z],
                    grid: vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT],
                });
            }
        }
        cells
    };
    // Authoritative terrain: one chunk in group (0,0,0) only.
    let overlay = vec![(
        ChunkCoords::new(0, 0, 0),
        Chunk::flat_floor_with(8, BlockId(7)),
    )];
    let (_, payload, _) = game::build_lod_job(
        game::LodBuildJob::Tile {
            key,
            overlays: vec![(game::RegionCoords::new(0, 0), overlay)],
            stored: vec![],
        },
        &coarse,
        &game::lod::LodWater::dry,
        &game::no_pristine_generator,
        &game::NoStoredTerrain,
    );
    let groups: std::collections::BTreeSet<[i32; 3]> = payload.iter().map(|(g, _)| *g).collect();
    assert_eq!(
        groups.len() as i32,
        per_side * per_side,
        "every column keeps terrain: {groups:?}"
    );
    let g0 = payload.iter().find(|(g, _)| *g == [0, 0, 0]).unwrap();
    let grid = g0.1.decode().unwrap();
    assert!(
        grid.iter().any(|v| v.block == BlockId(7)),
        "the overlaid column is authoritative"
    );
    assert!(
        !grid.iter().all(|v| v.block == BlockId(1)),
        "not the coarse fill"
    );
}

#[path = "support/lod_reference.rs"]
mod lod_reference;

// ---- LOD water surface layer (water plan Task 8) ---------------------------
