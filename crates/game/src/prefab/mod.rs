//! Prefabs: saved, named groups of LEGO parts that enter the world as a unit.
//! Design: `docs/superpowers/specs/2026-11-11-prefab-system-design.md`.
//!
//! Two forms of the same thing:
//! - the **file form** ([`PrefabFile`], `file.rs`): what is on disk and in an export
//!   string. Parts by LDraw number, natural materials by name, so a file survives
//!   table growth.
//! - the **resolved form** ([`ResolvedPrefab`], `resolve.rs`): what the sim, the wire
//!   and worldgen use. Table ids, validated, capped. A resolved prefab that arrives
//!   from outside (a register command) is untrusted until [`ResolvedPrefab::validate`]
//!   passes.

mod builtin;
mod capture;
mod codec;
mod command;
mod file;
mod layout;
#[cfg(not(target_arch = "wasm32"))]
mod library;
mod resolve;
mod seed;

pub use builtin::*;
pub use capture::*;
pub use codec::*;
pub use command::*;
pub use file::*;
pub use layout::*;
#[cfg(not(target_arch = "wasm32"))]
pub use library::*;
pub use resolve::*;
pub use seed::*;

use serde::{Deserialize, Serialize};

/// Most parts one prefab may hold.
pub const PREFAB_MAX_PARTS: usize = 4096;
/// Most boxes (solid fills plus carve boxes) one prefab may hold.
pub const PREFAB_MAX_BOXES: usize = 4096;
/// Most cells a prefab's solid boxes may fill in total, and most its carve boxes may
/// clear in total: the volume of the bounds. Bounds what one stamp can be made to tile.
pub const PREFAB_MAX_BOX_CELLS: u64 =
    (2 * PREFAB_HALF_STUDS as u64) * (2 * PREFAB_HALF_STUDS as u64) * (PREFAB_MAX_PLATE - PREFAB_MIN_PLATE) as u64;
/// Most markers one prefab may hold.
pub const PREFAB_MAX_MARKERS: usize = 256;
/// Longest marker kind, in bytes.
pub const PREFAB_MARKER_KIND_MAX: usize = 32;
/// A Build prefab's cells lie within `-PREFAB_HALF_STUDS..PREFAB_HALF_STUDS` of the
/// anchor in x and z, so a prefab always fits inside one region of the smallest size.
pub const PREFAB_HALF_STUDS: i32 = 48;
/// Lowest plate (inclusive) a Build prefab's cells may reach, relative to the anchor.
pub const PREFAB_MIN_PLATE: i32 = -90;
/// One past the highest plate a Build prefab's cells may reach, relative to the anchor.
pub const PREFAB_MAX_PLATE: i32 = 180;

/// A prefab's content address: the `StateHash` of its resolved form.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, state_hash::StateHash,
)]
pub struct PrefabKey(pub u32);

/// How sim state refers to a built-in prefab: the crc32 of its name.
pub fn prefab_name_key(name: &str) -> u32 {
    crc32fast::hash(name.as_bytes())
}
