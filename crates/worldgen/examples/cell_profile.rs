//! Headless breakdown of cell activation (spec phase 2, "measure first"):
//! what the cells around the cell-mode spawn are made of, and — with
//! `--time` — what materializing them costs on the region thread.
//!
//!   VOXEL_REGION_BLOCKS=2048 cargo run --release -p worldgen --example cell_profile -- \
//!       [--cells 32] [--radius 2] [--centre X Z] [--time]
//!
//! Composition (always): per cell, chunk count by class — all-water,
//! uniform solid (one cuboid collider), partial (voxels colliders), empty —
//! collider count and payload size (bincode / lz4). Timing (`--time`):
//! each cell goes through `GameEventKind::CellChanges` on a server-style
//! region (the real activation path, `HASH_MODE` from the env like the
//! binaries), then the scans that run per activation or per terrain-hash
//! check: `cell_terrain_term`, `cell_hash`, `terrain_fingerprint`.

use std::time::Instant;

use game::{
    BlockId, CellChanges, CellCoords, CellPayload, Chunk, DormantCell, GameEventKind, Region,
    RegionCoords, TerrainSnapshot,
};

const WATER: BlockId = BlockId(5);

#[derive(Default, Clone, Copy)]
struct Composition {
    chunks: usize,
    all_water: usize,
    uniform_solid: usize,
    partial: usize,
    has_water: usize,
    empty: usize,
    colliders: usize,
    voxel_colliders: usize,
}

impl Composition {
    fn add(&mut self, o: &Composition) {
        self.chunks += o.chunks;
        self.all_water += o.all_water;
        self.uniform_solid += o.uniform_solid;
        self.partial += o.partial;
        self.has_water += o.has_water;
        self.empty += o.empty;
        self.colliders += o.colliders;
        self.voxel_colliders += o.voxel_colliders;
    }
}

fn classify(chunks: &[(game::ChunkCoords, Chunk)]) -> Composition {
    let mut c = Composition { chunks: chunks.len(), ..Default::default() };
    for (_, chunk) in chunks {
        let cells = game::collider_cells(&chunk.blocks, &chunk.chisel);
        if chunk.is_uniform(game::Block::new(WATER)) {
            c.all_water += 1;
        } else if chunk.chisel.is_empty() && chunk.blocks.iter().all(|b| *b == chunk.blocks[0]) {
            c.uniform_solid += 1;
        } else if cells.is_empty() {
            c.empty += 1;
        } else {
            c.partial += 1;
        }
        if chunk.blocks.iter().any(|b| b.id == WATER) {
            c.has_water += 1;
        }
        if cells.is_full_cube() {
            c.colliders += 1;
        } else {
            let n = cells.grids().count();
            c.colliders += n;
            c.voxel_colliders += n;
        }
    }
    c
}

