//! The halo: each region holds a read-only mirror of what its neighbours own
//! within [`HALO_BLOCKS`] of their shared edges (spec Part 3, P5) — terrain
//! (the stud halo, `stud_halo.rs`), entities, item piles and chests — so a body
//! walking across a seam has a floor before its ownership flips, and the eye
//! ray and tile queries see past the edge. This file holds the band geometry,
//! the [`HaloUpdate`] message and its application.
//!
//! The halo is ordinary rollback state, changed only by
//! [`GameEventKind::HaloUpdate`](crate::GameEventKind::HaloUpdate) events:
//! - a region's tick (and the slot's events after it) that changed a chunk
//!   within the band sends [`HaloUpdate`]s to the neighbours that mirror it,
//!   stamped like departures (`RegionOutput::HaloDeltas`), so
//!   they land in slot T+1 and the client replays them in that slot;
//! - a starting region is seeded before its first tick: from each
//!   neighbour's running state (`RegionInput::HaloRequest`), its parked /
//!   cached terrain, or worldgen ([`HaloUpdate::replace`] seeds).
//!
//! Read-only: edits stay with the owner (a raycast may hit a halo brick; an
//! edit of it is an intent to the owner, `intents.rs`).
//!
//! The ENTITY halo rides the same stamped channel: each tick the owner sends
//! every owned entity within [`HALO_UNITS`] of an edge to that neighbour
//! ([`crate::HaloEntityDelta`], `RegionOutput::HaloEntities`), which upserts
//! a kinematic mirror (`GameData::halo_entities`). The owner remembers what it
//! mirrored where (`GameData::halo_sent`) and ends a mirror explicitly with
//! [`crate::HaloEntityGone`] — the entity left the band, despawned, was
//! parked, crossed into another region, or the owner stopped. A crossing into
//! the receiver itself sends none: the arrival upgrades the mirror in place.
//! A seed ([`HaloUpdate::replace`]) also drops every entity mirror of its
//! source, so a (re)started region never keeps a stale one; the source's next
//! tick re-sends the entities still in the band.
//!
//! The PILE halo rides the same updates
//! ([`HaloUpdate::piles`]): each own pile within [`HALO_BLOCKS`] of an edge
//! (`item_piles::pile_halo_targets`) is mirrored, read-only, into the
//! neighbours' `GameData::halo_piles` — written piles as deltas, all of them
//! on a seed (which drops the source's other mirrors). Pickup reach and a
//! deposit's merge search read them; takes and deposits are intents. The
//! CHEST halo ([`HaloUpdate::containers`], `containers.rs`) rides with it:
//! same band, same dirty cells.

use crate::{RegionCoords, Rollback, CHUNK_BLOCKS, REGION_CHUNKS};

/// Width of the halo past each edge, in blocks: at least every interaction
/// reach that may look past an edge (the asserts below), plus a sprint step
/// and a body's half-extent (spec P5's width rule).
pub const HALO_BLOCKS: i32 = 8;

/// [`HALO_BLOCKS`] in world units: an owned entity this close to an edge is
/// mirrored into that neighbour's entity halo (`crate::halo_offsets`).
pub const HALO_UNITS: f32 = (HALO_BLOCKS * crate::BLOCK_UNITS as i32) as f32;

/// How far (units) a halo mirror may be DRIVEN to its next pose in one
/// step; a jump past it TELEPORTS the mirror instead (`apply_halo_entity_safe`).
/// A driven mirror moves with the velocity that covers the jump in one tick,
/// and hands that velocity to any dynamic body it touches — right for motion,
/// a fling for a jump (an in-band teleport, or a mirror resumed after missed
/// ticks under jitter, whose updates land several ticks at once). The fastest
/// legit per-tick motion of a moving entity is a fly-sprint,
/// `FLY_SPRINT_SPEED` × `TICK_DT` = 1.08 blocks; ×1.5 margin = 1.62 blocks
/// (25.9 units), so every walk, sprint and flight step is driven. A fall
/// faster than ~32 m/s (≈0.7 s of free fall; terminal is 78.4) is teleported:
/// it only loses its push, the behaviour before mirrors were driven.
pub const MIRROR_SNAP_DISTANCE_UNITS: f32 = 1.5
    * crate::movement::FLY_SPRINT_SPEED
    * crate::movement::UNITS_PER_METER
    * crate::movement::TICK_DT;

