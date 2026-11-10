//! Far-terrain build execution. `WorldManager` never blocks on a build: it
//! submits keys to a [`LodBuilder`] and polls finished tiles back.
//! [`InlineStudLodBuilder`] computes synchronously (wasm/tests); the native
//! server wraps a small thread pool (crates/server/src/lod_workers.rs).

use crate::lod::LodKey;
use crate::stud_lod::StudLodTile;

/// A finished far-terrain build.
pub type StudLodResult = (LodKey, StudLodTile);
/// A key's stud far terrain from the recipe (`worldgen::stud_lod_tile`).
pub type StudLodGenerator = Box<dyn Fn(&LodKey) -> StudLodTile + Send>;

pub trait LodBuilder: Send {
    /// Build `key`'s far terrain; its tile comes back through [`poll`](Self::poll).
    fn submit(&mut self, key: LodKey);

    /// The next finished tile, if one is ready (never blocks).
    fn poll(&mut self) -> Option<StudLodResult>;
}

/// Synchronous builder (tests, the in-page server): builds on submit.
pub struct InlineStudLodBuilder {
    pub tile: StudLodGenerator,
    done: std::collections::VecDeque<StudLodResult>,
}

impl InlineStudLodBuilder {
    pub fn new(tile: StudLodGenerator) -> Self {
        Self { tile, done: Default::default() }
    }
}

impl LodBuilder for InlineStudLodBuilder {
    fn submit(&mut self, key: LodKey) {
        self.done.push_back((key, (self.tile)(&key)));
    }

    fn poll(&mut self) -> Option<StudLodResult> {
        self.done.pop_front()
    }
}

/// For worlds with no far terrain (the toy test world): every key is dropped.
#[derive(Debug, Default)]
pub struct NullLodBuilder {
    logged: bool,
}

impl LodBuilder for NullLodBuilder {
    fn submit(&mut self, _key: LodKey) {
        if !std::mem::replace(&mut self.logged, true) {
            log::info!("[server] lod: none for this world");
        }
    }

    fn poll(&mut self) -> Option<StudLodResult> {
        None
    }
}
