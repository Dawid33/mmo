//! Creature colliders from their `.bbmodel` rest pose, as written into
//! `assets/models/creatures/creature_colliders.json` by
//! `tools/creatures/build.py` (`game::creature_shapes`), checked against the
//! manifest `creatures.json`'s bounds.
use game::creature_shapes::{creature_shape, CREATURE_MAX_RADIUS, CREATURE_MIN_RADIUS};
use game::ColliderSpec;
use serde_json::Value;

fn manifest() -> serde_json::Map<String, Value> {
    let text = include_str!("../../../assets/models/creatures/creatures.json");
    match serde_json::from_str(text).expect("manifest is JSON") {
        Value::Object(m) => m,
        _ => panic!("manifest is not an object"),
    }
}

/// The manifest's rest-pose `bounds` of `id` as `(lo, hi)`.
fn bounds(entry: &Value) -> ([f32; 3], [f32; 3]) {
    let b = entry["bounds"].as_array().unwrap();
    let v = |v: &Value| {
        let a = v.as_array().unwrap();
        [0, 1, 2].map(|i| a[i].as_f64().unwrap() as f32)
    };
    (v(&b[0]), v(&b[1]))
}

fn capsule(spec: &ColliderSpec) -> (f32, f32) {
    match *spec {
        ColliderSpec::CapsuleY { half_height, radius } => (half_height, radius),
        ref other => panic!("move shape is not a Y capsule: {other:?}"),
    }
}

#[test]
fn every_manifest_creature_has_shapes() {
    let manifest = manifest();
    assert!(!manifest.is_empty());
    for (id, entry) in &manifest {
        let shape = creature_shape(id).unwrap_or_else(|| panic!("{id}: no shape"));
        let (half_height, radius) = capsule(&shape.move_shape);
        assert!(
            (CREATURE_MIN_RADIUS..=CREATURE_MAX_RADIUS).contains(&radius),
            "{id}: radius {radius} outside the clamp"
        );
        assert!(half_height >= 0.0, "{id}: half-height {half_height}");
        // The manifest's capsule is the rule over its bounds (build.py mirrors
        // the clamp constants; this pins the mirror).
        let (lo, hi) = bounds(entry);
        let want_r = ((hi[0] - lo[0]).min(hi[2] - lo[2]) / 2.0).clamp(CREATURE_MIN_RADIUS, CREATURE_MAX_RADIUS);
        let want_hh = ((hi[1] - lo[1]) / 2.0 - want_r).max(0.0);
        assert!((radius - want_r).abs() < 1e-3, "{id}: radius {radius}, the rule gives {want_r}");
        assert!((half_height - want_hh).abs() < 1e-3, "{id}: half-height {half_height}, the rule gives {want_hh}");
        match &shape.body {
            ColliderSpec::Compound(parts) => {
                assert!(!parts.is_empty(), "{id}: empty body");
                for (_, part) in parts {
                    let ColliderSpec::Cuboid { half_extents } = part else {
                        panic!("{id}: body part is not a cuboid: {part:?}");
                    };
                    assert!(half_extents.min_element() >= 0.0, "{id}: negative half extents");
                }
            }
            other => panic!("{id}: body is not a compound: {other:?}"),
        }
    }
    assert!(creature_shape("no_such_creature").is_none());
    // Stale-file guard: the embedded colliders cover exactly the manifest.
    let text = include_str!("../../../assets/models/creatures/creature_colliders.json");
    let colliders: serde_json::Map<String, Value> = serde_json::from_str(text).unwrap();
    assert!(colliders.keys().eq(manifest.keys()), "creature_colliders.json and creatures.json list different creatures (just creatures)");
}

#[test]
fn body_parts_stay_inside_bounds() {
    for (id, entry) in &manifest() {
        let (lo, hi) = bounds(entry);
        let ColliderSpec::Compound(parts) = &creature_shape(id).unwrap().body else { unreachable!() };
        let mut ulo = [f32::MAX; 3];
        let mut uhi = [f32::MIN; 3];
        for (pose, part) in parts {
            let ColliderSpec::Cuboid { half_extents: h } = part else { unreachable!() };
            let c = pose.translation;
            for (k, (c, h)) in [(c.x, h.x), (c.y, h.y), (c.z, h.z)].into_iter().enumerate() {
                ulo[k] = ulo[k].min(c - h);
                uhi[k] = uhi[k].max(c + h);
            }
        }
        for k in 0..3 {
            assert!(ulo[k] >= lo[k] - 1e-3 && uhi[k] <= hi[k] + 1e-3, "{id}: body {ulo:?}..{uhi:?} outside bounds {lo:?}..{hi:?}");
        }
        // The parts are the rest pose's cubes, so together they span it.
        for k in 0..3 {
            assert!((ulo[k] - lo[k]).abs() < 1e-3 && (uhi[k] - hi[k]).abs() < 1e-3, "{id}: body {ulo:?}..{uhi:?} short of bounds {lo:?}..{hi:?}");
        }
    }
}

#[test]
fn move_capsule_fits_height() {
    let manifest = manifest();
    for id in ["dwarf", "dog", "aardvark"] {
        let (lo, hi) = bounds(&manifest[id]);
        let height = hi[1] - lo[1];
        let (half_height, radius) = capsule(&creature_shape(id).unwrap().move_shape);
        let top = height / 2.0 + half_height + radius;
        assert!((top - height).abs() < 1e-3, "{id}: capsule top {top}, model height {height}");
    }
}

#[test]
fn body_mass_is_creature_density() {
    use game::creature_shapes::CREATURE_DENSITY;
    let shape = creature_shape("dog").unwrap();
    let ColliderSpec::Compound(parts) = &shape.body else { unreachable!() };
    let volume: f32 = parts
        .iter()
        .map(|(_, p)| match p {
            ColliderSpec::Cuboid { half_extents: h } => 8.0 * h.x * h.y * h.z,
            _ => unreachable!(),
        })
        .sum();
    let mp = game::mass_props::mass_properties(&shape.body, CREATURE_DENSITY);
    let mass = 1.0 / mp.inv_mass;
    assert!((mass - volume * CREATURE_DENSITY).abs() <= 1e-3 * mass, "dog mass {mass} vs {}", volume * CREATURE_DENSITY);
}