/// The halo width in whole chunks: chunk rows mirrored along each edge.
pub const HALO_CHUNKS: i32 = (HALO_BLOCKS + CHUNK_BLOCKS as i32 - 1) / CHUNK_BLOCKS as i32;

/// `user_data` of a halo chunk's body: no entity owns it. Halo bodies are
/// fixed, so the physics controller's active-body walk (the one reader of
/// body `user_data`) never meets one.
pub const HALO_BODY_USER_DATA: u128 = u128::MAX;

/// Whole blocks covering a reach of `r` blocks.
const fn ceil_blocks(r: f32) -> i32 {
    let whole = r as i32;
    if (whole as f32) < r { whole + 1 } else { whole }
}

// Every interaction reach that can look past an edge fits inside the halo.
// A new reach constant must be added here.
const _: () = assert!(ceil_blocks(crate::edit::REACH) <= HALO_BLOCKS, "edit::REACH exceeds the halo");
const _: () = assert!(crate::CONTAINER_REACH_BLOCKS <= HALO_BLOCKS, "CONTAINER_REACH_BLOCKS exceeds the halo");
const _: () = assert!(crate::PICKUP_REACH_H_BLOCKS <= HALO_BLOCKS, "PICKUP_REACH_H_BLOCKS exceeds the halo");
const _: () =
    assert!(ceil_blocks(crate::THROW_DISTANCE_BLOCKS) <= HALO_BLOCKS, "THROW_DISTANCE_BLOCKS exceeds the halo");

/// One region's halo chunks for one neighbour, as a
/// [`crate::GameEventKind::HaloUpdate`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HaloUpdate {
    /// The region the chunks belong to.
    pub from: RegionCoords,
    /// A seed: it lists ALL of `from`'s piles, chests and non-derivable stud
    /// chunks in the receiver's band; anything else of `from` held there is
    /// dropped, and so is every entity mirror of `from` (its next tick
    /// re-sends those in the band).
    pub replace: bool,
    /// `from`'s piles in the receiver's pile band, by world cell: the new
    /// pile, `None` = gone. On a seed, all of them (the source's other
    /// mirrors are dropped); on a delta, the cells written.
    pub piles: Vec<(crate::WorldBlockPos, Option<crate::ItemPile>)>,
    /// `from`'s chests in the receiver's pile band, by world cell, as
    /// `piles` (`containers.rs`). A delta carries every written band cell in
    /// both lists (a cell's current pile and chest, either may be `None`).
    pub containers: Vec<(crate::WorldBlockPos, Option<crate::Container>)>,
    /// `from`'s stud chunks (`stud_halo.rs`; `chunk.y` is the stud layer). On a seed:
    /// only the band chunks its recipes cannot give back — the receiver derives the
    /// pristine chunk of every surface layer itself and drops any other chunk of
    /// `from` not listed. On a delta: the band chunks written, whole.
    pub studs: Vec<crate::StudHaloDelta>,
    /// `from`'s stud ground recipes for the band's chunk columns, by world chunk
    /// column (`y` 0) — on seeds; empty on deltas (recipes never change).
    pub stud_ground: Vec<(crate::WorldChunkPos, crate::StudColumnGround)>,
}

impl HaloUpdate {
    /// Same source and the same chunks and pile cells (reconcile's identity
    /// match; the contents may differ).
    pub fn same_chunks(&self, other: &Self) -> bool {
        self.from == other.from
            && self.replace == other.replace
            && self.piles.len() == other.piles.len()
            && self.piles.iter().zip(&other.piles).all(|(a, b)| a.0 == b.0)
            && self.containers.len() == other.containers.len()
            && self.containers.iter().zip(&other.containers).all(|(a, b)| a.0 == b.0)
            && self.studs.len() == other.studs.len()
            && self.studs.iter().zip(&other.studs).all(|(a, b)| a.chunk == b.chunk)
            && self.stud_ground.len() == other.stud_ground.len()
            && self.stud_ground.iter().zip(&other.stud_ground).all(|(a, b)| a.0 == b.0)
    }
}

/// Offset toward the neighbour a local chunk column index lies against:
/// -1 / +1 within `HALO_CHUNKS` of the low / high edge, else 0.
fn edge_side(c: i32) -> i32 {
    let n = REGION_CHUNKS as i32;
    if c < HALO_CHUNKS {
        -1
    } else if c >= n - HALO_CHUNKS {
        1
    } else {
        0
    }
}

