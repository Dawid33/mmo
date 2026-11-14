//! The mapping table: which of our blocks a Minecraft block becomes (`data/materials.ron`).
//!
//! Rule keys are matched against the Minecraft name without its `minecraft:` prefix: `name` is an exact
//! match, `*suffix` and `prefix*` are patterns. The most specific match wins (see [`match_score`]).

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

/// Bumped on any change to the table or the form rules that changes what a state becomes; recorded in
/// imported prefab files.
pub const MAPPING_VERSION: u32 = 1;

const BUILTIN: &str = include_str!("../data/materials.ron");

/// The reasons a [`crate::Class::Dropped`] can carry (they are `&'static str`).
const DROP_REASONS: &[&str] = &["liquid", "plant", "redstone", "fire", "web", "crystal", "egg", "portal"];

#[derive(Debug, Clone, Deserialize)]
pub struct MarkerRule {
    pub kind: String,
    /// A block placed together with the marker (a chest's body, a lamp's cube).
    #[serde(default)]
    pub solid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Raw {
    fallback: String,
    #[serde(default)]
    strip_prefixes: Vec<String>,
    #[serde(default)]
    aliases: BTreeMap<String, String>,
    materials: BTreeMap<String, String>,
    keep: BTreeMap<String, String>,
    dropped: BTreeMap<String, String>,
    markers: BTreeMap<String, MarkerRule>,
}

#[derive(Debug)]
pub struct Table {
    pub(crate) fallback: String,
    pub(crate) strip_prefixes: Vec<String>,
    pub(crate) aliases: BTreeMap<String, String>,
    pub(crate) materials: BTreeMap<String, String>,
    pub(crate) keep: BTreeMap<String, String>,
    pub(crate) dropped: BTreeMap<String, &'static str>,
    pub(crate) markers: BTreeMap<String, MarkerRule>,
}

impl Table {
    /// The table embedded in the crate, parsed once. Panics only if the embedded file is malformed.
    pub fn builtin() -> &'static Table {
        static TABLE: OnceLock<Table> = OnceLock::new();
        TABLE.get_or_init(|| {
            Table::from_ron(BUILTIN).unwrap_or_else(|e| panic!("data/materials.ron is malformed: {e}"))
        })
    }

    pub fn from_ron(src: &str) -> Result<Table, String> {
        let raw: Raw = ron::from_str(src).map_err(|e| e.to_string())?;
        let mut dropped = BTreeMap::new();
        for (k, why) in raw.dropped {
            let why = DROP_REASONS
                .iter()
                .copied()
                .find(|r| *r == why)
                .ok_or_else(|| format!("dropped rule {k:?}: unknown reason {why:?}"))?;
            dropped.insert(k, why);
        }
        Ok(Table {
            fallback: raw.fallback,
            strip_prefixes: raw.strip_prefixes,
            aliases: raw.aliases,
            materials: raw.materials,
            keep: raw.keep,
            dropped,
            markers: raw.markers,
        })
    }

    pub fn fallback(&self) -> &str {
        &self.fallback
    }

    /// Every block name of ours this table can produce (the fallback, every material, every marker solid,
    /// and the chest the form rules place).
    pub fn target_blocks(&self) -> BTreeSet<&str> {
        let mut out: BTreeSet<&str> = self.materials.values().map(String::as_str).collect();
        out.insert(&self.fallback);
        out.insert("chest");
        out.insert("snow");
        out.extend(self.markers.values().filter_map(|m| m.solid.as_deref()));
        out
    }
}

/// How specifically `key` matches `name`: 1000 = the exact name; 500 + len = a `prefix*` pattern (longer
/// prefix wins); len = a `*suffix` pattern (longer suffix wins). So exact beats any prefix, which beats any
/// suffix. When keep, dropped and marker rules tie on score, `classify` takes keep, then dropped, then marker.
pub(crate) fn match_score(key: &str, name: &str) -> Option<usize> {
    if let Some(suffix) = key.strip_prefix('*') {
        name.ends_with(suffix).then_some(suffix.len())
    } else if let Some(prefix) = key.strip_suffix('*') {
        name.starts_with(prefix).then_some(500 + prefix.len())
    } else {
        (key == name).then_some(1000)
    }
}

/// The best-matching rule of one rule map.
pub(crate) fn best<'a, V>(rules: &'a BTreeMap<String, V>, name: &str) -> Option<(usize, &'a V)> {
    rules
        .iter()
        .filter_map(|(k, v)| match_score(k, name).map(|s| (s, v)))
        .max_by_key(|(s, _)| *s)
}
