//! The pulled (mesh-free) chunk render path: per-quad records in storage
//! buffers, drawn by custom phase items. Render-world half arrives in later
//! tasks; this module starts with the main-world component + toggle.

use std::sync::Arc;

use bevy::prelude::*;

use super::meshing::records::QuadRecord;

/// Default for the old-vs-pull render path toggle. Flipped to `true` once
/// visual parity is confirmed (Task 8); kept a named constant so it is
/// findable and overridable in tests either way.
pub const DEFAULT_PULL_PATH: bool = false;

#[derive(Resource)]
pub struct RenderPath {
    pub pull: bool,
}

impl Default for RenderPath {
    fn default() -> Self {
        Self { pull: DEFAULT_PULL_PATH }
    }
}

/// The pull-path replacement for `Mesh3d`: every visible greedy quad of one
/// chunk (or LOD cell), as GPU-ready records. `Arc` so render-world extraction
/// clones the handle, not the data. Requires `Visibility` because without
/// `Mesh3d` nothing else inserts it, and cave-culling writes `&mut Visibility`.
#[derive(Component, Clone)]
#[require(Visibility)]
pub struct ChunkQuads(pub Arc<Vec<QuadRecord>>);
