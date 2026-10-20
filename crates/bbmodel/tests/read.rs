//! The reader against real Blockbench files (MIT, from unnamed/hephaestus-engine,
//! `fixtures/hephaestus/LICENSE`) and small hand-written ones that pin the
//! conventions: rotation order and sign, face/UV layout, 4.x vs 5.0 groups,
//! mesh elements, absent faces, hidden elements.
use bbmodel::{AlphaKind, Model, parse};
use serde_json::{Value, json};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn load(name: &str) -> Model {
    parse(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// A 1x1 white PNG as a data URL.
fn white_png() -> String {
    use base64::Engine as _;
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 255, 255, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn cube(uuid: &str, from: [f32; 3], to: [f32; 3], origin: [f32; 3], rotation: [f32; 3]) -> Value {
    let face = json!({"uv": [0, 0, 16, 16], "texture": 0});
    json!({"uuid": uuid, "name": uuid, "from": from, "to": to, "origin": origin, "rotation": rotation,
           "faces": {"north": face, "south": face, "east": face, "west": face, "up": face, "down": face}})
}

fn model(elements: Vec<Value>, groups: Option<Vec<Value>>, outliner: Vec<Value>) -> Model {
    let mut file = json!({
        "meta": {"format_version": "5.0", "model_format": "free", "box_uv": false},
        "name": "test", "resolution": {"width": 16, "height": 16},
        "elements": elements, "outliner": outliner,
        "textures": [{"name": "white", "source": white_png()}],
    });
    if let Some(g) = groups {
        file["groups"] = Value::Array(g);
    }
    parse(&serde_json::to_vec(&file).unwrap()).unwrap()
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 1e-4)
}

#[test]
fn a_plain_cube_is_six_faces_with_its_face_uvs() {
    let m = load("hephaestus/cube.bbmodel");
    assert_eq!(m.triangle_count(), 12);
    assert_eq!(m.textures.len(), 1);
    let (lo, hi) = m.bounds().unwrap();
    let file: Value = serde_json::from_slice(&fixture("hephaestus/cube.bbmodel")).unwrap();
    let e = &file["elements"][0];
    let f = |k: &str, i| e[k][i].as_f64().unwrap() as f32;
    assert!(close(lo, [f("from", 0), f("from", 1), f("from", 2)]), "{lo:?}");
    assert!(close(hi, [f("to", 0), f("to", 1), f("to", 2)]), "{hi:?}");
    // Every UV stays inside the texture.
    for p in m.nodes.iter().flat_map(|n| &n.parts) {
        assert!(p.uvs.iter().all(|uv| (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1])));
    }
}

/// Box UV is recomputed from the cube's size (Blockbench's `updateUV`); the
/// file also stores the resolved face UVs, so the two must agree (up to the
/// 1/64 px anti-bleed margin).
#[test]
fn box_uv_matches_what_blockbench_stored() {
    let bytes = fixture("hephaestus/cube_boxuv.bbmodel");
    let m = parse(&bytes).unwrap();
    let file: Value = serde_json::from_slice(&bytes).unwrap();
    let res = file["resolution"]["width"].as_f64().unwrap() as f32;
    let part = &m.nodes.iter().flat_map(|n| &n.parts).next().unwrap();
    // Faces are emitted east, west, up, down, south, north, 4 vertices each;
    // vertex 0 carries (u1, v1) and vertex 3 (u2, v2).
    for (i, dir) in ["east", "west", "up", "down", "south", "north"].iter().enumerate() {
        let uv = &file["elements"][0]["faces"][dir]["uv"];
        let want = |k| uv[k].as_f64().unwrap() as f32 / res;
        let (a, b) = (part.uvs[i * 4], part.uvs[i * 4 + 3]);
        for (got, w) in [(a[0], want(0)), (a[1], want(1)), (b[0], want(2)), (b[1], want(3))] {
            assert!((got - w).abs() <= 1.0 / 64.0 / res + 1e-6, "{dir}: {got} vs {w}");
        }
    }
}

/// Three.js Euler order ZYX, right-handed: +90 deg about Z turns +X into +Y.
#[test]
fn rotation_is_zyx_and_right_handed() {
    let z = model(vec![cube("a", [0.0, 0.0, 0.0], [16.0, 1.0, 1.0], [0.0; 3], [0.0, 0.0, 90.0])], None, vec![json!("a")]);
    let (lo, hi) = z.bounds().unwrap();
    assert!(close(lo, [-1.0, 0.0, 0.0]) && close(hi, [0.0, 16.0, 1.0]), "{lo:?} {hi:?}");
    // X then Z: a +X rod rotated 90 about X stays on X, then 90 about Z goes to +Y.
    let xz = model(vec![cube("a", [0.0, 0.0, 0.0], [16.0, 1.0, 1.0], [0.0; 3], [90.0, 0.0, 90.0])], None, vec![json!("a")]);
    let (lo, hi) = xz.bounds().unwrap();
    assert!(lo[1] > -1.01 && hi[1] > 15.9, "X applied first, then Z: {lo:?} {hi:?}");
    // Y: +90 about Y turns +X into -Z.
    let y = model(vec![cube("a", [0.0, 0.0, 0.0], [16.0, 1.0, 1.0], [0.0; 3], [0.0, 90.0, 0.0])], None, vec![json!("a")]);
    let (lo, _) = y.bounds().unwrap();
    assert!((lo[2] + 16.0).abs() < 1e-3, "{lo:?}");
}

#[test]
fn faces_point_outward() {
    let m = model(vec![cube("a", [0.0; 3], [2.0; 3], [1.0; 3], [0.0; 3])], None, vec![json!("a")]);
    let part = &m.nodes[0].parts[0];
    for tri in part.indices.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|i| part.positions[tri[i] as usize]);
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
        let centre = [(a[0] + b[0] + c[0]) / 3.0 - 1.0, (a[1] + b[1] + c[1]) / 3.0 - 1.0, (a[2] + b[2] + c[2]) / 3.0 - 1.0];
        let out = n[0] * centre[0] + n[1] * centre[1] + n[2] * centre[2];
        assert!(out > 0.0, "counter-clockwise from outside");
        let normal = part.normals[tri[0] as usize];
        assert!(normal[0] * n[0] + normal[1] * n[1] + normal[2] * n[2] > 0.0, "normal agrees with winding");
    }
}

