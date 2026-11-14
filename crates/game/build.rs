//! Embeds `assets/prefabs/**` (every `*.prefab.ron` / `*.prefab.bin`, recursively) as a sorted slice of
//! `(file name, bytes)` in `OUT_DIR/builtin_prefabs.rs`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The same few lines as `read_dir_sorted` in `src/prefab/builtin.rs`: keep the two filters identical
/// (a file named `*.prefab.ron` or `*.prefab.bin`, not starting with `.`; directories starting with `.`
/// and symlinked directories are not entered; a name that is not UTF-8 is skipped).
fn takes_file(name: &str) -> bool {
    !name.starts_with('.') && (name.ends_with(".prefab.ron") || name.ends_with(".prefab.bin"))
}

fn walk(dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(name) = e.file_name().into_string() else { continue };
        let Ok(kind) = e.file_type() else { continue };
        let path = e.path();
        if kind.is_dir() {
            if !name.starts_with('.') {
                walk(&path, files);
            }
        } else if takes_file(&name) && (kind.is_file() || path.is_file()) {
            files.push((name, path));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let dir = manifest.join("../../assets/prefabs");
    let mut files = Vec::new();
    if dir.is_dir() {
        walk(&dir, &mut files);
    }
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
