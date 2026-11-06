//! Water bodies: a connected pool whose water is one `volume`. Every member layer
//! below `top` is virtually full; the top layer shares `frac` evenly.

use crate::FULL;
use crate::grid::Dims;
use crate::state::{BodyId, Fnv};

pub(crate) const F: u64 = FULL as u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Body {
    pub id: BodyId,
    pub volume: u64,
    /// Highest member layer.
    pub top: u32,
    /// Member count per layer (len = dims.y).
    pub area: Vec<u32>,
    /// Members strictly below `top` (= cells - area[top]).
    pub below: u64,
    pub cells: u64,
    /// Inclusive bounding box of members, (x, y, z). Grows only.
    pub lo: [u32; 3],
    pub hi: [u32; 3],
    /// Sorted non-member, non-solid cells to wake when the level rises (spill edges, edit holes).
    pub shore: Vec<u32>,
    /// A refused expansion is not retried before this step.
    pub expand_retry: u32,
}

impl Body {
    pub fn empty(id: BodyId, dims: Dims) -> Self {
        Body {
            id,
            volume: 0,
            top: 0,
            area: vec![0; dims.y as usize],
            below: 0,
            cells: 0,
            lo: [u32::MAX; 3],
            hi: [0; 3],
            shore: Vec::new(),
            expand_retry: 0,
        }
    }
    pub fn top_area(&self) -> u64 {
        self.area[self.top as usize] as u64
    }
    pub fn capacity(&self) -> u64 {
        F * (self.below + self.top_area())
    }
    /// Water in the top layer.
    pub fn frac(&self) -> u64 {
        self.volume.saturating_sub(F * self.below)
    }
    pub fn room(&self) -> u64 {
        self.capacity().saturating_sub(self.volume)
    }
    /// The water a member at layer `y` holds, as a CA cell would see it.
    pub fn virtual_at(&self, y: u32) -> u16 {
        if y < self.top {
            FULL
        } else if y == self.top && self.top_area() > 0 {
            (self.frac() / self.top_area()) as u16
        } else {
            0
        }
    }
    /// Surface height in units.
    pub fn level(&self) -> u64 {
        self.top as u64 * F + self.virtual_at(self.top) as u64
    }
    pub fn byte_size(&self) -> usize {
        48 + 4 * self.area.len() + 4 * self.shore.len()
    }
    pub fn hash_into(&self, h: &mut Fnv) {
        h.u32(self.id);
        h.u64(self.volume);
        h.u32(self.top);
        for a in &self.area {
            h.u32(*a);
        }
        h.u64(self.below);
        h.u64(self.cells);
        for v in self.lo.iter().chain(&self.hi) {
            h.u32(*v);
        }
        for s in &self.shore {
            h.u32(*s);
        }
        h.u32(self.expand_retry);
    }
    pub(crate) fn grow_bbox(&mut self, (x, y, z): (u32, u32, u32)) {
        for (k, v) in [x, y, z].into_iter().enumerate() {
            self.lo[k] = self.lo[k].min(v);
            self.hi[k] = self.hi[k].max(v);
        }
    }
}
