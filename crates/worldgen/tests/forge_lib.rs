//! The forge pipeline as a library: a small world forges, serialises and
//! round-trips through the world-file format.

use worldgen::forge::geology::{rock_index, ROCKS};
use worldgen::forge::{forge, ForgeParams};
use worldgen::forged::WorldArtifact;

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forge_small_world_round_trips() {
    // Seed 4: most seeds drown a 2 km world's every flat dry site (spawn::pick panics).
    let p = ForgeParams { size_m: 2048.0, cell_m: 64.0, seed: 4, ..ForgeParams::default() };
    let artifact = forge(&p).artifact(&p);
    let n = (p.size_m / p.cell_m).round() as usize;
    assert_eq!(artifact.height.len(), n * n);
    let names: Vec<String> = ROCKS.iter().map(|r| r.name.to_string()).collect();
    assert_eq!(artifact.rock_names, names);

    let bytes = artifact.to_bytes();
    let back = WorldArtifact::from_bytes(&bytes).expect("world file parses");
    assert_eq!(back.rock_names, names);
    assert_eq!(back.height.len(), n * n);
    assert_eq!(back.to_bytes(), bytes);
    for (i, r) in ROCKS.iter().enumerate() {
        assert_eq!(rock_index(r.name), Some(i));
    }
    assert_eq!(rock_index("marble"), None);
}
