//! The creature models' lint (`just creature-check`): every model in
//! `assets/models/creatures/creatures.json` (written by
//! `tools/creatures/build.py`) read with the real parser must follow the
//! creature style guide (docs/superpowers/specs/2026-10-29-creature-asset-pipeline.md):
//! no reader warnings, its template's clips with the manifest's loop modes and
//! lengths, within the template's budgets, and the size the manifest records.
use bbmodel::{LoopMode, Model, parse, quat_rotate};
use serde_json::Value;

fn assets() -> String {
    format!("{}/../../assets", env!("CARGO_MANIFEST_DIR"))
}

fn manifest() -> serde_json::Map<String, Value> {
    let path = format!("{}/models/creatures/creatures.json", assets());
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    match serde_json::from_str(&text).expect("manifest is JSON") {
        Value::Object(m) => m,
        _ => panic!("manifest is not an object"),
    }
}

/// The rest pose's box: every vertex through its node chain (rotation, then
/// the translation from the parent).
fn rest_bounds(model: &Model) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for (i, node) in model.nodes.iter().enumerate() {
        for part in &node.parts {
            for &p in &part.positions {
                let mut p = p;
                let mut n = Some(i);
                while let Some(j) = n {
                    let node = &model.nodes[j];
                    let r = quat_rotate(node.rotation_quat(), p);
                    p = [r[0] + node.translation[0], r[1] + node.translation[1], r[2] + node.translation[2]];
                    n = node.parent;
                }
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
    }
    (lo, hi)
}

fn vec3(v: &Value) -> [f32; 3] {
    let a = v.as_array().expect("a 3-vector");
    [0, 1, 2].map(|i| a[i].as_f64().unwrap() as f32)
}

#[test]
fn creature_models_follow_the_style_guide() {
    let manifest = manifest();
    assert!(!manifest.is_empty(), "no creatures in the manifest");
    let mut problems = Vec::new();
    for (id, entry) in &manifest {
        let rel = entry["model"].as_str().unwrap();
        let path = format!("{}/{rel}", assets());
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{id}: {path}: {e}"));
        let model = parse(&bytes).unwrap_or_else(|e| panic!("{id}: {e}"));
        let mut bad = |what: String| problems.push(format!("{id}: {what}"));

        for w in &model.warnings {
            bad(format!("reader warning: {w}"));
        }
        if model.textures.len() != 1 {
            bad(format!("{} textures, the style guide says one", model.textures.len()));
        }

        // Every cube is 6 faces of 2 triangles; the root node is not a bone.
        let cubes = model.nodes.iter().flat_map(|n| &n.parts).map(|p| p.triangle_count()).sum::<usize>() / 12;
        let bones = model.nodes.len() - 1;
        let budget = &entry["budget"];
        if cubes as u64 > budget["cubes"].as_u64().unwrap() {
            bad(format!("{cubes} cubes over the budget {}", budget["cubes"]));
        }
        if bones as u64 > budget["bones"].as_u64().unwrap() {
            bad(format!("{bones} bones over the budget {}", budget["bones"]));
        }
        if cubes as u64 != entry["cubes"].as_u64().unwrap() {
            bad(format!("{cubes} cubes but the manifest says {} (stale model?)", entry["cubes"]));
        }

        for required in entry["required_clips"].as_array().unwrap() {
            let name = required.as_str().unwrap();
            if model.animation(name).is_none() {
                bad(format!("missing required clip {name:?}"));
            }
        }
        for (name, clip) in entry["clips"].as_object().unwrap() {
            let Some(anim) = model.animation(name) else {
                bad(format!("manifest clip {name:?} not in the model"));
                continue;
            };
            let mode = match anim.loop_mode {
                LoopMode::Loop => "loop",
                LoopMode::Once => "once",
                LoopMode::Hold => "hold",
            };
            if clip["loop"].as_str() != Some(mode) {
                bad(format!("clip {name:?} is {mode}, the manifest says {}", clip["loop"]));
            }
            let length = clip["length"].as_f64().unwrap() as f32;
            if (anim.length - length).abs() > 1e-3 {
                bad(format!("clip {name:?} is {} s, the manifest says {length}", anim.length));
            }
            if anim.tracks.is_empty() {
                bad(format!("clip {name:?} moves nothing"));
            }
        }

        // Feet on the ground, and the size the game will use for its collider.
        let (lo, hi) = rest_bounds(&model);
        let bounds = entry["bounds"].as_array().unwrap();
        let (want_lo, want_hi) = (vec3(&bounds[0]), vec3(&bounds[1]));
        if lo[1].abs() > 1e-3 {
            bad(format!("rest pose bottom at y = {}, not on the ground", lo[1]));
        }
        for k in 0..3 {
            if (lo[k] - want_lo[k]).abs() > 1e-2 || (hi[k] - want_hi[k]).abs() > 1e-2 {
                bad(format!("rest bounds {lo:?}..{hi:?}, the manifest says {want_lo:?}..{want_hi:?}"));
                break;
            }
        }

        // The sim's colliders (`game::creature_shapes`): a move capsule and
        // one rest-pose box per bone with cubes, inside the bounds.
        let Some(collider) = entry.get("collider") else {
            bad("no collider".into());
            continue;
        };
        let mv = &collider["move"];
        if mv["radius"].as_f64().is_none_or(|r| r <= 0.0) || mv["half_height"].as_f64().is_none_or(|h| h < 0.0) {
            bad(format!("bad move capsule {mv}"));
        }
        let body = collider["body"].as_array().map(Vec::as_slice).unwrap_or_default();
        if body.is_empty() || body.len() > bones {
            bad(format!("{} body boxes for {bones} bones", body.len()));
        }
        for part in body {
            let (plo, phi) = (vec3(&part["min"]), vec3(&part["max"]));
            if (0..3).any(|k| plo[k] > phi[k] || plo[k] < want_lo[k] - 1e-3 || phi[k] > want_hi[k] + 1e-3) {
                bad(format!("body box {plo:?}..{phi:?} outside the bounds {want_lo:?}..{want_hi:?}"));
            }
        }
    }
    assert!(problems.is_empty(), "creature lint:\n{}", problems.join("\n"));
}
