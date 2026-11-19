//! The forge pipeline as a library: a small world forges, serialises and
//! round-trips through the world-file format.

use worldgen::forge::{forge, ForgeParams};

/// Gate: the forge's quality score stays at its baseline (forge tunables must pass it)
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

    let mut p = ForgeParams { seed: DEFAULT_SEED, ..ForgeParams::square(16_000.0, 32.0) };
    p.knobs.layout = LAYOUT_ONE_ISLAND as f64;
    p.knobs.pole = worldgen::forge::climate::POLE_NONE as f64;
    p.knobs.latitude_center_deg = 45.0;
    p.knobs.meander_amplitude_deg = 0.0;
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

