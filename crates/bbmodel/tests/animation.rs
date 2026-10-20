//! Animation sampling against Blockbench's preview rules (see
//! `src/animation.rs`), with hand-written clips and the real fixtures.
use bbmodel::molang::Context;
use bbmodel::{BonePose, LoopMode, Model, parse};
use serde_json::{Value, json};

fn model_with(version: &str, keyframes: Vec<Value>, loop_mode: &str, length: f32) -> Model {
    let file = json!({
        "meta": {"format_version": version, "model_format": "free", "box_uv": false},
        "name": "anim", "resolution": {"width": 16, "height": 16},
        "elements": [{"uuid": "c", "name": "c", "from": [0, 0, 0], "to": [1, 1, 1], "origin": [0, 0, 0],
                      "faces": {"up": {"uv": [0, 0, 1, 1], "texture": null}}}],
        "groups": [{"uuid": "bone", "name": "bone", "origin": [0, 4, 0], "rotation": [10, 0, 0]}],
        "outliner": [{"uuid": "bone", "children": ["c"]}],
        "textures": [],
        "animations": [{"name": "clip", "loop": loop_mode, "length": length,
                        "animators": {"bone": {"name": "bone", "type": "bone", "keyframes": keyframes}}}],
    });
    parse(&serde_json::to_vec(&file).unwrap()).unwrap()
}

fn key(channel: &str, time: f32, interpolation: &str, points: Vec<[Value; 3]>) -> Value {
    let dps: Vec<Value> = points.into_iter().map(|[x, y, z]| json!({"x": x, "y": y, "z": z})).collect();
    json!({"channel": channel, "time": time, "interpolation": interpolation, "data_points": dps})
}

fn n(v: f32) -> Value {
    json!(v)
}

