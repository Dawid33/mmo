//! Wire form of a region join snapshot (`ServerPacket::Region`): the `Rollback` as
//! serde writes it, minus what the receiver derives by itself.
//!
//! * **Bricks**: a stud chunk (own or halo) goes as a diff against its recipe's
//!   `pristine_chunk` when the diff has fewer bricks than the chunk (trees are
//!   additions, an untouched chunk is an empty diff). The recipes travel in the same
//!   snapshot (`stud_ground` / `stud_halo_ground`).
//! * **Colliders**: a stud chunk's `Voxels` collider shape is replaced by a shared
//!   placeholder when the sender has checked that a fresh from-bricks build
//!   (`stud_collider_shapes`) serializes to the very bytes of the shape it holds. An
//!   incrementally edited shape has the same content but another chunk / BVH layout
//!   (which orders contacts, and is not in the state hash), fails the check and is
//!   sent whole. Only the shape is swapped (`Collider::swap_shape_raw`): the collider
//!   and body arenas, handles, change flags and broad phase travel as they are.
//!
//! Hull shapes, disabled spare colliders and every non-stud collider are sent whole.
//! The store's rows are not touched: this is the wire only.
//!
//! Cost: the receiver pays a `Voxels` build per chunk (what genesis pays), spread over
//! threads on native. The sender's check is memoised per shape allocation
//! ([`note_fresh_shape`]): shapes built by `stud_colliders_tagged` are fresh by
//! construction, anything else is checked once. A server whose clients are on loopback
//! keeps the shapes on the wire instead ([`set_snapshot_sends_colliders`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use rapier3d::prelude::{ColliderHandle, RigidBodyHandle};
use serde::{Deserialize, Serialize};

use crate::parry::shape::{Shape, SharedShape};
use crate::stud::{StudBrick, StudChunk};
use crate::stud_collider::stud_collider_shapes;
use crate::stud_ground::StudColumnGround;
use crate::{ColumnCoords, EntityKey, Rollback, StudChunkCoords, WorldChunkPos};

/// A chunk as its pristine chunk minus `removed` (min cells) plus `added`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StudChunkDiff {
    removed: Vec<[u8; 3]>,
    added: Vec<StudBrick>,
}

impl StudChunkDiff {
    /// The diff of `chunk` against `pristine`, when it carries fewer bricks than `chunk`.
    fn smaller(chunk: &StudChunk, pristine: &StudChunk) -> Option<Self> {
        let removed: Vec<_> =
            pristine.bricks.iter().filter(|(k, b)| chunk.bricks.get(*k) != Some(*b)).map(|(k, _)| *k).collect();
        let added: Vec<_> =
            chunk.bricks.iter().filter(|(k, b)| pristine.bricks.get(*k) != Some(*b)).map(|(_, b)| *b).collect();
        (removed.len() + added.len() < chunk.bricks.len()).then_some(Self { removed, added })
    }

    fn apply(self, mut base: StudChunk) -> StudChunk {
        for k in &self.removed {
            base.bricks.remove(k);
        }
        for b in self.added {
            base.bricks.insert(b.min, b);
        }
        base
    }
}

#[derive(Serialize)]
struct WireOut<'a> {
    slim: &'a Rollback,
    own: Vec<(StudChunkCoords, StudChunkDiff)>,
    halo: Vec<(WorldChunkPos, StudChunkDiff)>,
    shapes: Vec<ColliderHandle>,
}

#[derive(Deserialize)]
struct WireIn {
    slim: Rollback,
    own: Vec<(StudChunkCoords, StudChunkDiff)>,
    halo: Vec<(WorldChunkPos, StudChunkDiff)>,
    shapes: Vec<ColliderHandle>,
}

fn placeholder() -> SharedShape {
    SharedShape::ball(0.0)
}

static SEND_COLLIDERS: AtomicBool = AtomicBool::new(false);

/// `true`: join snapshots carry every collider shape (more bytes, no rebuild on
/// receipt) — for a server whose clients are on loopback (single player). Process-wide;
/// the default is `false`. Receivers take either form.
pub fn set_snapshot_sends_colliders(send: bool) {
    SEND_COLLIDERS.store(send, Ordering::Relaxed);
}