fn arg<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    game::set_hash_mode_from_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--cells` is in BLOCKS (like VOXEL_CELL_BLOCKS); the game API takes chunks.
    let cell_chunks: usize = arg::<usize>(&args, "--cells", 32) / game::CHUNK_BLOCKS;
    let radius: u32 = arg(&args, "--radius", 2);
    let time = args.iter().any(|a| a == "--time");
    let rc = RegionCoords::new(0, 0);
    let materials = worldgen::material::Materials {
        stone: BlockId(2),
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: WATER,
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
    };
    let spawn = game::player_spawn_xz(true);
    let spawn_cell = game::cell_of_pos(spawn.0, spawn.1, cell_chunks).expect("spawn inside the region");
    // `--centre X Z`: another cell (e.g. out at sea) instead of the spawn's.
    let centre = args
        .iter()
        .position(|a| a == "--centre")
        .and_then(|i| Some(game::CellCoords::new(args.get(i + 1)?.parse().ok()?, args.get(i + 2)?.parse().ok()?)))
        .unwrap_or(spawn_cell);
    let cells = game::cells_within(centre, radius, cell_chunks);
    println!(
        "REGION_BLOCKS {} cell_chunks {cell_chunks} radius {radius}: {} cells around {centre:?}",
        game::REGION_BLOCKS,
        cells.len()
    );

    let mut payloads = Vec::new();
    let mut total = Composition::default();
    let (mut raw_bytes, mut wire_bytes) = (0usize, 0usize);
    println!(
        "{:>10} {:>6} {:>6} {:>7} {:>7} {:>6} {:>6} {:>6} {:>9} {:>9}",
        "cell", "chunks", "water", "uniform", "partial", "w-mix", "empty", "coll", "raw KiB", "lz4 KiB"
    );
    for &cell in &cells {
        let columns = worldgen::generate_cell(rc, cell, cell_chunks, worldgen::WORLD_SEED, materials);
        let mut chunks = TerrainSnapshot::from_columns(columns).chunks;
        chunks.sort_by_key(|(k, _)| (k.x, k.y, k.z));
        let comp = classify(&chunks);
        let payload = CellPayload {
            cell,
            version: 0,
            materialized_hash: 0,
            content: DormantCell { chunks, entities: Vec::new() },
        };
        let raw = bincode::serialized_size(&payload).unwrap_or(0) as usize;
        let wire = game::encode_packet(&payload).map_or(0, |b| b.len());
        println!(
            "{:>10} {:>6} {:>6} {:>7} {:>7} {:>6} {:>6} {:>6} {:>9.1} {:>9.1}",
            format!("({},{})", cell.x, cell.z),
            comp.chunks,
            comp.all_water,
            comp.uniform_solid,
            comp.partial,
            comp.has_water - comp.all_water,
            comp.empty,
            comp.colliders,
            raw as f64 / 1024.0,
            wire as f64 / 1024.0,
        );
        total.add(&comp);
        raw_bytes += raw;
        wire_bytes += wire;
        payloads.push(payload);
    }
    let pct = |n: usize| 100.0 * n as f64 / total.chunks.max(1) as f64;
    println!(
        "total: {} chunks — all-water {} ({:.0}%), uniform solid {} ({:.0}%), partial {} ({:.0}%), \
         water-mixed {} ({:.0}%), empty {}; {} colliders ({} voxels shapes); payload {:.1} MiB raw, {:.1} MiB lz4",
        total.chunks,
        total.all_water,
        pct(total.all_water),
        total.uniform_solid,
        pct(total.uniform_solid),
        total.partial,
        pct(total.partial),
        total.has_water - total.all_water,
        pct(total.has_water - total.all_water),
        total.empty,
        total.colliders,
        total.voxel_colliders,
        raw_bytes as f64 / (1024.0 * 1024.0),
        wire_bytes as f64 / (1024.0 * 1024.0),
    );

    if !time {
        println!("(pass --time for materialize / scan timings)");
        return;
    }
    let mut region = Region::new_cells(rc, cell_chunks, None, None);
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    println!("{:>10} {:>9} {:>9} {:>9}", "cell", "activate", "term", "hash");
    for payload in payloads {
        let cell: CellCoords = payload.cell;
        let t = Instant::now();
        region
            .handle_event(GameEventKind::CellChanges(CellChanges { deactivate: Vec::new(), activate: vec![payload] }))
            .expect("activation");
        region.commit_server_event();
        let activate = ms(t);
        let t = Instant::now();
        std::hint::black_box(region.data().cell_terrain_term(cell));
        let term = ms(t);
        let t = Instant::now();
        std::hint::black_box(region.data().cell_hash(cell));
        let hash = ms(t);
        println!("{:>10} {activate:>9.2} {term:>9.2} {hash:>9.2}", format!("({},{})", cell.x, cell.z));
    }
    let t = Instant::now();
    std::hint::black_box(region.terrain_fingerprint());
    println!("terrain_fingerprint over all active cells: {:.2} ms", ms(t));
}
