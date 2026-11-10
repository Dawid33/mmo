//! The vertex streams water is meshed into (`stud/water.rs`, `stud/far.rs`):
//! triangle lists in world units, coloured [`super::WATER_COLOR`] with a
//! per-quad alpha ([`super::water_alpha`]).

use serde::{Deserialize, Serialize};

use super::WATER_COLOR;

/// Meshed water: triangle-list vertex streams, in the frame of the entity
/// that draws them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WaterMeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl WaterMeshData {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// One quad at corner `p` spanned by `u` then `v` (`u × v` is the
    /// normal, so it is front-facing from outside).
    pub(crate) fn quad(&mut self, p: [f32; 3], u: [f32; 3], v: [f32; 3], normal: [f32; 3], alpha: f32) {
        let base = self.positions.len() as u32;
        let add = |a: [f32; 3], b: [f32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
        for corner in [p, add(p, u), add(add(p, u), v), add(p, v)] {
            self.positions.push(corner);
            self.normals.push(normal);
            self.colors.push([WATER_COLOR[0], WATER_COLOR[1], WATER_COLOR[2], alpha]);
        }
        self.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}