/// Shape allocations whose layout is known to be (`true`) or not to be (`false`) a
/// fresh `stud_collider_shapes` build of their content. Not state: a memo of a pure
/// check. The `Weak` pins the allocation, so an address never names another shape, and
/// makes `SharedShape::make_mut` copy on the next edit, so a verdict never outlives the
/// content it was given for.
static SHAPE_VERDICTS: Mutex<BTreeMap<usize, (Weak<dyn Shape>, bool)>> = Mutex::new(BTreeMap::new());
/// Dead entries are dropped whenever the memo has grown by this many since the last sweep.
const VERDICT_SWEEP_EVERY: usize = 4096;
static VERDICT_SWEEP_AT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(VERDICT_SWEEP_EVERY);

fn shape_addr(shape: &SharedShape) -> usize {
    Arc::as_ptr(&shape.0) as *const () as usize
}

fn note_shape(shape: &SharedShape, fresh: bool) {
    let mut memo = SHAPE_VERDICTS.lock().unwrap_or_else(|e| e.into_inner());
    memo.insert(shape_addr(shape), (Arc::downgrade(&shape.0), fresh));
    if memo.len() >= VERDICT_SWEEP_AT.load(Ordering::Relaxed) {
        memo.retain(|_, (w, _)| w.strong_count() > 0);
        VERDICT_SWEEP_AT.store(memo.len() + VERDICT_SWEEP_EVERY, Ordering::Relaxed);
    }
}

/// Records that `shape` was just built by `stud_collider_shapes`.
pub(crate) fn note_fresh_shape(shape: &SharedShape) {
    if shape.as_voxels().is_some() {
        note_shape(shape, true);
    }
}

fn shape_verdict(shape: &SharedShape) -> Option<bool> {
    let memo = SHAPE_VERDICTS.lock().unwrap_or_else(|e| e.into_inner());
    memo.get(&shape_addr(shape)).map(|(_, fresh)| *fresh)
}

/// `f` over `items`, in order; on native spread over up to [`WIRE_THREADS`] threads.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(WIRE_THREADS);
        if threads > 1 && items.len() >= 2 * threads {
            let per = items.len().div_ceil(threads);
            let f = &f;
            return std::thread::scope(|s| {
                let handles: Vec<_> = items.chunks(per).map(|part| s.spawn(move || part.iter().map(f).collect::<Vec<R>>())).collect();
                handles.into_iter().flat_map(|h| h.join().expect("a snapshot worker panicked")).collect()
            });
        }
    }
    items.iter().map(f).collect()
}
#[cfg(not(target_arch = "wasm32"))]
const WIRE_THREADS: usize = 8;

fn enabled_colliders(rb: &Rollback, body: RigidBodyHandle) -> Vec<ColliderHandle> {
    let Some(b) = rb.data.physics.bodies.get(body) else { return Vec::new() };
    b.colliders()
        .iter()
        .copied()
        .filter(|&h| rb.data.physics.colliders.get(h).is_some_and(|c| c.is_enabled()))
        .collect()
}

fn swap_shape(rb: &mut Rollback, h: ColliderHandle, shape: SharedShape) {
    let Some(mut c) = rb.data.physics.colliders.get(h).cloned() else { return };
    c.swap_shape_raw(shape);
    rb.data.physics.raw_mut().colliders.raw_mut().restore_raw(h, c);
}

fn halo_ground(rb: &Rollback, k: WorldChunkPos) -> Option<&StudColumnGround> {
    rb.data.stud_halo_ground.get(&WorldChunkPos { y: 0, ..k })
}

fn own_ground(rb: &Rollback, c: StudChunkCoords) -> Option<&StudColumnGround> {
    rb.data.stud_ground.get(&ColumnCoords::new(c.x, c.z))
}

#[derive(Clone, Copy)]
enum Slot {
    Own(StudChunkCoords, EntityKey),
    Halo(WorldChunkPos),
}

/// One stud chunk of the snapshot being written.
struct OutJob<'a> {
    slot: Slot,
    layer: i32,
    body: Option<RigidBodyHandle>,
    chunk: &'a StudChunk,
    ground: Option<&'a StudColumnGround>,
    /// The enabled voxel colliders of the chunk's body.
    grids: Vec<(ColliderHandle, SharedShape)>,
}

