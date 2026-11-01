//! The forge pipeline as a library: a small world forges, serialises and
//! round-trips through the world-file format.

use worldgen::forge::geology::{rock_index, ROCKS};
use worldgen::forge::{forge, ForgeParams};
use worldgen::forged::WorldArtifact;

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forge_small_world_round_trips() {
    // Seed 4: a 2 km world with dry land for a spawn site (kilometre relief
    // leaves no flat one: spawn::pick falls back to the flattest dry patch).
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
/// when the default layout changes, and the climate to the island presets'
/// (no pole, the whole map at 45 deg: westerlies, like the old west-only
/// rain sweep). Re-bless with `FORGE_QUALITY_BLESS=1`
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
    p.knobs.pole = worldgen::forge::climate::POLE_NONE as f64;
    p.knobs.latitude_center_deg = 45.0;
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

/// The default knobs build kilometre-plus relief (spec §3): a 16 km / 32 m
/// forge of the default layout reaches a 1.5 km peak, and the geology column
/// is deep enough for every level the erosion exhumes.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn default_forge_reaches_kilometre_peaks() {
    use worldgen::forge::quality::quality;

    let p = ForgeParams { size_m: 16_000.0, cell_m: 32.0, ..ForgeParams::default() };
    let forged = forge(&p);
    let q = quality(&forged);
    assert!(q.max_peak_m >= 1_500.0, "tallest peak {:.0} m, want >= 1,500 m", q.max_peak_m);
    assert!(q.max_peak_m <= p.knobs.max_peak_m + 1.0e-6, "peak {:.0} m over the cap", q.max_peak_m);
    let w = &forged.world;
    let deepest = (0..w.n * w.n)
        .filter(|&i| !w.ocean[i])
        .map(|i| w.h[i] - w.uplifted[i] - forged.geology.bedding[i])
        .fold(f64::MAX, f64::min);
    assert!(
        deepest > worldgen::forge::geology::COLUMN_BOTTOM_S,
        "the default forge exhumes s = {deepest:.0} m, below the geology column"
    );
}
