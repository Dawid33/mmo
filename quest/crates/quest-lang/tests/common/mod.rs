//! Helpers shared by the compiler's test binaries.

#![allow(dead_code)]

use quest_lang::{Diagnostic, SourceTree, check};

/// A source tree of `(path, text)` pairs.
pub fn tree(files: &[(&str, &str)]) -> SourceTree {
    SourceTree {
        files: files
            .iter()
            .map(|(path, text)| (path.to_string(), text.to_string()))
            .collect(),
    }
}

/// Every diagnostic the front end built so far reports on `files`, against the stub host's API, in the
/// order `check` gives them.
pub fn diagnostics(files: &[(&str, &str)]) -> Vec<Diagnostic> {
    match check(&tree(files), &quest_stub::stub_api()) {
        Ok(warnings) => warnings,
        Err(all) => all,
    }
}

/// The code, file and line of every diagnostic, sorted.
pub fn diags(files: &[(&str, &str)]) -> Vec<(String, String, u32)> {
    let mut out: Vec<(String, String, u32)> = diagnostics(files)
        .into_iter()
        .map(|d| (d.code.to_string(), d.file, d.line))
        .collect();
    out.sort();
    out
}