/// The same hierarchy saved the 4.x way (groups inline in the outliner) and
/// the 5.0 way (`groups[]` + uuid tree) reads identically.
#[test]
fn groups_read_the_same_in_4x_and_5_0_layouts() {
    let elements = vec![
        cube("body", [-4.0, 0.0, -4.0], [4.0, 8.0, 4.0], [0.0; 3], [0.0; 3]),
        cube("arm", [4.0, 4.0, -1.0], [10.0, 6.0, 1.0], [4.0, 5.0, 0.0], [0.0, 0.0, 30.0]),
    ];
    let inline = vec![json!({"uuid": "g1", "name": "torso", "origin": [0, 4, 0], "rotation": [0, 45, 0],
        "children": ["body", {"uuid": "g2", "name": "shoulder", "origin": [4, 5, 0], "rotation": [10, 0, 0], "children": ["arm"]}]})];
    let groups = vec![
        json!({"uuid": "g1", "name": "torso", "origin": [0, 4, 0], "rotation": [0, 45, 0]}),
        json!({"uuid": "g2", "name": "shoulder", "origin": [4, 5, 0], "rotation": [10, 0, 0]}),
    ];
    let tree = vec![json!({"uuid": "g1", "children": ["body", {"uuid": "g2", "children": ["arm"]}]})];
    let a = model(elements.clone(), None, inline);
    let b = model(elements, Some(groups), tree);
    assert_eq!(a.nodes.len(), 3);
    assert_eq!(b.nodes.len(), 3);
    for (x, y) in a.nodes.iter().zip(&b.nodes) {
        assert_eq!((&x.name, x.parent, x.translation, x.rotation_deg), (&y.name, y.parent, y.translation, y.rotation_deg));
        let pos = |n: &bbmodel::Node| n.parts.iter().map(|p| p.positions.clone()).collect::<Vec<_>>();
        assert_eq!(pos(x), pos(y));
    }
    // Bones: the shoulder sits at its origin relative to the torso's.
    assert_eq!(a.nodes[2].translation, [4.0, 1.0, 0.0]);
    let (_, hi) = a.bounds().unwrap();
    assert!(hi[1] > 8.0, "the rotated arm reaches above the body: {hi:?}");
}

