//! Near-chunk meshing cost on real terrain, off the render path: generate the
//! production 3x3 join window, then run each stage the client runs per chunk
//! (derive + RLE, padded gather, greedy quads, face connectivity) and report
//! per-stage cost plus worker-thread scaling.
//!
//! `cargo run --release -p client --example mesh_bench [-- threads=1,2,4,8]`

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use client::renderer::bridge::VoxelData;
use client::renderer::meshing::binary::mesh_quads;
use client::renderer::meshing::neighbors::{gather_padded_rle, zone_offsets};
use client::renderer::meshing::visibility::face_connectivity;
use game::{BlockId, Chunk, RegionCoords, REGION_CHUNKS};

type Key = [i32; 3];

fn window() -> Vec<(Key, Chunk)> {
    let mats = worldgen::material::Materials {
        stone: BlockId(2),
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
    };
    let mut out = Vec::new();
    for rz in -1..=1 {
        for rx in -1..=1 {
            let rc = RegionCoords::new(rx, rz);
            for (cc, column) in worldgen::generate_region(rc, worldgen::WORLD_SEED, mats) {
                for (sub_y, chunk) in column.materialized_sub_chunks() {
                    let key = [
                        rx * REGION_CHUNKS as i32 + cc.x as i32,
                        sub_y,
                        rz * REGION_CHUNKS as i32 + cc.z as i32,
                    ];
                    out.push((key, chunk));
                }
            }
        }
    }
    out
}

#[derive(Default)]
struct Stage {
    derive: Duration,
    gather: Duration,
    quads: Duration,
    connectivity: Duration,
}

/// Mesh one chunk exactly like the async task does (records packing aside).
fn mesh_one(key: Key, grid: &HashMap<Key, VoxelData>, stage: Option<&mut Stage>) -> usize {
    let t0 = Instant::now();
    let center = &grid[&key];
    let mut neighbors = BTreeMap::new();
    for off in zone_offsets() {
        let nk = [key[0] + off[0], key[1] + off[1], key[2] + off[2]];
        if let Some(nv) = grid.get(&nk) {
            neighbors.insert(off, nv);
        }
    }
    let padded = gather_padded_rle(center, &neighbors);
    let t1 = Instant::now();
    let quads = mesh_quads(&padded);
    let t2 = Instant::now();
    let conn = face_connectivity(&padded);
    let t3 = Instant::now();
    std::hint::black_box(conn);
    if let Some(s) = stage {
        s.gather += t1 - t0;
        s.quads += t2 - t1;
        s.connectivity += t3 - t2;
    }
    quads.len()
}

fn main() {
    let threads: Vec<usize> = std::env::args()
        .find_map(|a| a.strip_prefix("threads=").map(str::to_owned))
        .unwrap_or_else(|| "1,2,4,8".into())
        .split(',')
        .map(|t| t.parse().expect("threads=1,2,4"))
        .collect();

    let t = Instant::now();
    let chunks = window();
    println!("worldgen: {} chunks in 3x3 regions, {:?}", chunks.len(), t.elapsed());

    let mut stage = Stage::default();
    let t = Instant::now();
    let mut grid: HashMap<Key, VoxelData> = HashMap::new();
    let (mut unchiselled, mut uniform_solid, mut uniform_air) = (0, 0, 0);
    for (key, chunk) in &chunks {
        if chunk.chisel.is_empty() {
            unchiselled += 1;
            let first = chunk.blocks[0].id;
            if chunk.blocks.iter().all(|b| b.id == first) {
                if first == BlockId::AIR {
                    uniform_air += 1;
                } else {
                    uniform_solid += 1;
                }
            }
        }
        let voxels = game::derive_voxels(&chunk.blocks, &chunk.chisel);
        grid.insert(*key, VoxelData::new(voxels));
    }
    stage.derive = t.elapsed();
    let n = chunks.len();
    println!(
        "chunks: unchiselled {unchiselled} ({:.0}%), uniform solid {uniform_solid}, uniform air {uniform_air}",
        100.0 * unchiselled as f64 / n as f64
    );

    let keys: Vec<Key> = chunks.iter().map(|(k, _)| *k).collect();
    let (mut empty, mut total_quads) = (0usize, 0usize);
    let t = Instant::now();
    for &k in &keys {
        let q = mesh_one(k, &grid, Some(&mut stage));
        total_quads += q;
        empty += (q == 0) as usize;
    }
    let single = t.elapsed();
    let per = |d: Duration| d.as_secs_f64() * 1e6 / n as f64;
    println!(
        "zero-quad chunks: {empty} ({:.0}%), quads total {total_quads}, avg {:.1}/chunk",
        100.0 * empty as f64 / n as f64,
        total_quads as f64 / n as f64
    );
    println!("per chunk (us, 1 thread):");
    println!("  derive+rle    {:7.1}   (snapshot-voxel task)", per(stage.derive));
    println!("  gather_padded {:7.1}", per(stage.gather));
    println!("  mesh_quads    {:7.1}", per(stage.quads));
    println!("  connectivity  {:7.1}", per(stage.connectivity));
    println!("  mesh task sum {:7.1}   (whole window {single:?})", per(single));

    for &t_count in &threads {
        let t = Instant::now();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..t_count {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&k) = keys.get(i) else { break };
                    std::hint::black_box(mesh_one(k, &grid, None));
                });
            }
        });
        let wall = t.elapsed();
        println!(
            "threads={t_count:2}: {wall:?} -> {:.0} chunks/s",
            n as f64 / wall.as_secs_f64()
        );
    }
}