/// The neighbour offsets that mirror local chunk column `[cx, cz]` of a
/// region (none for an interior column; up to 3 at a corner).
pub fn halo_targets(cx: i32, cz: i32) -> Vec<(i32, i32)> {
    let (sx, sz) = (edge_side(cx), edge_side(cz));
    let mut out = Vec::with_capacity(3);
    for dx in [0, sx] {
        for dz in [0, sz] {
            if (dx, dz) != (0, 0) && !out.contains(&(dx, dz)) {
                out.push((dx, dz));
            }
        }
    }
    out
}

/// Whether local chunk column `[cx, cz]` (in a region's chunk frame) lies in
/// that region's halo: outside it, within `HALO_CHUNKS` of an edge.
pub fn in_halo_band(cx: i32, cz: i32) -> bool {
    let n = REGION_CHUNKS as i32;
    let inside = |c: i32| (0..n).contains(&c);
    let near = |c: i32| (-HALO_CHUNKS..n + HALO_CHUNKS).contains(&c);
    near(cx) && near(cz) && !(inside(cx) && inside(cz))
}

/// The neighbour offset from `from` to `to`, when they are neighbours.
fn neighbour_offset(from: RegionCoords, to: RegionCoords) -> Option<(i32, i32)> {
    let (dx, dz) = (to.x - from.x, to.z - from.z);
    ((dx, dz) != (0, 0) && dx.abs() <= 1 && dz.abs() <= 1).then_some((dx, dz))
}

/// A neighbour's terrain as a seed for region `to`'s halo: the edge recipes and
/// the edge chunks they cannot give back (`StudEdge::seed_parts`). `None`
/// when `from` and `to` are not neighbours.
pub fn halo_seed_from_snapshot(from: RegionCoords, to: RegionCoords, snap: &crate::TerrainSnapshot) -> Option<HaloUpdate> {
    let (studs, stud_ground) = snap.stud.seed_parts(from, neighbour_offset(from, to)?);
    Some(HaloUpdate { from, replace: true, piles: Vec::new(), containers: Vec::new(), studs, stud_ground })
}

/// The chunk columns `xs × zs` of `from` (its own frame) that neighbour `to`
/// mirrors: the band along their shared edge, or the corner block for a
/// diagonal neighbour. Empty ranges when they are not neighbours.
pub fn facing_columns(from: RegionCoords, to: RegionCoords) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    let n = REGION_CHUNKS;
    let h = (HALO_CHUNKS as usize).min(n);
    let span = |d: i32| match d {
        -1 => 0..h,
        1 => n - h..n,
        _ => 0..n,
    };
    match neighbour_offset(from, to) {
        Some((dx, dz)) => (span(dx), span(dz)),
        None => (0..0, 0..0),
    }
}

/// [`facing_columns`] cut into square windows at most `HALO_CHUNKS` on a
/// side, in a fixed order: worldgen's patch is square on the longer side, so
/// a whole edge strip in one window would cost a whole region's patch.
pub fn facing_windows(from: RegionCoords, to: RegionCoords) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let (xs, zs) = facing_columns(from, to);
    let h = (HALO_CHUNKS as usize).max(1);
    let cut = |r: std::ops::Range<usize>| -> Vec<std::ops::Range<usize>> {
        r.clone().step_by(h).map(|s| s..(s + h).min(r.end)).collect()
    };
    let zcuts = cut(zs);
    cut(xs).into_iter().flat_map(|x| zcuts.iter().map(move |z| (x.clone(), z.clone()))).collect()
}

/// The 8 neighbours of `rc`, in a fixed order.
pub fn neighbours(rc: RegionCoords) -> impl Iterator<Item = RegionCoords> {
    [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)]
        .into_iter()
        .map(move |(dx, dz)| RegionCoords::new(rc.x + dx, rc.z + dz))
}

