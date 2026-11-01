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

/// Quality gate: a 16 km / 32 m forge must stay within tolerance of the
/// recorded baseline (`tests/fixtures/forge_quality_16k_32m.ron`). Output-
/// changing forge speed-ups must pass it. The layout is pinned to the
/// one-island layout explicitly so the gate keeps measuring the same world
/// when the default layout changes. Re-bless with `FORGE_QUALITY_BLESS=1`
/// ONLY on purpose (a deliberate change of the world), never to make a
/// failure go away.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forge_quality_matches_baseline() {
    use worldgen::forge::knobs::LAYOUT_ONE_ISLAND;
    use worldgen::forge::quality::{quality, within, Quality, QualityTolerance};
    use worldgen::forge::world::DEFAULT_SEED;

    let mut p = ForgeParams { size_m: 16_000.0, cell_m: 32.0, seed: DEFAULT_SEED, ..ForgeParams::default() };
    p.knobs.layout = LAYOUT_ONE_ISLAND as f64;
    let q = quality(&forge(&p));
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/forge_quality_16k_32m.ron");
    if std::env::var_os("FORGE_QUALITY_BLESS").is_some() {
        std::fs::write(path, q.to_text()).expect("write baseline");
        eprintln!("blessed {path}:\n{}", q.to_text());
        return;
    }
    let text = std::fs::read_to_string(path).expect("baseline fixture");
    let baseline = Quality::from_text(&text).expect("baseline parses");
    if let Err(e) = within(&q, &baseline, &QualityTolerance::DEFAULT) {
        panic!("{e}");
    }
}
