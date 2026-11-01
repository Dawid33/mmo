//! Headless breakdown of a region snapshot's wire size — what
//! `ServerPacket::Region` carries for one freshly generated region.
//!
//!   cargo run --release -p worldgen --example snapshot_size -- [rx rz]

use game::{BlockId, Region, RegionCoords};

/// MiB of `value` in the wire encoding (bincode). A macro rather than a
/// generic fn: `worldgen` does not depend on `serde` by name.
macro_rules! size {
    ($value:expr) => {
        bincode::serialized_size($value).map_or(0.0, |n| n as f64 / (1024.0 * 1024.0))
    };
}

fn main() {
    let args: Vec<i32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let rc = RegionCoords::new(*args.first().unwrap_or(&0), *args.get(1).unwrap_or(&0));
    let materials = worldgen::material::Materials {
        stone: BlockId(2),
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
        biome: worldgen::material::BiomeBlocks::NONE,
    };
    let columns = worldgen::generate_region(rc, worldgen::WORLD_SEED, materials);
    let region = Region::from_columns(rc, columns);
    let data = &region.data().data;

    let chunks: Vec<&game::Chunk> = data.ecs.chunk.iter().filter_map(|(_, c)| c.as_ref()).collect();
    let chiseled_blocks: usize = chunks.iter().map(|c| c.chisel.len()).sum();
    let blocks_only: f64 = chunks.iter().map(|c| size!(&c.blocks)).sum();
    let chisel_only: f64 = chunks.iter().map(|c| size!(&c.chisel)).sum();

    println!("region {rc:?}: {} chunks, {chiseled_blocks} chiseled blocks of {}", chunks.len(), chunks.len() * 8);
    println!("{:>9.2} MiB  whole snapshot (GameData)", size!(&**data));
    println!("{:>9.2} MiB    ecs (all components)", size!(&data.ecs));
    println!("{:>9.2} MiB      chunk blocks", blocks_only);
    println!("{:>9.2} MiB      chunk chisel data  ({:.2} KiB per chiseled block)", chisel_only, chisel_only * 1024.0 / chiseled_blocks.max(1) as f64);
    println!("{:>9.2} MiB    physics (all)", size!(&data.physics));
    println!("{:>9.2} MiB      colliders", size!(&data.physics.colliders));
    println!("{:>9.2} MiB      bodies", size!(&data.physics.bodies));
    println!("{:>9.2} MiB      broad phase", size!(&data.physics.broad_phase));
    println!("{:>9.2} MiB      narrow phase", size!(&data.physics.narrow_phase));
    println!("{:>9.2} MiB      islands", size!(&data.physics.islands));
    // The real wire path: `ServerPacket::Region` through `game::encode_packet`.
    let packet = game::ServerPacket::Region(rc, region.data().clone());
    let t = std::time::Instant::now();
    let wire = game::encode_packet(&packet).expect("region packet encodes");
    let encode_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = std::time::Instant::now();
    let decoded: game::ServerPacket = game::decode_packet(&wire).expect("region packet decodes");
    let decode_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(matches!(decoded, game::ServerPacket::Region(id, _) if id == rc));
    println!(
        "{:>9.2} MiB  on the wire (format byte {}), encode {encode_ms:.0} ms, decode {decode_ms:.0} ms",
        wire.len() as f64 / (1024.0 * 1024.0),
        wire[0]
    );

    // SNAPSHOT_DUMP=<path> writes the exact wire bytes, for compression experiments.
    if let Ok(path) = std::env::var("SNAPSHOT_DUMP") {
        let bytes = bincode::serialize(&**data).expect("snapshot serializes");
        std::fs::write(&path, &bytes).expect("write snapshot dump");
        println!("wrote {} bytes to {path}", bytes.len());
    }
}