thread_local! {
    /// Depenetration lifts on this thread (see [`depenetration_lifts`]).
    static LIFTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Notes one non-zero `movement::depenetration_lift`.
pub(crate) fn note_lift() {
    LIFTS.with(|c| c.set(c.get() + 1));
}

/// How many bodies `movement::depenetration_lift` lifted on this thread: a
/// test probe that a seam crossing no longer sinks into the next region.
#[doc(hidden)]
pub fn depenetration_lifts() -> u64 {
    LIFTS.with(|c| c.get())
}

impl Rollback {
    /// What neighbour `to` mirrors of this region (its edge terrain, piles and
    /// chests), as a seed.
    pub fn halo_seed_for(&self, to: RegionCoords) -> Option<HaloUpdate> {
        let from = *self.data.region;
        let (studs, stud_ground) = self.stud_edge().seed_parts(from, neighbour_offset(from, to)?);
        Some(HaloUpdate {
            from,
            replace: true,
            piles: self.pile_seed_for(to),
            containers: self.container_seed_for(to),
            studs,
            stud_ground,
        })
    }

    /// The halo updates for the stud chunks (`studs`) and own pile cells
    /// (`piles`) written since the last call (any order, repeats allowed):
    /// one update per neighbour that mirrors any of them, with each chunk's
    /// and pile's current content.
    pub(crate) fn halo_updates_for(
        &self,
        piles: &std::collections::BTreeSet<crate::WorldBlockPos>,
        studs: &std::collections::BTreeSet<crate::StudChunkCoords>,
    ) -> Vec<(HaloUpdate, RegionCoords)> {
        type Pending =
            (Vec<(crate::WorldBlockPos, Option<crate::ItemPile>)>, Vec<(crate::WorldBlockPos, Option<crate::Container>)>);
        let from = *self.data.region;
        let mut by_target: std::collections::BTreeMap<(i32, i32), Pending> = Default::default();
        for &p in piles {
            let l = crate::item_piles::local_cell(from, p);
            let pile = self.data.item_piles.get(&p).cloned();
            let chest = self.data.containers().get(&p).cloned();
            for t in crate::item_piles::pile_halo_targets(l[0], l[2]) {
                let e = by_target.entry(t).or_default();
                e.0.push((p, pile.clone()));
                e.1.push((p, chest.clone()));
            }
        }
        let mut stud_deltas = self.stud_halo_deltas_for(studs);
        for t in stud_deltas.keys() {
            by_target.entry(*t).or_default();
        }
        by_target
            .into_iter()
            .map(|((dx, dz), (piles, containers))| {
                let studs = stud_deltas.remove(&(dx, dz)).unwrap_or_default();
                let update = HaloUpdate { from, replace: false, piles, containers, studs, stud_ground: Vec::new() };
                (update, RegionCoords::new(from.x + dx, from.z + dz))
            })
            .collect()
    }

    /// Applies a [`HaloUpdate`], undo-safely: a seed first drops the source's
    /// entity, pile and chest mirrors it does not list; then the piles and
    /// chests, then the stud halo ([`Self::apply_stud_halo_update_safe`]).
    pub(crate) fn apply_halo_update_safe(&mut self, u: &HaloUpdate) {
        if u.replace {
            self.drop_halo_source_safe(u.from);
            let listed: std::collections::BTreeSet<crate::WorldBlockPos> = u.piles.iter().map(|(p, _)| *p).collect();
            let stale: Vec<_> = self
                .data
                .halo_piles
                .keys()
                .filter(|p| p.to_local().0 == u.from && !listed.contains(p))
                .map(|p| (*p, None))
                .collect();
            self.write_halo_piles_safe(stale);
            let listed: std::collections::BTreeSet<crate::WorldBlockPos> =
                u.containers.iter().map(|(p, _)| *p).collect();
            let stale: Vec<_> = self
                .data
                .halo_containers()
                .keys()
                .filter(|p| p.to_local().0 == u.from && !listed.contains(p))
                .map(|p| (*p, None))
                .collect();
            self.write_halo_containers_safe(stale);
        }
        let region = *self.data.region;
        let piles: Vec<_> = u
            .piles
            .iter()
            .filter(|(p, _)| {
                let l = crate::item_piles::local_cell(region, *p);
                let ok = p.to_local().0 == u.from && crate::item_piles::in_pile_halo_band(l[0], l[2]);
                if !ok {
                    log::warn!("halo update from {:?}: pile {p:?} outside its band; ignored", u.from);
                }
                ok
            })
            .cloned()
            .collect();
        self.write_halo_piles_safe(piles);
        let containers: Vec<_> = u
            .containers
            .iter()
            .filter(|(p, _)| {
                let l = crate::item_piles::local_cell(region, *p);
                let ok = p.to_local().0 == u.from && crate::item_piles::in_pile_halo_band(l[0], l[2]);
                if !ok {
                    log::warn!("halo update from {:?}: chest {p:?} outside its band; ignored", u.from);
                }
                ok
            })
            .cloned()
            .collect();
        self.write_halo_containers_safe(containers);
        self.apply_stud_halo_update_safe(u);
    }
}
