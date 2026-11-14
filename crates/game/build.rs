//! Embeds `assets/prefabs/**` (minus the skipped sub-directories) as a sorted slice of
//! `(file name, bytes)` in `OUT_DIR/builtin_prefabs.rs`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Mirrors `BUILTIN_SKIPPED_DIRS` in `src/prefab/builtin.rs`.
const SKIPPED_DIRS: &[&str] = &["imported", "sources"];

fn walk(dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) {
                walk(&path, files);
            }
        } else {
            files.push((name, path));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let dir = manifest.join("../../assets/prefabs");
    let mut files = Vec::new();
    walk(&dir, &mut files);
    files.sort();
    let mut out = String::from("&[\n");
    for (name, path) in &files {
        let abs = path.canonicalize().unwrap_or_else(|_| path.clone());
        let _ = writeln!(out, "    ({:?}, include_bytes!({:?}) as &[u8]),", name, abs.to_string_lossy());
    }
    out.push_str("]\n");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets it"));
    std::fs::write(out_dir.join("builtin_prefabs.rs"), out).expect("write builtin_prefabs.rs");
    println!("cargo:rerun-if-changed=build.rs");
}
