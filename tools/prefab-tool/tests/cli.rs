//! Runs the built binary from the repo root, as `just` does.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_prefab-tool"))
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("run prefab-tool")
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prefab-tool-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

//// (width, height, RGB bytes; the tool writes 8-bit RGB)
fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    (info.width, info.height, buf[..info.buffer_size()].to_vec())
}

fn assert_hut_png(out: &Path) {
    let (w, h, px) = read_png(out);
    assert_eq!(w, (7 + 7 + 7) * 4 + 2 * 4);
    assert_eq!(h, 7 * 4);
    let first = &px[..3];
    assert!(px.chunks(3).any(|p| p != first), "image is one flat colour");
}

#[test]
fn check_passes_on_the_builtin_set() {
    let o = run(&["check"]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("dev-hut"), "{t}");
    assert!(t.contains("prefabs ok"), "{t}");
}

#[test]
fn check_fails_on_a_bad_file() {
    let dir = temp_dir("bad");
    std::fs::write(
        dir.join("bad.prefab.ron"),
        r#"(format: 2, name: "bad", size: (1, 1, 1), anchor: (0, 0, 0), palette: ["no_such_block"],
            boxes: [(min: (0, 0, 0), size: (1, 1, 1), mat: 0)])"#,
    )
    .unwrap();
    let o = run(&["check", dir.to_str().unwrap()]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("bad.prefab.ron"), "{t}");
    assert!(t.contains("no_such_block"), "{t}");
}

#[test]
fn view_writes_a_png() {
    let dir = temp_dir("view");
    let out = dir.join("hut.png");
    let o = run(&["view", "dev-hut", "-o", out.to_str().unwrap(), "--scale", "4"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_hut_png(&out);
}

#[test]
fn view_takes_a_path() {
    let dir = temp_dir("path");
    let out = dir.join("hut.png");
    let o = run(&["view", "assets/prefabs/dev-hut.prefab.ron", "-o", out.to_str().unwrap(), "--scale", "4"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_hut_png(&out);
}

/// Six small hand-written houses tagged `test-style` + `house`, a door marker on the south face.
fn write_test_houses(dir: &Path) {
    for (i, (sx, sz)) in [(7, 6), (9, 7), (8, 8), (6, 9), (11, 8), (7, 7)].into_iter().enumerate() {
        let (px, pz) = (sx / 2, sz - 1);
        let (ix, iz) = (sx - 2, sz - 2);
        // A pitched roof (height grows toward the middle) of a different wood per house.
        let roofs: String = (0..sx)
            .map(|x| format!("(min: ({x}, 3, 0), size: (1, {}, {sz}), mat: {}),", 1 + x.min(sx - 1 - x), 2 + i % 4))
            .collect();
        let ron = format!(
            r#"(format: 2, name: "test-house-{i}", size: ({sx}, 10, {sz}), anchor: ({px}, 0, {pz}),
            palette: ["stone_bricks", "oak_planks", "spruce_planks", "birch_planks", "bricks", "acacia_planks"],
            boxes: [
                (min: (0, 0, 0), size: ({sx}, 1, {sz}), mat: 0),
                (min: (0, 1, 0), size: ({sx}, 2, {sz}), mat: 1),
                {roofs}
            ],
            carve: [(min: (1, 1, 1), size: ({ix}, 2, {iz})), (min: ({px}, 1, {pz}), size: (1, 2, 1))],
            markers: [(at: ({px}, 1, {pz}), kind: "door")],
            tags: ["test-style", "house"])"#
        );
        std::fs::write(dir.join(format!("test-house-{i}.prefab.ron")), ron).unwrap();
    }
}

/// "extent 104x98 blocks ..." from the stdout.
fn extent_of(t: &str) -> (u32, u32) {
    let line = t.lines().find(|l| l.starts_with("extent ")).unwrap_or_else(|| panic!("no extent line:\n{t}"));
    let dims = line["extent ".len()..].split_whitespace().next().unwrap();
    let (w, d) = dims.split_once('x').unwrap();
    (w.parse().unwrap(), d.parse().unwrap())
}

fn lots_of(t: &str) -> usize {
    let line = t.lines().find(|l| l.starts_with("lots ")).unwrap_or_else(|| panic!("no lots line:\n{t}"));
    line["lots ".len()..].split_whitespace().next().unwrap().parse().unwrap()
}

#[test]
fn town_writes_a_png() {
    let dir = temp_dir("town");
    write_test_houses(&dir);
    let out = dir.join("town.png");
    let o = run(&["town", "test-style", "--dirs", dir.to_str().unwrap(), "--seed", "1", "-o", out.to_str().unwrap(), "--scale", "2"]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("test-style"), "{t}");
    assert!(lots_of(&t) > 0, "{t}");
    let (w, d) = extent_of(&t);
    let (iw, ih, px) = read_png(&out);
    assert_eq!((iw, ih), (w * 2, d * 2), "{t}");
    let colours: std::collections::BTreeSet<&[u8]> = px.chunks(3).collect();
    assert!(colours.len() >= 20, "only {} colours", colours.len());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn town_is_deterministic() {
    let dir = temp_dir("town-det");
    write_test_houses(&dir);
    let d = dir.to_str().unwrap();
    let png = |name: &str, seed: &str| {
        let out = dir.join(name);
        let o = run(&["town", "test-style", "--dirs", d, "--seed", seed, "-o", out.to_str().unwrap(), "--scale", "2"]);
        assert!(o.status.success(), "{}", text(&o));
        std::fs::read(&out).unwrap()
    };
    let (a, b, c) = (png("a.png", "1"), png("b.png", "1"), png("c.png", "2"));
    assert_eq!(a, b);
    assert_ne!(a, c);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_style_fails() {
    let dir = temp_dir("town-style");
    write_test_houses(&dir);
    let o = run(&["town", "no-such-style", "--dirs", dir.to_str().unwrap()]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("no-such-style"), "{t}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_flags() {
    for args in [
        &["town"][..],
        &["town", "medieval", "--bogus"],
        &["town", "medieval", "--seed", "x"],
        &["town", "medieval", "--arms", "5"],
        &["town", "medieval", "--terrain", "lava"],
        &["town", "medieval", "--scale", "0"],
        &["town", "medieval", "--len"],
    ] {
        let o = run(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", text(&o));
    }
}
