//! Region-owned lifecycle state for generated trees.

use std::collections::BTreeMap;

use crate::{BlockId, Tick};

/// Compact immutable data needed to reproduce a tree at any growth stage.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct TreeGenome {
    pub id: u32,
    pub species: u8,
    pub seed: u64,
    /// Region-local block at the lowest visible trunk layer.
    pub base: [i32; 3],
    pub initial_age: u16,
    pub log: BlockId,
    pub branch: BlockId,
    pub leaves: BlockId,
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub enum TreeState {
    #[default]
    Standing,
    Regrowing {
        next_stage: u8,
        next_tick: Tick,
        /// `Some(age)` is a one-stage repair of a branch-cut standing tree,
        /// rather than the sapling-to-mature sequence.
        repair_age: Option<u16>,
    },
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    state_hash::StateHash,
)]
pub struct TreeLife {
    pub genome: TreeGenome,
    /// Every root emitted at genesis, immutable after generation.
    pub original_roots: Vec<[i32; 3]>,
    /// Roots still present in terrain, canonical position order.
    pub surviving_roots: Vec<[i32; 3]>,
    pub state: TreeState,
}

/// Genesis-only metadata attached to a `Column` before it becomes terrain.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TreeLifeSeed {
    pub life: TreeLife,
}

pub type RegionTrees = BTreeMap<u32, TreeLife>;

pub fn standing_life(seed: TreeLifeSeed) -> TreeLife {
    TreeLife {
        state: TreeState::Standing,
        ..seed.life
    }
}
