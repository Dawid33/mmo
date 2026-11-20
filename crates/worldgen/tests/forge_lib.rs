//! The forge pipeline as a library: a small world forges, serialises and
//! round-trips through the world-file format.

use worldgen::forge::{ForgeParams, forge};

/// Gate: the forge's quality score stays at its baseline (forge tunables must pass it)
/// Quality gate: the dev Planet (16,384 × 8,192 m at 32 m, the default seed
/// and knobs) must stay within tolerance of the recorded baseline
/// (`tests/fixtures/forge_quality_planet_dev_32m.ron`). Output-changing forge
/// speed-ups must pass it. Re-bless with `FORGE_QUALITY_BLESS=1` ONLY on
/// purpose (a deliberate change of the world), never to make a failure go
/// away.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn forge_quality_matches_baseline() {
    use worldgen::forge::quality::{Quality, QualityTolerance, quality, within};
    use worldgen::forge::world::DEFAULT_SEED;

    let p = ForgeParams {
        seed: DEFAULT_SEED,
        ..ForgeParams::default()
    };
    let q = quality(&forge(&p));
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/forge_quality_planet_dev_32m.ron"
    );
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
