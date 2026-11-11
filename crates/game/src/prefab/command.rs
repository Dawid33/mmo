//! Prefabs in a running game: what a client sends (`GameEventKind::Prefab`) and the
//! per-player table of registered prefabs it acts on.
//!
//! The durable library is the player's own files; the world never stores one. To use a
//! prefab in a game the client registers its resolved form; the server validates it
//! again (it is untrusted input) and keeps it in that player's `Client`, where it is
//! ordinary rollback state that travels with the player across seams. Registrations
//! last for the session: a parked record keeps none (`parked_client`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{layout_build, prefabs, ChunkStamp, PrefabKey, ResolvedKind, ResolvedPrefab, Trust};
use crate::stud::{stud_layer_ok, STUD_LAYER_LIMIT};
use crate::{
    ClientId, Intent, IntentMsg, Rollback, StudChunkCoords, StudShape, Target, REGION_CHUNKS, REGION_STUDS,
    STUD_CHUNK_PLATES, STUD_CHUNK_STUDS,
};

/// Most prefabs one player may have registered at a time.
pub const MAX_REGISTERED_PREFABS: usize = 8;
/// Most elements (`ResolvedPrefab::element_count`) one player's registered prefabs may
/// hold in total: about 100 KB of state at worst.
pub const MAX_REGISTERED_ELEMENTS: usize = 8192;

/// One registered prefab. The definition sits behind a shared pointer with its key
/// computed once, so hashing, undo copies and seam crossings do not walk it: the state
/// hash covers the key alone (the key IS the hash of the definition). On the wire it is
/// the definition alone; the key is recomputed on arrival, never trusted.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisteredPrefab {
    pub key: PrefabKey,
    pub def: Arc<ResolvedPrefab>,
}

impl RegisteredPrefab {
    pub fn new(def: ResolvedPrefab) -> Self {
        Self { key: def.key(), def: Arc::new(def) }
    }
}

impl state_hash::StateHash for RegisteredPrefab {
    fn state_hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.key.state_hash(h);
    }
}

impl Serialize for RegisteredPrefab {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (*self.def).serialize(s)
    }
}

impl<'de> Deserialize<'de> for RegisteredPrefab {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        ResolvedPrefab::deserialize(d).map(Self::new)
    }
}

/// Which prefab a stamp lays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrefabRef {
    /// One the stamping player has registered.
    Registered(PrefabKey),
    /// One of the game's own, by `prefab_name_key` (debug and the editor).
    Builtin(u32),
}

/// What a client asks for; carried by `GameEventKind::Prefab`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum PrefabCommand {
    Register(Box<ResolvedPrefab>),
    Deregister(PrefabKey),
    /// Lay a prefab with its anchor on world cell `anchor` (studs x, plates y, studs z),
    /// turned `turn` quarter turns. Developer-grade, like `Order::CarveStud` /
    /// `FillStud`: no cost, no survival rules.
    Stamp { prefab: PrefabRef, anchor: [i64; 3], turn: u8 },
}

// Compact on purpose: events are logged with `{:?}`, and a definition is thousands of parts.
impl std::fmt::Debug for PrefabCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrefabCommand::Register(p) => write!(f, "Register({:?}, {} elements)", p.name, p.element_count()),
            PrefabCommand::Deregister(k) => write!(f, "Deregister({k:?})"),
            PrefabCommand::Stamp { prefab, anchor, turn } => write!(f, "Stamp({prefab:?} at {anchor:?} turn {turn})"),
        }
    }
}

/// What a runtime stamp did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StampOutcome {
    /// Bricks laid in this region.
    pub placed: u32,
    /// Bricks left out here: overlapping a brick or a body, or in an illegal layer.
    pub skipped_overlap: u32,
    /// Non-box parts left out because they would straddle a chunk edge.
    pub skipped_split: u32,
    /// Carve boxes and bricks sent to neighbouring regions as intents (no reply is awaited).
    pub sent_to_neighbours: u32,
}

impl Rollback {
    /// Applies one chunk's share of a stamp to this region's chunk `c`, undo-safe, as
    /// ONE chunk write (`AddStudChunk` / `SetStudChunk`), or none when it changes
    /// nothing. Returns `(bricks placed, bricks skipped)`. A chunk that was only implied
    /// by the recipe is materialised when the stamp changes it, and also when a carve
    /// of the stamp reaches it or its side (the ground a pit exposes needs colliders).
    pub fn apply_chunk_stamp_safe(&mut self, c: StudChunkCoords, stamp: &ChunkStamp) -> (u32, u32) {
        if c.x >= REGION_CHUNKS || c.z >= REGION_CHUNKS || !stud_layer_ok(c.y) {
            return (0, stamp.bricks.len() as u32);
        }
        let implied = self.stud_chunk_or_pristine(c).map(std::borrow::Cow::into_owned);
        let exists = implied.is_some();
        let mut chunk = implied.unwrap_or_default();
        let applied = stamp.apply(c.y, &mut chunk);
        let opens = exists && !(stamp.carve.is_empty() && stamp.shell.is_empty());
        match self.stud_key_at(c) {
            Some(key) if applied.changed => self.set_stud_chunk_safe(key, chunk),
            Some(_) => {}
            None if applied.changed || opens => {
                self.create_stud_chunk_safe(c, chunk);
            }
            None => {}
        }
        if applied.changed {
            // Chests whose bricks went spill, and piles settle again, over the whole chunk.
            let (cw, ch) = (STUD_CHUNK_STUDS as i32, STUD_CHUNK_PLATES as i32);
            let lo = [c.x as i32 * cw, c.y * ch, c.z as i32 * cw];
            let hi = [lo[0] + cw, lo[1] + ch, lo[2] + cw];
            if applied.carved > 0 {
                let _destroyed = self.stud_containers_after_write_safe(lo, hi);
            }
            let _destroyed = self.stud_piles_after_write_safe(lo, hi);
        }
        (applied.placed, applied.skipped)
    }

