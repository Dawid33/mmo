//! Embeds `quest/corpus/{run,fail}/<case>/` into the crate (wasm32 has no filesystem): every `*.qs` file
//! (paths relative to the case, `/` separators), `expect.txt` and, for a run case, `golden.txt` when it
//! exists. Cases and files are sorted by name, and the generated code names files relative to
//! `CARGO_MANIFEST_DIR`, so the output is the same on every machine.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// The corpus, relative to this crate's manifest.
const CORPUS: &str = "../../corpus";

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| {
            e.unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
                .path()
        })
        .collect();
    out.sort();
    out
}

/// Every `*.qs` under `dir`, as `/`-separated paths relative to it, sorted.
fn sources(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    for path in sorted_entries(dir) {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| panic!("{}: not a UTF-8 name", path.display()))
            .to_string();
        if path.is_dir() {
            sources(&path, &format!("{prefix}{name}/"), out);
        } else if name.ends_with(".qs") {
            out.push(format!("{prefix}{name}"));
        }
    }
}

/// `include_str!` of `rel` (relative to the manifest directory).
fn include(rel: &str) -> String {
    format!(
        "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), {:?}))",
        format!("/{rel}")
    )
}

fn cases(manifest: &Path, kind: &str, with_golden: bool) -> String {
    let dir = manifest.join(CORPUS).join(kind);
    let mut code = String::new();
    for case in sorted_entries(&dir) {
        if !case.is_dir() {
            continue;
        }
        let name = case
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| panic!("{}: not a UTF-8 name", case.display()))
            .to_string();
        let base = format!("{CORPUS}/{kind}/{name}");
        let mut files = Vec::new();
        sources(&case, "", &mut files);
        files.sort();
        assert!(
            files.iter().any(|f| f == "main.qs"),
            "corpus case {kind}/{name} has no main.qs"
        );
        assert!(
            case.join("expect.txt").is_file(),
            "corpus case {kind}/{name} has no expect.txt"
        );
        let _ = write!(
            code,
            "    Case {{\n        name: {name:?},\n        files: &[\n"
        );
        for f in &files {
            let _ = writeln!(
                code,
                "            ({f:?}, {}),",
                include(&format!("{base}/{f}"))
            );
        }
        let _ = writeln!(code, "        ],");
        let _ = writeln!(
            code,
            "        expect: {},",
            include(&format!("{base}/expect.txt"))
        );
        let golden = if with_golden && case.join("golden.txt").is_file() {
            format!("Some({})", include(&format!("{base}/golden.txt")))
        } else {
            "None".to_string()
        };
        let _ = writeln!(code, "        golden: {golden},\n    }},");
    }
    code
}

fn main() {
    let manifest =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    // A directory is scanned recursively for changes, so an added, edited or blessed file rebuilds.
    println!("cargo:rerun-if-changed={CORPUS}");
    let code = format!(
        "/// The `run/` cases, sorted by name.\npub const RUN: &[Case] = &[\n{}];\n\n\
         /// The `fail/` cases, sorted by name.\npub const FAIL: &[Case] = &[\n{}];\n",
        cases(&manifest, "run", true),
        cases(&manifest, "fail", false),
    );
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("corpus.rs");
    fs::write(&out, code).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}
