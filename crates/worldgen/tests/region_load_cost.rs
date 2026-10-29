//! Headless benchmark of the CPU work behind "receive and load a region".
//! Ignored by default (prints, doesn't assert); run it via `just bench-region`
//! or `cargo test -p worldgen --release --test region_load_cost -- --ignored --nocapture`.
//!
//! It isolates each stage the real client/server run per region so a profile or
//! a before/after tweak has concrete numbers to compare against. The GPU upload
//! (`meshes.add` + `RenderAssetUsages`) is NOT covered here — that needs the
//! live client under `just perf-client`.
use std::time::Instant;

fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
    }
}

#[test]
#[ignore = "benchmark; run explicitly via `just bench-region`"]
fn region_load_cost() {
    // Warm up allocator/caches so the first stage isn't charged for them.
    let _ = worldgen::generate_region(game::RegionCoords::new(0, 0), 1, mats());

    // 1. Server-side generation of one region (64 columns).
    let t = Instant::now();
    let columns = worldgen::generate_region(game::RegionCoords::new(3, 5), 1, mats());
    let gen_t = t.elapsed();

    // Flatten each column's materialized (Dense) sub-chunks into the actual
    // per-chunk `Chunk`s, mirroring what the wire/client path deals with.
    let chunks: Vec<(i32, game::Chunk)> = columns
        .iter()
        .flat_map(|(_, col)| col.materialized_sub_chunks())
        .collect();

    // 2. Client-side: derive the 32^3 voxel array for every chunk. This is what
    //    `spawn_region_snapshot` does SYNCHRONOUSLY, on the main schedule, for
    //    all 64 chunks in the single frame a region arrives — a guaranteed hitch.
    let t = Instant::now();
    let mut total_voxels = 0usize;
    let mut derived = Vec::with_capacity(chunks.len());
    for (_, c) in &chunks {
        // Dense voxels exist per 32³ render brick, never per sim chunk.
        for (_, brick) in c.bricks() {
            let v = brick.voxels();
            total_voxels += v.len();
            derived.push(v);
        }
    }
    let derive = t.elapsed();

    // 3. bincode round-trip of the column grid (proxy for the wire cost paid on
    //    the server's writer thread and the client's netcode thread).
    let t = Instant::now();
    let bytes = bincode::serialize(&columns).unwrap();
    let ser = t.elapsed();
    let t = Instant::now();
    let _back: Vec<(game::ColumnCoords, game::Column)> = bincode::deserialize(&bytes).unwrap();
    let de = t.elapsed();

    // 4. Greedy mesher over every derived chunk — the per-chunk async meshing
    //    job (`build_chunk_mesh`) `queue_meshing` spawns, minus the bevy Mesh
    //    alloc. Typically the single largest slice of "load a region" wall-clock.
    use block_mesh::{greedy_quads, GreedyQuadsBuffer, RIGHT_HANDED_Y_UP_CONFIG};
    let t = Instant::now();
    let mut quads_total = 0usize;
    for v in &derived {
        let mut buffer = GreedyQuadsBuffer::new(v.len());
        greedy_quads(v, &game::BrickShape {}, [0; 3], [31; 3], &RIGHT_HANDED_Y_UP_CONFIG.faces, &mut buffer);
        quads_total += buffer.quads.num_quads();
    }
    let mesh = t.elapsed();

    eprintln!("--- region load cost ({} chunks) ---", chunks.len());
    eprintln!("generate_region:      {:>12?}", gen_t);
    eprintln!("brick voxels  x{:<3}:    {:>12?}   ({} voxels, sync on main)", chunks.len(), derive, total_voxels);
    eprintln!("bincode serialize:    {:>12?}   ({} bytes/region)", ser, bytes.len());
    eprintln!("bincode deserialize:  {:>12?}", de);
    eprintln!("greedy_quads x{:<3}:     {:>12?}   ({} quads over {} bricks, async pool)", chunks.len(), mesh, quads_total, derived.len());
    eprintln!("  per-chunk derive:   {:>12?}", derive / chunks.len() as u32);
    eprintln!("  per-chunk mesh:     {:>12?}", mesh / chunks.len() as u32);
    eprintln!("  per-brick mesh:     {:>12?}", mesh / derived.len().max(1) as u32);
}
