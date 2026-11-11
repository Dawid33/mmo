//! Embeds the game's own prefabs (`assets/prefabs/*.prefab.ron`) into the crate:
//! `OUT_DIR/builtin_prefabs.rs` is a list of `(file stem, RON text)` pairs, sorted by
//! stem, read by `prefab/builtin.rs`. A missing directory is an empty list.

use std::fmt::Write as _;
use std::path::PathBuf;

const SUFFIX: &str = ".prefab.ron";

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../assets/prefabs");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let stem = name.strip_suffix(SUFFIX)?.to_string();
                    Some((stem, e.path().canonicalize().ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let mut out = String::from("&[\n");
    for (stem, path) in &files {
        writeln!(out, "    ({stem:?}, include_str!({:?})),", path.display().to_string()).unwrap();
    }
    out.push_str("]\n");
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("builtin_prefabs.rs");
    std::fs::write(dest, out).unwrap();
}
