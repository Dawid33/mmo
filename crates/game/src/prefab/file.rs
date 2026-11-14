//! The on-disk form of a prefab: plain data, resolved into block ids elsewhere.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::resolve::Problem;
use super::Shape;

pub const PREFAB_FORMAT: u16 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabFile {
    pub format: u16,
    pub name: String,
    #[serde(default)]
    pub meta: PrefabMeta,
    /// Blocks; cells are `0..size` on each axis.
    pub size: [u32; 3],
    /// A cell coordinate (may lie outside the box) that lands on the placement position.
    #[serde(default)]
    pub anchor: [i32; 3],
    /// Block names.
    #[serde(default)]
    pub palette: Vec<String>,
    #[serde(default)]
    pub boxes: Vec<FileBox>,
    #[serde(default)]
    pub shaped: Vec<FileShaped>,
    #[serde(default)]
    pub carve: Vec<FileCarve>,
    #[serde(default)]
    pub markers: Vec<Marker>,
    #[serde(default)]
    pub ground: GroundFit,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefabMeta {
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub licence: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub import: Option<ImportInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportInfo {
    #[serde(default)]
    pub source_hash: String,
    #[serde(default)]
    pub source_box: Option<([i32; 3], [i32; 3])>,
    #[serde(default)]
    pub data_version: i32,
    #[serde(default)]
    pub mapping_version: u32,
}

/// `mat` is a palette index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileBox {
    pub min: [i32; 3],
    pub size: [u32; 3],
    pub mat: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileShaped {
    pub at: [i32; 3],
    pub shape: FileShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileShape {
    Named { shape: Shape, mat: u16 },
    /// `(min, exclusive max, index into mats)`.
    Cuboids { cuboids: Vec<([u8; 3], [u8; 3], u8)>, mats: Vec<u16> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCarve {
    pub min: [i32; 3],
    pub size: [u32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub at: [i32; 3],
    pub kind: String,
    #[serde(default)]
    pub turn: u8,
    #[serde(default)]
    pub props: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroundFit {
    #[default]
    Flat,
    Follow,
    Buried,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefabError {
    Syntax(String),
    NotAPrefab,
    TooLarge,
    Corrupt,
    Problems(Vec<Problem>),
    Io(String),
}

impl fmt::Display for PrefabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PrefabError::Syntax(s) => write!(f, "prefab syntax error: {s}"),
            PrefabError::NotAPrefab => write!(f, "not a prefab file"),
            PrefabError::TooLarge => write!(f, "prefab file too large"),
            PrefabError::Corrupt => write!(f, "prefab file is corrupt"),
            PrefabError::Problems(p) => write!(f, "prefab has {} problem(s): {}", p.len(), p.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("; ")),
            PrefabError::Io(s) => write!(f, "prefab i/o error: {s}"),
        }
    }
}

impl std::error::Error for PrefabError {}
