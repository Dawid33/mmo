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
