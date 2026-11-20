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
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
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
const STONE: &str = "minecraft:stone_bricks";

/// A v2 schematic from a size, a per-cell state closure and sign entities `(pos, text)`.
fn schem(
    size: [u32; 3],
    signs: &[([i32; 3], &str)],
    mut f: impl FnMut(u32, u32, u32) -> &'static str,
) -> Vec<u8> {
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
    let palette = V::Compound(
        states
            .iter()
            .enumerate()
            .map(|(i, s)| (s.to_string(), V::Int(i as i32)))
            .collect(),
    );
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
        (
            "Metadata",
            c(vec![("Name", s("Test")), ("Author", s("Tester"))]),
        ),
    ]);
    gzip(&nbt("Schematic", &root))
}

fn write_schem(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.map(|e| e.unwrap().path().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn bins(out: &Path) -> Vec<String> {
    listing(out)
        .iter()
        .filter_map(|p| p.rsplit('/').next())
        .filter(|n| n.ends_with(".prefab.bin"))
        .map(String::from)
        .collect()
}

/// Regression: split dropped single-build files: on the IMPORT RUN over the real 482-file library 14 small decorations yielded zero builds (and 67 single buildings lost layers) — controller finding I4 'from running the tool over the real library' (session 67a33e5e 2026-11-14 19:17), fixed by the whole-file fallback in ef2ec4c1
#[test]
fn a_tiny_decoration_is_not_dropped() {
    let dir = temp_dir("tiny");
    let input = write_schem(
        &dir,
        "tiny.schem",
        &schem([5, 5, 5], &[], |x, y, z| {
            if y == 0 && z == 2 && x < 3 {
                STONE
            } else {
                AIR
            }
        }),
    );
    let out = dir.join("out");
    let o = run(&[
        "split",
        input.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert_eq!(bins(&out), vec!["tiny.prefab.bin"]);
    done(&dir);
}
