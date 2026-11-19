//! Seam-translation invariance: the acceptance oracle of seam-transparent
//! regions. A [`Scenario`] is played on a uniform floor (or its own ground,
//! [`Scenario::floor`]) three ways — centred
//! in a region, across a straight seam ([`Placement::Seam`]) and through a
//! four-region corner ([`Placement::Corner`]) — and must end in the same
//! outcome in the WORLD frame, measured from the placement's origin.
//!
//! Deviation from the plan's `world_state_hash` (controller ruling R18):
//! bit-exact equality across placements is unachievable for float physics,
//! since rapier rounds by the local coordinate magnitude, which necessarily
//! differs between a centred and a seam placement. A placement's outcome is
//! a [`WorldSnapshot`] instead, compared by [`compare`]: DISCRETE state
//! exactly (the set of `EntityId`s and kinds, each entity's feet tile in
//! world blocks, whether its body sleeps, its walk goal, a client's mode and
//! inventory, changed terrain chunks, item piles, chests) and CONTINUOUS
//! state (world poses, rotations, velocities) within named [`Tolerances`]
//! (a scenario may set its own: [`Scenario::lockstep_tolerances`]).
//! Ownership is checked inside [`snapshot`] instead of compared: every
//! entity must be owned by the region its world position lies in.
//!
//! Scope of the snapshot: entities the scenario plays with. Slimes are left
//! out — the spawner places them by region-local rules, so they do not move
//! with the placement (they are scenery here, not the scenario).
//!
//! Every placement starts in the spawn region (the rig's client spawns
//! there): a scenario starts on the +x/+z side of the origin and moves toward
//! −x/−z through it, so its motion crosses the seam / the corner.

use game::{
    Chunk, ColumnCoords, Column,
    RegionCoords, SubChunk, BLOCK_VOXELS,
    REGION_CHUNKS,
};

/// A one-block floor under every chunk column of every region: the same
/// ground in the world frame wherever a scenario is placed.
pub fn uniform_floor(_rc: RegionCoords) -> Vec<(ColumnCoords, Column)> {
    let air = game::Block::new(game::BlockId::AIR);
    let mut out = Vec::new();
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(BLOCK_VOXELS as u32)));
            out.push((ColumnCoords::new(x, z), Column::new(0, air, air, vec![sub])));
        }
    }
    out
}