fn pose(m: &Model, t: f32) -> BonePose {
    let a = &m.animations[0];
    a.sample(a.local_time(t), &Context::default())[0].1
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

#[test]
fn linear_and_step() {
    let m = model_with("5.0", vec![
        key("position", 0.0, "linear", vec![[n(0.0), n(0.0), n(0.0)]]),
        key("position", 1.0, "linear", vec![[n(4.0), n(8.0), n(0.0)]]),
        key("rotation", 0.0, "step", vec![[n(0.0), n(0.0), n(0.0)]]),
        key("rotation", 1.0, "step", vec![[n(90.0), n(0.0), n(0.0)]]),
    ], "hold", 1.0);
    let p = pose(&m, 0.25);
    assert!(approx(p.position[0], 1.0) && approx(p.position[1], 2.0), "{p:?}");
    assert_eq!(p.rotation[0], 0.0, "step holds the earlier key");
    assert_eq!(pose(&m, 1.0).rotation[0], 90.0, "on the key");
    assert_eq!(pose(&m, 5.0).position[0], 4.0, "hold stops on the last frame");
    assert_eq!(pose(&m, 0.5).scale, [1.0; 3], "no scale keys: unscaled");
}

#[test]
fn pre_and_post_data_points_make_a_jump() {
    let m = model_with("5.0", vec![
        key("position", 0.0, "linear", vec![[n(0.0), n(0.0), n(0.0)]]),
        key("position", 1.0, "linear", vec![[n(10.0), n(0.0), n(0.0)], [n(-10.0), n(0.0), n(0.0)]]),
        key("position", 2.0, "linear", vec![[n(0.0), n(0.0), n(0.0)]]),
    ], "once", 2.0);
    assert!(approx(pose(&m, 0.999).position[0], 9.99), "arrives at the pre value");
    assert!(approx(pose(&m, 1.5).position[0], -5.0), "leaves from the post value");
}

#[test]
fn loop_modes() {
    let keys = || vec![
        key("position", 0.0, "linear", vec![[n(0.0), n(0.0), n(0.0)]]),
        key("position", 2.0, "linear", vec![[n(2.0), n(0.0), n(0.0)]]),
    ];
    let looping = model_with("5.0", keys(), "loop", 2.0);
    assert!(approx(pose(&looping, 2.5).position[0], 0.5), "wraps");
    let once = model_with("5.0", keys(), "once", 2.0);
    assert_eq!(once.animations[0].loop_mode, LoopMode::Once);
    assert!(once.animations[0].finished(2.1) && !once.animations[0].finished(1.9));
    assert!(approx(pose(&once, 9.0).position[0], 2.0));
}

#[test]
fn molang_values_read_the_animation_time() {
    let m = model_with("5.0", vec![key("rotation", 0.0, "linear",
        vec![[json!("math.sin(query.anim_time * 90) * 30"), n(0.0), n(0.0)]])], "loop", 4.0);
    assert!(approx(pose(&m, 1.0).rotation[0], 30.0), "{:?}", pose(&m, 1.0));
    assert!(approx(pose(&m, 2.0).rotation[0], 0.0));
}

#[test]
fn catmull_rom_passes_through_its_keys_and_smooths_between() {
    let m = model_with("5.0", vec![
        key("position", 0.0, "catmullrom", vec![[n(0.0), n(0.0), n(0.0)]]),
        key("position", 1.0, "catmullrom", vec![[n(10.0), n(0.0), n(0.0)]]),
        key("position", 2.0, "catmullrom", vec![[n(0.0), n(0.0), n(0.0)]]),
    ], "once", 2.0);
    assert!(approx(pose(&m, 1.0).position[0], 10.0));
    let near_peak = pose(&m, 0.9).position[0];
    assert!(near_peak > 9.0 && near_peak < 10.5, "a curve, not a line: {near_peak}");
    let a = pose(&m, 0.5).position[0];
    assert!(a > 5.0, "eases out of the first key: {a}");
}

#[test]
fn bezier_eases_between_keys() {
    let m = model_with("5.0", vec![
        key("position", 0.0, "bezier", vec![[n(0.0), n(0.0), n(0.0)]]),
        key("position", 1.0, "bezier", vec![[n(10.0), n(0.0), n(0.0)]]),
    ], "once", 1.0);
    let quarter = pose(&m, 0.25).position[0];
    let half = pose(&m, 0.5).position[0];
    assert!(approx(half, 5.0), "symmetric handles: half way at half time ({half})");
    assert!(quarter < 2.5, "eased start: {quarter}");
}

#[test]
fn files_before_5_0_flip_axes_as_blockbench_does() {
    let keys = || vec![key("rotation", 0.0, "linear", vec![[n(20.0), n(30.0), n(40.0)]]),
                       key("position", 0.0, "linear", vec![[n(1.0), n(2.0), n(3.0)]])];
    let new = pose(&model_with("5.0", keys(), "hold", 1.0), 0.0);
    let old = pose(&model_with("4.9", keys(), "hold", 1.0), 0.0);
    assert_eq!(new.rotation, [20.0, 30.0, 40.0]);
    assert_eq!(old.rotation, [-20.0, -30.0, 40.0]);
    assert_eq!(old.position, [-1.0, 2.0, 3.0]);
}

#[test]
fn local_pose_adds_rotation_to_the_rest_euler() {
    let m = model_with("5.0", vec![key("rotation", 0.0, "linear", vec![[n(20.0), n(0.0), n(0.0)]]),
                                   key("position", 0.0, "linear", vec![[n(0.0), n(1.0), n(0.0)]])], "hold", 1.0);
    let a = &m.animations[0];
    let local = m.local_pose(&a.sample(0.0, &Context::default()));
    let bone = 1;
    let want = bbmodel::euler_zyx_quat([30.0, 0.0, 0.0]);
    assert!((0..4).all(|i| approx(local[bone].1[i], want[i])), "rest 10 + 20 = 30 degrees about X");
    assert_eq!(local[bone].0, [0.0, 5.0, 0.0], "rest offset 4 + 1");
    assert_eq!(local[0].1, [0.0, 0.0, 0.0, 1.0], "the root is untouched");
}

#[test]
fn real_animations_map_to_bones_and_sample_finite() {
    for name in ["dragon", "redstone_monstrosity", "correct_x_axis_rotation_test", "geometry"] {
        let path = format!("{}/tests/fixtures/hephaestus/{name}.bbmodel", env!("CARGO_MANIFEST_DIR"));
        let m = parse(&std::fs::read(&path).unwrap()).unwrap();
        let unmapped: Vec<_> = m.warnings.iter().filter(|w| w.contains("animation")).collect();
        assert!(unmapped.is_empty(), "{name}: {unmapped:?}");
        for a in &m.animations {
            assert!(!a.tracks.is_empty(), "{name}/{}: no tracks", a.name);
            for step in 0..=20 {
                let t = a.length * step as f32 / 20.0;
                for (_, p) in a.sample(t, &Context::default()) {
                    let all = p.rotation.iter().chain(&p.position).chain(&p.scale);
                    assert!(all.clone().all(|v| v.is_finite()), "{name}/{} at {t}: {p:?}", a.name);
                }
            }
        }
    }
    let dragon = parse(&std::fs::read(format!("{}/tests/fixtures/hephaestus/dragon.bbmodel", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
    assert!(dragon.animation("flying").is_some());
}
