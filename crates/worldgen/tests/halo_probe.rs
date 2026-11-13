//! Memory of one region's terrain halo (`game::halo`), on generated terrain:
//! the neighbours' band chunks a region mirrors. A probe, not a gate — run at
//! the play size with `just region_blocks=256 test-one worldgen "--test it --
//! --include-ignored --nocapture halo_probe::"`.
use game::{Block, ChunkBlocks, RegionCoords, TerrainSnapshot, REGION_BLOCKS, REGION_CHUNKS};
use worldgen::material::Materials;

fn mats() -> Materials {
    let manifest = include_str!("../../../assets/blocks/blocks.ron");
    let reg = game::BlockRegistry::from_ron(manifest).expect("valid manifest");
    Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")))
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn halo_memory_per_region() {
    let rc = RegionCoords::new(0, 0);
    let (mut chunks, mut dense, mut columns, mut heap, mut wire) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for n in game::halo::neighbours(rc) {
        let generated: Vec<_> = game::halo::facing_windows(n, rc)
            .into_iter()
            .flat_map(|(xs, zs)| worldgen::generate_columns(n, xs, zs, worldgen::WORLD_SEED, mats()))
            .collect();
        columns += generated.len();
        let snap = TerrainSnapshot::from_columns(generated);
        let seed = game::halo::halo_seed_from_chunks(n, rc, snap.chunks.iter().map(|(c, k)| (*c, k))).unwrap();
        assert_eq!(seed.deltas.len(), snap.chunks.len(), "every generated chunk of the band is mirrored");
        for d in &seed.deltas {
            let chunk = d.data.as_ref().unwrap();
            chunks += 1;
            // In memory: the chunk itself, a dense grid's blocks, the chisel map's entries.
            heap += std::mem::size_of::<game::HaloChunk>();
            if let ChunkBlocks::Dense(b) = &chunk.blocks {
                dense += 1;
                heap += b.len() * std::mem::size_of::<Block>();
            }
            heap += chunk.chisel.values().map(|c| std::mem::size_of_val(c) + 16).sum::<usize>();
            wire += bincode::serialized_size(chunk).unwrap() as usize;
        }
    }
    eprintln!(
        "[halo-probe] region_blocks={REGION_BLOCKS} chunk_blocks={}: {columns} halo columns \
         ({} own), {chunks} chunks ({dense} dense), ~{} KiB in memory (chunk data, excl. colliders), \
         {} KiB serialized",
        game::CHUNK_BLOCKS,
        REGION_CHUNKS * REGION_CHUNKS,
        heap / 1024,
        wire / 1024
    );
    assert_eq!(columns, 4 * REGION_CHUNKS * game::HALO_CHUNKS as usize + 4 * (game::HALO_CHUNKS as usize).pow(2));
}