#[test]
fn hidden_elements_absent_faces_and_untextured_faces() {
    let mut hidden = cube("hidden", [0.0; 3], [1.0; 3], [0.0; 3], [0.0; 3]);
    hidden["visibility"] = json!(false);
    let mut noexport = cube("noexport", [0.0; 3], [1.0; 3], [0.0; 3], [0.0; 3]);
    noexport["export"] = json!(false);
    let mut partial = cube("partial", [0.0; 3], [1.0; 3], [0.0; 3], [0.0; 3]);
    partial["faces"]["up"]["texture"] = Value::Null; // absent
    partial["faces"]["down"]["texture"] = json!(false); // untextured
    let m = model(vec![hidden, noexport, partial], None, vec![json!("hidden"), json!("noexport"), json!("partial")]);
    assert_eq!(m.triangle_count(), 10, "5 faces of the visible cube");
    let untextured = m.nodes[0].parts.iter().find(|p| p.texture.is_none()).unwrap();
    assert_eq!(untextured.triangle_count(), 2);
}

#[test]
fn zero_thickness_planes_still_have_faces() {
    let m = model(vec![cube("plane", [0.0, 0.0, 0.0], [8.0, 8.0, 0.0], [0.0; 3], [0.0; 3])], None, vec![json!("plane")]);
    assert_eq!(m.triangle_count(), 12);
}

#[test]
fn mesh_elements_triangulate_quads_and_tris() {
    let mesh = json!({"uuid": "m", "type": "mesh", "name": "m", "origin": [0, 0, 0], "rotation": [0, 0, 0],
        "vertices": {"a": [0, 0, 0], "b": [4, 0, 0], "c": [4, 0, 4], "d": [0, 0, 4], "e": [2, 4, 2]},
        "faces": {
            // A quad given in a crossing order: sorted into its outline.
            "q": {"vertices": ["a", "b", "d", "c"], "uv": {"a": [0, 0], "b": [16, 0], "c": [16, 16], "d": [0, 16]}, "texture": 0},
            "t": {"vertices": ["a", "b", "e"], "uv": {"a": [0, 0], "b": [16, 0], "e": [8, 8]}, "texture": 0}
        }});
    let m = model(vec![mesh], None, vec![json!("m")]);
    assert_eq!(m.triangle_count(), 3);
    // The quad's two triangles cover it: area 16.
    let part = &m.nodes[0].parts[0];
    let area: f32 = part.indices.chunks(3).take(2).map(|t| {
        let [a, b, c] = [0, 1, 2].map(|i| part.positions[t[i] as usize]);
        ((b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2])).abs() / 2.0
    }).sum();
    assert!((area - 16.0).abs() < 1e-4, "quad area {area}");
}

/// Real mobs: every reader path at scale, nothing silently dropped.
#[test]
fn real_models_read_whole() {
    for name in [
        "hephaestus/dragon.bbmodel",
        "hephaestus/redstone_monstrosity.bbmodel",
        "hephaestus/geometry.bbmodel",
        "hephaestus/cube_multitexture.bbmodel",
        "hephaestus/correct_x_axis_rotation_test.bbmodel",
        "hephaestus/correct_y_axis_rotation_test.bbmodel",
        "hephaestus/correct_z_axis_rotation_test.bbmodel",
    ] {
        let m = load(name);
        let file: Value = serde_json::from_slice(&fixture(name)).unwrap();
        let cubes = file["elements"].as_array().unwrap().len();
        assert!(m.triangle_count() > 0, "{name}");
        assert!(m.triangle_count() <= cubes * 12, "{name}");
        assert!(m.textures.iter().all(|t| t.rgba.len() == (t.width * t.height * 4) as usize), "{name}");
        let unexpected: Vec<_> = m.warnings.iter().filter(|w| !w.contains("animation")).collect();
        assert!(unexpected.is_empty(), "{name}: {unexpected:?}");
    }
    let multi = load("hephaestus/cube_multitexture.bbmodel");
    assert_eq!(multi.nodes.iter().flat_map(|n| &n.parts).filter(|p| p.texture.is_some()).count(), 2, "one part per texture");
}

#[test]
fn alpha_is_classified_per_texture() {
    let m = load("hephaestus/cube.bbmodel");
    assert!(matches!(m.textures[0].alpha, AlphaKind::Opaque | AlphaKind::Mask | AlphaKind::Blend));
}