    /// `PrefabCommand::Stamp`: lays a Build prefab with its anchor on world cell
    /// `anchor`. `None`, nothing changed: `turn > 3`, a reference that does not resolve
    /// (a key this client has not registered, a built-in name key nobody installed),
    /// or an anchor outside this region or the legal layers (the region that owns the
    /// anchor runs the layout).
    ///
    /// This region's chunks are written at once, one write per chunk; a brick that
    /// would overlap a body is left out. The part in a neighbouring region goes there
    /// as `Intent::CarveStud` per carve box and `Intent::PlaceStud` per brick, applied
    /// at the next tick like every intent; a neighbour that is not running does not
    /// get its part (intents never wake a region). Developer-grade, like
    /// `Order::CarveStud` / `FillStud`: no cost, no survival rules.
    pub fn stamp_prefab_order_safe(
        &mut self,
        client: ClientId,
        prefab: super::PrefabRef,
        anchor: [i64; 3],
        turn: u8,
        out: &mut Vec<IntentMsg>,
    ) -> Option<StampOutcome> {
        if turn > 3 {
            return None;
        }
        let def = match prefab {
            super::PrefabRef::Registered(key) => self.data.clients.get(&client)?.prefabs.get(&key)?.def.clone(),
            super::PrefabRef::Builtin(key) => prefabs().get(key)?.clone(),
        };
        let ResolvedKind::Build(build) = &def.kind;
        let region = *self.data.region;
        let local = crate::local_stud_cell(region, anchor);
        let plate_limit = STUD_LAYER_LIMIT as i64 * STUD_CHUNK_PLATES as i64;
        if !(0..REGION_STUDS).contains(&local[0])
            || !(0..REGION_STUDS).contains(&local[2])
            || !(-plate_limit..=plate_limit).contains(&anchor[1])
        {
            return None;
        }
        let layout = layout_build(build, anchor, turn);
        let by = self.stud_edit_source(client);
        let mut outcome = StampOutcome { skipped_split: layout.skipped_split, ..Default::default() };
        for (chunk, mut stamp) in layout.chunks {
            if !stud_layer_ok(chunk.y) {
                outcome.skipped_overlap += stamp.bricks.len() as u32;
                continue;
            }
            let (rc, c) = chunk.to_local();
            if rc == region {
                let before = stamp.bricks.len();
                stamp.bricks.retain(|b| !self.stud_brick_overlaps_body([c.x as i32, c.y, c.z as i32], b));
                outcome.skipped_overlap += (before - stamp.bricks.len()) as u32;
                let (placed, skipped) = self.apply_chunk_stamp_safe(StudChunkCoords { x: c.x, y: c.y, z: c.z }, &stamp);
                outcome.placed += placed;
                outcome.skipped_overlap += skipped;
                continue;
            }
            // A neighbour's chunk: its carve boxes, then its bricks, as intents. The
            // ground its pit exposes is re-bricked there by the carve itself.
            let base = [chunk.x * STUD_CHUNK_STUDS as i64, chunk.y as i64 * STUD_CHUNK_PLATES as i64, chunk.z * STUD_CHUNK_STUDS as i64];
            for (min, size) in stamp.carve {
                let lo = [0, 1, 2].map(|i| base[i] + i64::from(min[i]));
                let hi = [0, 1, 2].map(|i| lo[i] + i64::from(size[i]));
                self.send_intent_safe(Target::Stud(chunk), Intent::CarveStud { lo, hi, by }, out);
                outcome.sent_to_neighbours += 1;
            }
            for brick in stamp.bricks {
                if matches!(brick.shape, StudShape::Part { .. }) {
                    self.send_intent_safe(Target::Stud(chunk), Intent::PlaceStud { chunk, brick, by }, out);
                    outcome.sent_to_neighbours += 1;
                }
            }
        }
        Some(outcome)
    }

    /// Registers `prefab` for `client`. `false` with nothing changed when the client is
    /// unknown, the prefab is not valid for a player, or a cap would be passed;
    /// registering a prefab that already is changes nothing and is `true`.
    pub fn register_prefab_safe(&mut self, client: ClientId, prefab: ResolvedPrefab) -> bool {
        let Some(c) = self.data.clients.get(&client) else { return false };
        if prefab.validate(Trust::Player).is_err() {
            return false;
        }
        let entry = RegisteredPrefab::new(prefab);
        if c.prefabs.contains_key(&entry.key) {
            return true;
        }
        let used: usize = c.prefabs.values().map(|p| p.def.element_count()).sum();
        if c.prefabs.len() >= MAX_REGISTERED_PREFABS || used + entry.def.element_count() > MAX_REGISTERED_ELEMENTS {
            return false;
        }
        let Some(c) = self.data.clients.get_mut(&client) else { return false };
        c.prefabs.insert(entry.key, entry);
        true
    }

    /// Removes `client`'s registration of `key`; `false` when there was none.
    pub fn deregister_prefab_safe(&mut self, client: ClientId, key: PrefabKey) -> bool {
        if !self.data.clients.get(&client).is_some_and(|c| c.prefabs.contains_key(&key)) {
            return false;
        }
        let Some(c) = self.data.clients.get_mut(&client) else { return false };
        c.prefabs.remove(&key).is_some()
    }
}
