//! The module tree (spec §5.10): the crate root `main.qs` and every module its `mod` items reach.
//!
//! A module's children live in its directory: the root's beside `main.qs`, those of `npc/mod.qs` or of
//! `npc.qs` in `npc/` (as in Rust). `mod x` is `<dir>/x.qs` or `<dir>/x/mod.qs`; both is `mod-ambiguous`,
//! neither `mod-not-found`. Files no `mod` reaches are not read.

use crate::ast::{Crate, ItemKind, Module};
use crate::diag::Diagnostic;
use crate::parse::parse_file;
use crate::source::SourceTree;
use std::collections::{BTreeMap, BTreeSet};

/// The crate root's file.
pub const ROOT_FILE: &str = "main.qs";

/// Words the grammar accepts as a `mod` name (they are path segments) that cannot name a module.
const NOT_MODULE_NAMES: [&str; 3] = ["self", "crate", "super"];

/// Reads and parses the module tree of `tree`. The modules come out sorted by path, the root first; the
/// diagnostics are the parser's and the tree's.
pub fn load_modules(tree: &SourceTree) -> (Crate, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    if !tree.files.contains_key(ROOT_FILE) {
        diags.push(Diagnostic::error(
            "mod-not-found",
            format!("the crate root `{ROOT_FILE}` is missing"),
            ROOT_FILE,
            1,
            1,
            1,
        ));
        return (Crate::default(), diags);
    }
    let mut loaded: BTreeMap<Vec<String>, Module> = BTreeMap::new();
    let mut pending: Vec<(Vec<String>, String)> = vec![(Vec::new(), ROOT_FILE.to_string())];
    while let Some((path, file)) = pending.pop() {
        let text = tree.files.get(&file).map_or("", String::as_str);
        let (ast, mut parsed) = parse_file(&file, text);
        diags.append(&mut parsed);
        let dir: String = path.iter().map(|seg| format!("{seg}/")).collect();
        // A second `mod` of one name is `duplicate-def` (resolution); the module is read once.
        let mut declared: BTreeSet<&str> = BTreeSet::new();
        for item in &ast.items {
            let ItemKind::Mod(name) = &item.kind else {
                continue;
            };
            let at = item.span;
            if NOT_MODULE_NAMES.contains(&name.as_str()) {
                diags.push(Diagnostic::error(
                    "syntax",
                    format!("`{name}` cannot name a module"),
                    &file,
                    at.line,
                    at.col,
                    at.len,
                ));
                continue;
            }
            if !declared.insert(name) {
                continue;
            }
            let flat = format!("{dir}{name}.qs");
            let nested = format!("{dir}{name}/mod.qs");
            let found = match (
                tree.files.contains_key(&flat),
                tree.files.contains_key(&nested),
            ) {
                (true, false) => flat,
                (false, true) => nested,
                (true, true) => {
                    diags.push(Diagnostic::error(
                        "mod-ambiguous",
                        format!("module `{name}` is both `{flat}` and `{nested}`"),
                        &file,
                        at.line,
                        at.col,
                        at.len,
                    ));
                    continue;
                }
                (false, false) => {
                    diags.push(Diagnostic::error(
                        "mod-not-found",
                        format!("module `{name}` has no file: expected `{flat}` or `{nested}`"),
                        &file,
                        at.line,
                        at.col,
                        at.len,
                    ));
                    continue;
                }
            };
            let mut child = path.clone();
            child.push(name.clone());
            pending.push((child, found));
        }
        loaded.insert(path.clone(), Module { path, file, ast });
    }
    (
        Crate {
            modules: loaded.into_values().collect(),
        },
        diags,
    )
}
