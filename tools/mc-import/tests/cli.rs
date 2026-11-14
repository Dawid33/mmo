//! Runs the built binary from the repo root, as `just` does. Fixtures are built here, never copied.

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mc-import"))
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("run mc-import")
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn done(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mc-import-tool-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const AIR: &str = "minecraft:air";
const GRASS: &str = "minecraft:grass_block";
const STONE: &str = "minecraft:stone_bricks";

/// A v2 schematic from a size, a per-cell state closure and sign entities `(pos, text)`.
fn schem(size: [u32; 3], signs: &[([i32; 3], &str)], mut f: impl FnMut(u32, u32, u32) -> &'static str) -> Vec<u8> {
    let mut states: Vec<&str> = Vec::new();
    let mut cells = Vec::new();
    for y in 0..size[1] {
        for z in 0..size[2] {
            for x in 0..size[0] {
                let st = f(x, y, z);
                let i = states.iter().position(|s| *s == st).unwrap_or_else(|| {
                    states.push(st);
                    states.len() - 1
                });
                cells.push(i as u32);
            }
        }
    }
    let palette = V::Compound(states.iter().enumerate().map(|(i, s)| (s.to_string(), V::Int(i as i32))).collect());
    let entities = signs
        .iter()
        .map(|(p, t)| {
            let msg = V::List(8, vec![s(&format!("{{\"text\":\"{t}\"}}"))]);
            c(vec![
                ("Pos", V::Ints(p.to_vec())),
                ("Id", s("minecraft:sign")),
                ("front_text", c(vec![("messages", msg)])),
            ])
        })
        .collect();
    let root = c(vec![
        ("Version", V::Int(2)),
        ("DataVersion", V::Int(3463)),
        ("Width", V::Short(size[0] as i16)),
        ("Height", V::Short(size[1] as i16)),
        ("Length", V::Short(size[2] as i16)),
        ("Palette", palette),
        ("BlockData", V::Bytes(varints(&cells))),
        ("BlockEntities", V::List(10, entities)),
        ("Metadata", c(vec![("Name", s("Test")), ("Author", s("Tester"))])),
    ]);
    gzip(&nbt("Schematic", &root))
}

fn write_schem(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

/// A 6 x 5 x 6 stone house with a hollow inside.
fn house() -> Vec<u8> {
    schem([6, 5, 6], &[], |x, y, z| {
        let wall = x == 0 || x == 5 || z == 0 || z == 5;
        if y == 0 || y == 4 || wall {
            STONE
        } else {
            AIR
        }
    })
}

/// Three 4 x 6 x 4 pillars on a 2-layer plate, a sign `Zeus` beside the middle one.
fn pillars() -> Vec<u8> {
    schem([40, 12, 14], &[([20, 2, 5], "Zeus")], |x, y, z| {
        if y < 2 {
            GRASS
        } else if [3, 15, 30].iter().any(|&p| (p..p + 4).contains(&x)) && (4..8).contains(&z) && y < 8 {
            STONE
        } else {
            AIR
        }
    })
}

#[test]
fn import_writes_a_prefab_that_loads() {
    let dir = temp_dir("import");
    let input = write_schem(&dir, "My House.schem", &house());
    let out = dir.join("out");
    let o = run(&["import", input.to_str().unwrap(), "--out", out.to_str().unwrap(), "--tag", "test"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let bin = out.join("my-house.prefab.bin");
    let file = game::prefab::prefab_from_bin(&std::fs::read(&bin).unwrap(), None).unwrap();
    assert_eq!(file.name, "my-house");
    assert_eq!(file.meta.source, "My House.schem");
    assert_eq!(file.meta.author, "Tester");
    assert!(file.tags.contains(&"imported".to_string()) && file.tags.contains(&"test".to_string()));
    let info = file.meta.import.as_ref().unwrap();
    assert_eq!(info.source_hash.len(), 8);
    assert!(info.source_hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(info.mapping_version, mc_import::MAPPING_VERSION);
    assert!(out.join("my-house.report.txt").is_file());
    done(&dir);
}

#[test]
fn split_writes_one_prefab_per_build() {
    let dir = temp_dir("split");
    let input = write_schem(&dir, "pillars.schem", &pillars());
    let out = dir.join("out");
    let o = run(&["split", input.to_str().unwrap(), "--out", out.to_str().unwrap(), "--prefix", "p"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let mut names: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".prefab.bin"))
        .collect();
    names.sort();
    // Labels are opt-in: by default the builds are only numbered.
    assert_eq!(names, vec!["p-01.prefab.bin", "p-02.prefab.bin", "p-03.prefab.bin"]);
    for n in &names {
        let file = game::prefab::prefab_from_bin(&std::fs::read(out.join(n)).unwrap(), None).unwrap();
        assert_eq!(format!("{}.prefab.bin", file.name), *n);
        assert_eq!(file.size[1], 6, "{n} must not contain the plate");
    }
    done(&dir);
}

#[test]
fn split_labels_are_opt_in() {
    let dir = temp_dir("labels");
    let input = write_schem(&dir, "pillars.schem", &pillars());
    let out = dir.join("out");
    let o = run(&["split", input.to_str().unwrap(), "--out", out.to_str().unwrap(), "--prefix", "p", "--labels"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let mut names: Vec<String> = listing(&out)
        .iter()
        .filter_map(|p| p.rsplit('/').next().map(String::from))
        .filter(|n| n.ends_with(".prefab.bin"))
        .collect();
    names.sort();
    assert_eq!(names, vec!["p-01.prefab.bin", "p-03.prefab.bin", "p-zeus.prefab.bin"]);
    // `--labels` belongs to split.
    let o = run(&["import", input.to_str().unwrap(), "--labels"]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    done(&dir);
}

#[test]
fn a_write_error_on_one_build_does_not_stop_the_others() {
    let dir = temp_dir("writeerr");
    let input = write_schem(&dir, "pillars.schem", &pillars());
    let out = dir.join("out");
    std::fs::create_dir_all(out.join("p-01.prefab.bin")).unwrap(); // a directory where a file goes
    let o = run(&["split", input.to_str().unwrap(), "--out", out.to_str().unwrap(), "--prefix", "p"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("p-01"), "{}", text(&o));
    assert!(out.join("p-02.prefab.bin").is_file() && out.join("p-03.prefab.bin").is_file(), "{}", text(&o));
    done(&dir);
}

#[test]
fn the_summary_line_counts_carved_cells_and_usage_explains_split() {
    let dir = temp_dir("summary");
    let input = write_schem(&dir, "house.schem", &schem([6, 5, 6], &[], |x, y, z| {
        let wall = x == 0 || x == 5 || z == 0 || z == 5;
        if y == 0 || y == 4 || wall { STONE } else { AIR }
    }));
    let o = run(&["import", input.to_str().unwrap(), "--out", dir.join("o").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("48 carved"), "{}", text(&o));
    let u = text(&run(&[]));
    for needle in ["showroom", "--licence", "--labels", "--prefix"] {
        assert!(u.contains(needle), "usage lacks {needle}: {u}");
    }
    done(&dir);
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| r.map(|e| e.unwrap().path().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn report_writes_nothing() {
    let dir = temp_dir("report");
    let input = write_schem(&dir, "pillars.schem", &pillars());
    let imports = repo_root().join("prefab-imports");
    let (before_imports, before_dir) = (listing(&imports), listing(&dir));
    let o = run(&["report", input.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("pillars.schem"), "{t}");
    assert!(t.contains("builds 3"), "{t}");
    assert!(!t.contains("whole"), "{t}");
    assert_eq!(listing(&imports), before_imports);
    assert_eq!(listing(&dir), before_dir);
    done(&dir);
}

#[test]
fn bad_input() {
    let dir = temp_dir("bad");
    let junk = write_schem(&dir, "junk.schem", b"not a schematic at all");
    let o = run(&["import", junk.to_str().unwrap(), "--out", dir.join("o").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("junk.schem"), "{}", text(&o));
    let o = run(&[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).contains("usage"), "{}", text(&o));
    let o = run(&["import", junk.to_str().unwrap(), "--frobnicate"]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    done(&dir);
}

#[test]
fn refuses_to_write_under_assets() {
    let dir = temp_dir("assets");
    let input = write_schem(&dir, "house.schem", &house());
    let o = run(&["import", input.to_str().unwrap(), "--out", "assets/prefabs/imported-test"]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(!repo_root().join("assets/prefabs/imported-test").exists());
    done(&dir);
}

fn bins(out: &Path) -> Vec<String> {
    listing(out).iter().filter_map(|p| p.rsplit('/').next()).filter(|n| n.ends_with(".prefab.bin")).map(String::from).collect()
}

fn load_bin(path: &Path) -> game::prefab::PrefabFile {
    game::prefab::prefab_from_bin(&std::fs::read(path).unwrap(), None).unwrap()
}

#[test]
fn a_lone_cube_is_imported_whole_by_split() {
    let dir = temp_dir("cube");
    let input = write_schem(&dir, "cube.schem", &schem([8, 8, 8], &[], |_, _, _| STONE));
    let out = dir.join("out");
    let o = run(&["split", input.to_str().unwrap(), "--out", out.to_str().unwrap(), "--prefix", "c"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("note:") && text(&o).contains("one build, imported whole"), "{}", text(&o));
    assert_eq!(bins(&out), vec!["c.prefab.bin"]);
    assert_eq!(load_bin(&out.join("c.prefab.bin")).size[1], 8);
    done(&dir);
}

#[test]
fn a_tiny_decoration_is_not_dropped() {
    let dir = temp_dir("tiny");
    let input = write_schem(&dir, "tiny.schem", &schem([5, 5, 5], &[], |x, y, z| {
        if y == 0 && z == 2 && x < 3 { STONE } else { AIR }
    }));
    let out = dir.join("out");
    let o = run(&["split", input.to_str().unwrap(), "--out", out.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert_eq!(bins(&out), vec!["tiny.prefab.bin"]);
    done(&dir);
}

#[test]
fn an_explicit_ground_auto_does_not_fall_back() {
    let dir = temp_dir("explicit");
    let input = write_schem(&dir, "cube.schem", &schem([8, 8, 8], &[], |_, _, _| STONE));
    let out = dir.join("out");
    let o = run(&["split", input.to_str().unwrap(), "--out", out.to_str().unwrap(), "--ground", "auto"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("warning:"), "{}", text(&o));
    assert!(!text(&o).contains("imported whole"), "{}", text(&o));
    assert!(bins(&out).is_empty());
    done(&dir);
}

#[test]
fn report_marks_the_whole_fallback() {
    let dir = temp_dir("repwhole");
    let input = write_schem(&dir, "cube.schem", &schem([8, 8, 8], &[], |_, _, _| STONE));
    let o = run(&["report", input.to_str().unwrap()]);
    assert!(text(&o).contains("whole"), "{}", text(&o));
    done(&dir);
}

#[test]
fn import_of_an_empty_schematic_fails() {
    let dir = temp_dir("empty");
    let input = write_schem(&dir, "empty.schem", &schem([4, 4, 4], &[], |_, _, _| AIR));
    let o = run(&["import", input.to_str().unwrap(), "--out", dir.join("o").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("warning:") && text(&o).contains("nothing to convert"), "{}", text(&o));
    done(&dir);
}

#[test]
fn an_unwritable_out_fails_that_file_and_the_rest_continue() {
    let dir = temp_dir("unwritable");
    let input = write_schem(&dir, "house.schem", &house());
    let blocker = write_schem(&dir, "blocker", b"a file, not a directory");
    let o = run(&["import", input.to_str().unwrap(), "--out", blocker.join("sub").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    done(&dir);
}

#[test]
fn tags_are_deduplicated() {
    let dir = temp_dir("tags");
    let input = write_schem(&dir, "house.schem", &house());
    let out = dir.join("out");
    let o = run(&["import", input.to_str().unwrap(), "--out", out.to_str().unwrap(), "--tag", "imported", "--tag", "a", "--tag", "a"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert_eq!(load_bin(&out.join("house.prefab.bin")).tags, vec!["imported", "a"]);
    done(&dir);
}

#[test]
fn a_non_utf8_argument_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt;
    let o = Command::new(env!("CARGO_BIN_EXE_mc-import"))
        .arg("report")
        .arg(std::ffi::OsStr::from_bytes(b"bad\xff.schem"))
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
}
