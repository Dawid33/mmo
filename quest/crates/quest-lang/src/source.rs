//! The program's source files.

use std::collections::BTreeMap;

/// A program's files by path: `"main.qs"`, `"npc/mod.qs"`, … → text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceTree {
    pub files: BTreeMap<String, String>,
}