/// `serde(with)` for the `Rollback` of `ServerPacket::Region`.
pub fn serialize<S: serde::Serializer>(rb: &Rollback, s: S) -> Result<S::Ok, S::Error> {
    let omit = !SEND_COLLIDERS.load(Ordering::Relaxed);
    let grids = |body: RigidBodyHandle| -> Vec<(ColliderHandle, SharedShape)> {
        if !omit {
            return Vec::new();
        }
        enabled_colliders(rb, body)
            .into_iter()
            .filter_map(|h| {
                let shape = rb.data.physics.colliders.get(h)?.shared_shape();
                shape.as_voxels().map(|_| (h, shape.clone()))
            })
            .collect()
    };
    let mut jobs: Vec<OutJob> = Vec::new();
    for (c, e) in rb.data.stud_index.iter() {
        let Some(chunk) = rb.data.ecs.stud.try_get(*e).as_ref() else { continue };
        let body = *rb.data.ecs.rigidbody.try_get(*e);
        jobs.push(OutJob { slot: Slot::Own(*c, *e), layer: c.y, body, chunk, ground: own_ground(rb, *c), grids: body.map_or_else(Vec::new, grids) });
    }
    for (k, h) in rb.data.stud_halo.iter() {
        let Some(chunk) = h.chunk.as_ref() else { continue };
        jobs.push(OutJob { slot: Slot::Halo(*k), layer: k.y, body: Some(h.body), chunk, ground: halo_ground(rb, *k), grids: grids(h.body) });
    }
    if jobs.is_empty() {
        return WireOut { slim: rb, own: Vec::new(), halo: Vec::new(), shapes: Vec::new() }.serialize(s);
    }
    // Per chunk, off this thread: its diff, and the verdict of every grid not judged yet.
    let done = par_map(&jobs, |job| {
        let diff = job.ground.and_then(|g| g.pristine_chunk(job.layer)).and_then(|p| StudChunkDiff::smaller(job.chunk, &p));
        let verdicts: Vec<Option<bool>> = job.grids.iter().map(|(_, shape)| shape_verdict(shape)).collect();
        let omitted: Vec<ColliderHandle> = if verdicts.iter().all(|v| v.is_some()) {
            job.grids.iter().zip(&verdicts).filter(|(_, v)| **v == Some(true)).map(|((h, _), _)| *h).collect()
        } else {
            let built: Vec<SharedShape> = stud_collider_shapes(job.chunk).into_iter().filter(|s| s.as_voxels().is_some()).collect();
            let bytes = |s: &SharedShape| bincode::serialize(s).ok();
            job.grids
                .iter()
                .filter(|(_, shape)| {
                    let size = shape.as_voxels().map(|v| v.voxel_size());
                    let fresh = built
                        .iter()
                        .find(|b| b.as_voxels().map(|v| v.voxel_size()) == size)
                        .is_some_and(|b| bytes(b).is_some_and(|b| Some(b) == bytes(shape)));
                    note_shape(shape, fresh);
                    fresh
                })
                .map(|(h, _)| *h)
                .collect()
        };
        (diff, omitted)
    });
    let mut slim = rb.clone();
    let hole = placeholder();
    let (mut own, mut halo, mut shapes) = (Vec::new(), Vec::new(), Vec::new());
    for (job, (diff, omitted)) in jobs.iter().zip(done) {
        for h in omitted {
            swap_shape(&mut slim, h, hole.clone());
            shapes.push(h);
        }
        let Some(diff) = diff else { continue };
        match job.slot {
            Slot::Own(c, e) => {
                slim.data.ecs.stud.raw_mut().insert(e, Some(StudChunk::default()));
                own.push((c, diff));
            }
            Slot::Halo(k) => {
                let Some(body) = job.body else { continue };
                slim.data.stud_halo.insert_raw(k, crate::StudHaloChunk { chunk: Some(StudChunk::default()), body });
                halo.push((k, diff));
            }
        }
    }
    WireOut { slim: &slim, own, halo, shapes }.serialize(s)
}

/// One stud chunk of a received snapshot that needs work.
struct InJob<'a> {
    slot: Slot,
    layer: i32,
    body: Option<RigidBodyHandle>,
    /// The chunk as received, when it came whole.
    whole: Option<&'a StudChunk>,
    diff: Option<StudChunkDiff>,
    ground: Option<&'a StudColumnGround>,
    /// The body's enabled colliders, when any of them was omitted.
    rebuild: Vec<ColliderHandle>,
}

const BAD_DIFF: &str = "region snapshot: a stud chunk diff against no recipe";
const BAD_SHAPES: &str = "region snapshot: an omitted stud collider does not match its chunk";

/// Inverse of [`serialize`]. Wire data is untrusted: a snapshot that does not add up
/// is an error, never a panic or a hole in the terrain.
pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Rollback, D::Error> {
    use serde::de::Error;
    let WireIn { slim: mut rb, own, halo, shapes } = WireIn::deserialize(d)?;
    if own.is_empty() && halo.is_empty() && shapes.is_empty() {
        return Ok(rb);
    }
    let omitted: BTreeSet<(u32, u32)> = shapes.iter().map(|h| h.into_raw_parts()).collect();
    let mut own: BTreeMap<StudChunkCoords, StudChunkDiff> = own.into_iter().collect();
    let mut halo: BTreeMap<WorldChunkPos, StudChunkDiff> = halo.into_iter().collect();
    let rebuild = |rb: &Rollback, body: Option<RigidBodyHandle>| -> Vec<ColliderHandle> {
        let enabled = body.map_or_else(Vec::new, |b| enabled_colliders(rb, b));
        if enabled.iter().any(|h| omitted.contains(&h.into_raw_parts())) { enabled } else { Vec::new() }
    };
    let mut jobs: Vec<InJob> = Vec::new();
    for (c, e) in rb.data.stud_index.iter() {
        let body = *rb.data.ecs.rigidbody.try_get(*e);
        let (diff, rebuild) = (own.remove(c), rebuild(&rb, body));
        if diff.is_some() || !rebuild.is_empty() {
            let whole = rb.data.ecs.stud.try_get(*e).as_ref();
            jobs.push(InJob { slot: Slot::Own(*c, *e), layer: c.y, body, whole, diff, ground: own_ground(&rb, *c), rebuild });
        }
    }
    for (k, h) in rb.data.stud_halo.iter() {
        let (diff, rebuild) = (halo.remove(k), rebuild(&rb, Some(h.body)));
        if diff.is_some() || !rebuild.is_empty() {
            jobs.push(InJob { slot: Slot::Halo(*k), layer: k.y, body: Some(h.body), whole: h.chunk.as_ref(), diff, ground: halo_ground(&rb, *k), rebuild });
        }
    }
    if !own.is_empty() || !halo.is_empty() {
        return Err(D::Error::custom("region snapshot: a diff for no stud chunk"));
    }
    type Done = Result<(Option<StudChunk>, Vec<(ColliderHandle, SharedShape)>), &'static str>;
    let done: Vec<Done> = par_map(&jobs, |job| {
        let rebuilt = match &job.diff {
            Some(diff) => Some(diff.clone().apply(job.ground.and_then(|g| g.pristine_chunk(job.layer)).ok_or(BAD_DIFF)?)),
            None => None,
        };
        let mut swaps = Vec::new();
        if !job.rebuild.is_empty() {
            let chunk = rebuilt.as_ref().or(job.whole).ok_or(BAD_SHAPES)?;
            let built = stud_collider_shapes(chunk);
            if built.len() != job.rebuild.len() {
                return Err(BAD_SHAPES);
            }
            for (h, shape) in job.rebuild.iter().zip(built) {
                if omitted.contains(&h.into_raw_parts()) {
                    if shape.as_voxels().is_none() {
                        return Err(BAD_SHAPES);
                    }
                    swaps.push((*h, shape));
                }
            }
        }
        Ok((rebuilt, swaps))
    });
    let slots: Vec<(Slot, Option<RigidBodyHandle>)> = jobs.iter().map(|j| (j.slot, j.body)).collect();
    drop(jobs);
    let mut restored = 0;
    for ((slot, body), done) in slots.into_iter().zip(done) {
        let (chunk, swaps) = done.map_err(D::Error::custom)?;
        if let Some(chunk) = chunk {
            match (slot, body) {
                (Slot::Own(_, e), _) => {
                    rb.data.ecs.stud.raw_mut().insert(e, Some(chunk));
                }
                (Slot::Halo(k), Some(body)) => {
                    rb.data.stud_halo.insert_raw(k, crate::StudHaloChunk { chunk: Some(chunk), body });
                }
                (Slot::Halo(_), None) => {}
            }
        }
        for (h, shape) in swaps {
            note_fresh_shape(&shape);
            swap_shape(&mut rb, h, shape);
            restored += 1;
        }
    }
    if restored != omitted.len() {
        return Err(D::Error::custom("region snapshot: an omitted stud collider has no chunk to be built from"));
    }
    Ok(rb)
}
