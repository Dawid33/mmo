//! The portable region format (world store R0+R1, Task 3): what the world
//! store keeps of a region, independent of its in-memory layout (rapier
//! handles, slotmap keys, undo wrappers). A region is its meta maps, its
//! terrain chunks, its ground recipes and its owned entities as
//! [`EntityBundle`]s — the same bundles a seam crossing sends. Players go to
//! the store's players table, people are respawned by the history, and
//! everything else is derived on load or dropped; [`classify`] pins that
//! decision for every `GameData` and `Ecs` field.
//!
//! Saves are incremental: [`Rollback::portable_snapshot`] takes the terms
//! ([`SaveTerms`]) of the last save and returns only the chunks
//! whose term changed, plus the full current terms for the next call.
//!
//! **Stud terrain** (stud world plan, P5) is stored by one rule: only what the
//! ground recipe cannot give back. Per materialised stud chunk
//! ([`StudKept`]): content equal to its recipe's `pristine_chunk` in a
//! surface layer → no row (a restore materialises the surface layers by
//! itself); equal to it anywhere else (a neighbour a carve opened, a chunk
//! dug and refilled) → a [`StudChunkRecord::Pristine`] marker, coordinates
//! only; anything else (edited, built in the air, holding feature bricks)
//! → its bricks. A restore therefore materialises exactly the chunks that
//! were materialised when it was saved, with the same bricks and colliders,
//! and everything else stays implied by the recipe as before.
//! [`restore_portable`] rebuilds a running region from a [`StoredRegion`].
//! A restored region does not hash-match its pre-save self (physics handles
//! and entity keys are rebuilt); its portable snapshot matches.

use std::collections::BTreeMap;

use crate::{
    Chunk, ChunkCoords, ClientId, ColumnCoords, ColumnGround, Ecs, EntityBundle, EntityKind, GameData, PlayerRecord,
    Region, RegionId, Rollback, StateHash as _, StudChunk, StudChunkCoords, StudColumnGround, Tick,
};

/// Every terrain chunk's `chunk_terrain_term` (its coordinates + content
/// digest), by chunk: what a save compares against to send only the
/// chunks that changed.
pub type ChunkTerms = BTreeMap<ChunkCoords, u32>;

/// How the store keeps one materialised stud chunk (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StudKept {
    /// Pristine in a surface layer: no row, re-derived by every restore.
    Derived,
    /// Pristine outside the surface layers: a marker row.
    Pristine,
    /// Not what the recipe gives: its bricks.
    Edited,
}

/// One materialised stud chunk at the last save: its content digest and how
/// it was kept. An unchanged digest keeps the classification, so a save
/// derives `pristine_chunk` only for chunks that changed since.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StudTerm {
    pub digest: u32,
    pub kept: StudKept,
}

impl StudTerm {
    /// What the store holds for the chunk: nothing, a marker, or bricks with this digest.
    fn row(&self) -> Option<Option<u32>> {
        match self.kept {
            StudKept::Derived => None,
            StudKept::Pristine => Some(None),
            StudKept::Edited => Some(Some(self.digest)),
        }
    }
}

/// [`StudTerm`] of every materialised stud chunk.
pub type StudTerms = BTreeMap<StudChunkCoords, StudTerm>;

/// What a save compares against: the terms of the previous one (empty: none
/// yet in this incarnation, so the next snapshot is whole).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveTerms {
    pub chunks: ChunkTerms,
    pub studs: StudTerms,
}

impl SaveTerms {
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty() && self.studs.is_empty()
    }
}

/// A stored stud chunk (`stud_chunks.data`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StudChunkRecord {
    /// Materialised with exactly its recipe's `pristine_chunk`, outside the
    /// surface layers: a restore derives it again.
    Pristine,
    /// Its bricks.
    Edited(StudChunk),
}

/// A region's state besides its terrain and entities: the clock, the id
/// counters and the rollback maps that persist.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RegionMeta {
    pub tick: Tick,
    pub next_game_event_id: usize,
    pub next_entity_serial: u64,
    pub designations: BTreeMap<crate::TilePos, crate::Designation>,
    pub constructed: BTreeMap<crate::TilePos, ()>,
    pub item_piles: BTreeMap<crate::WorldBlockPos, crate::ItemPile>,
    pub containers: BTreeMap<crate::WorldBlockPos, crate::Container>,
    pub pile_takes: BTreeMap<(crate::EntityId, crate::WorldBlockPos), Tick>,
    pub intents: BTreeMap<(Tick, crate::EntityId, u32), crate::SentIntent>,
    pub intent_replies: BTreeMap<(Tick, crate::EntityId, u32), crate::SentReply>,
    pub intent_inbox: BTreeMap<(crate::EntityId, u32), crate::IntentReply>,
    /// The region's labelled water bodies. Labelled once at genesis from
    /// the genesis chunks (ice splits bodies), so a restore inserts them
    /// as stored and never relabels.
    pub water_bodies: BTreeMap<crate::WaterBodyId, crate::WaterBody>,
}

/// One snapshot of a running region, in the portable format.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PortableRegion {
    pub meta: RegionMeta,
    /// Only the chunks whose term differs from the saved terms (all of them
    /// against empty terms).
    pub chunks: BTreeMap<ChunkCoords, Chunk>,
    /// The ground recipes: `Some` only in a region's first snapshot of this
    /// incarnation (when the saved terms are empty) — they never change.
    pub ground: Option<Vec<(ColumnCoords, ColumnGround)>>,
    /// Every owned entity that is neither a player nor a person, sorted by
    /// `EntityId`.
    pub entities: Vec<EntityBundle>,
    /// The players with a body here, as records (claims empty).
    pub players: Vec<(ClientId, PlayerRecord)>,
    /// The stud chunks whose stored row changes: `Some` = write this record,
    /// `None` = delete the row (the chunk is pristine in a surface layer
    /// again, or gone). Empty for a voxel region.
    pub stud_chunks: BTreeMap<StudChunkCoords, Option<StudChunkRecord>>,
    /// The stud ground recipes, `Some` when `ground` is (a first snapshot);
    /// empty for a voxel region.
    pub stud_ground: Option<Vec<(ColumnCoords, StudColumnGround)>>,
}

impl PortableRegion {
    /// Folds a newer snapshot of the same region into this one (a save that
    /// has not been committed yet): chunks and stud rows union with the
    /// newer winning (a newer delete replaces an older write), meta, entities
    /// and players replaced, the grounds the first `Some`.
    pub fn merge(&mut self, newer: PortableRegion) {
        let PortableRegion { meta, chunks, ground, entities, players, stud_chunks, stud_ground } = newer;
        self.meta = meta;
        self.chunks.extend(chunks);
        if self.ground.is_none() {
            self.ground = ground;
        }
        self.entities = entities;
        self.players = players;
        self.stud_chunks.extend(stud_chunks);
        if self.stud_ground.is_none() {
            self.stud_ground = stud_ground;
        }
    }
}

/// A region as the store holds it: every chunk ever saved (the latest
/// version of each), the ground recipes and the entities. What
/// [`restore_portable`] rebuilds a region from.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StoredRegion {
    pub meta: RegionMeta,
    pub chunks: Vec<(ChunkCoords, Chunk)>,
    pub ground: Vec<(ColumnCoords, ColumnGround)>,
    pub entities: Vec<EntityBundle>,
    /// Every stored stud chunk row, by coordinates (module docs).
    pub stud_chunks: Vec<(StudChunkCoords, StudChunkRecord)>,
    /// The stud ground recipes (empty: a voxel region).
    pub stud_ground: Vec<(ColumnCoords, StudColumnGround)>,
}

/// A whole snapshot (one taken against empty terms) as a stored region; its
/// players are dropped — they go to the players table.
impl From<PortableRegion> for StoredRegion {
    fn from(p: PortableRegion) -> Self {
        StoredRegion {
            meta: p.meta,
            chunks: p.chunks.into_iter().collect(),
            ground: p.ground.unwrap_or_default(),
            entities: p.entities,
            stud_chunks: p.stud_chunks.into_iter().filter_map(|(c, r)| Some((c, r?))).collect(),
            stud_ground: p.stud_ground.unwrap_or_default(),
        }
    }
}

impl Rollback {
    /// The current [`ChunkTerms`] of every terrain chunk.
    pub fn chunk_terms(&self) -> ChunkTerms {
        let mut terms = ChunkTerms::new();
        for (key, chunk) in self.data.ecs.chunk.iter() {
            if chunk.is_none() {
                continue;
            }
            let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let coords = crate::chunk_coords_from_translation(body.translation());
            if let Some(term) = self.chunk_terrain_term(key) {
                terms.insert(coords, term);
            }
        }
        terms
    }

    /// A materialised stud chunk's content digest (coordinates + bricks).
    fn stud_digest(c: StudChunkCoords, chunk: &StudChunk) -> u32 {
        let mut h = crate::StateHasher::new();
        c.state_hash(&mut h);
        chunk.state_hash(&mut h);
        h.finalize()
    }

    /// How the store keeps stud chunk `c` holding `chunk` (module docs).
    fn stud_kept(&self, c: StudChunkCoords, chunk: &StudChunk) -> StudKept {
        let Some(g) = self.data.stud_ground.get(&ColumnCoords::new(c.x, c.z)) else { return StudKept::Edited };
        if g.pristine_chunk(c.y).as_ref() != Some(chunk) {
            StudKept::Edited
        } else if g.surface_layers().contains(&c.y) {
            StudKept::Derived
        } else {
            StudKept::Pristine
        }
    }

    /// The current [`StudTerms`]: a chunk whose digest is the one in `saved`
    /// keeps its classification, any other is classified against its recipe.
    pub fn stud_terms(&self, saved: &StudTerms) -> StudTerms {
        let mut terms = StudTerms::new();
        for (&c, &key) in self.data.stud_index.iter() {
            let Some(chunk) = self.data.ecs.stud.try_get(key).as_ref() else { continue };
            let digest = Self::stud_digest(c, chunk);
            let kept = match saved.get(&c) {
                Some(t) if t.digest == digest => t.kept,
                _ => self.stud_kept(c, chunk),
            };
            terms.insert(c, StudTerm { digest, kept });
        }
        terms
    }

    /// The stud rows that change between `saved` and `terms`.
    fn stud_rows(&self, saved: &StudTerms, terms: &StudTerms) -> BTreeMap<StudChunkCoords, Option<StudChunkRecord>> {
        let mut rows = BTreeMap::new();
        for (c, term) in terms {
            let before = saved.get(c).and_then(StudTerm::row);
            if before == term.row() {
                continue;
            }
            let record = match term.kept {
                StudKept::Derived => None,
                StudKept::Pristine => Some(StudChunkRecord::Pristine),
                StudKept::Edited => self.stud_chunk_at(*c).cloned().map(StudChunkRecord::Edited),
            };
            rows.insert(*c, record);
        }
        // A chunk that is no longer materialised (its creation was undone).
        for (c, term) in saved {
            if !terms.contains_key(c) && term.row().is_some() {
                rows.insert(*c, None);
            }
        }
        rows
    }

    /// This region in the portable format, read-only, with the chunks whose
    /// term differs from `saved` (every chunk when `saved` is empty, which
    /// also adds the ground recipes), and the full current terms.
    pub fn portable_snapshot(&self, saved: &SaveTerms) -> (PortableRegion, SaveTerms) {
        let d = &self.data;
        let first = saved.is_empty();
        let stud_terms = self.stud_terms(&saved.studs);
        let stud_chunks = self.stud_rows(&saved.studs, &stud_terms);
        let stud_ground = first.then(|| d.stud_ground.iter().map(|(c, g)| (*c, g.clone())).collect());
        let saved = &saved.chunks;
        let terms = self.chunk_terms();
        let mut chunks = BTreeMap::new();
        for (key, chunk) in d.ecs.chunk.iter() {
            let Some(chunk) = chunk else { continue };
            let Some(handle) = *d.ecs.rigidbody.try_get(key) else { continue };
            let Some(body) = d.physics.bodies.get(handle) else { continue };
            let coords = crate::chunk_coords_from_translation(body.translation());
            if saved.get(&coords) != terms.get(&coords) {
                chunks.insert(coords, chunk.clone());
            }
        }
        let ground = first.then(|| d.ground.iter().map(|(c, g)| (*c, g.clone())).collect());
        // `id_index` is ordered by `EntityId` and holds owned entities only
        // (never halo mirrors).
        let entities = d
            .id_index
            .iter()
            .filter(|(_, &key)| {
                !matches!(*d.ecs.kind.try_get(key), Some(EntityKind::Player) | Some(EntityKind::Person { .. }))
                    && !d.player_entites.values().any(|&p| p == key)
            })
            .map(|(_, &key)| self.bundle_of(key))
            .collect();
        let meta = RegionMeta {
            tick: *d.tick,
            next_game_event_id: *d.next_game_event_id,
            next_entity_serial: *d.next_entity_serial,
            designations: (*d.designations).clone(),
            constructed: (*d.constructed).clone(),
            item_piles: (*d.item_piles).clone(),
            containers: (*d.containers).clone(),
            pile_takes: (*d.pile_takes).clone(),
            intents: (*d.intents).clone(),
            intent_replies: (*d.intent_replies).clone(),
            intent_inbox: (*d.intent_inbox).clone(),
            water_bodies: (*d.water_bodies).clone(),
        };
        let players = crate::players_in(self, *d.region);
        (
            PortableRegion { meta, chunks, ground, entities, players, stud_chunks, stud_ground },
            SaveTerms { chunks: terms, studs: stud_terms },
        )
    }
}

/// Rebuilds region `id` from its stored form, and returns its terms
/// (what the next save compares against):
/// 1. the terrain, ground recipes and water bodies, raw and unlogged like
///    genesis (the water bodies as stored — never relabelled); the stud
///    terrain the same way: the recipes, the pristine chunk of every surface
///    layer, then the stored rows — an edited chunk replacing or adding to
///    them, a `Pristine` marker derived from its recipe — and
///    `BodyScale::STUD` for a region with stud terrain;
/// 2. the meta maps, raw; `pile_expiry` rebuilt from `item_piles`; the
///    clock and counters in one forgotten transaction;
/// 3. the region around it, then the entities through `apply_arrival`;
/// 4. `next_entity_serial` raised to at least the stored value.
///
/// The rollback is built before the `Region`, so the region's
/// `base_event_id` is the stored `next_game_event_id`.
pub fn restore_portable(id: RegionId, stored: StoredRegion) -> (Region, SaveTerms) {
    let StoredRegion { meta, chunks, ground, entities, stud_chunks, stud_ground } = stored;
    let RegionMeta {
        tick,
        next_game_event_id,
        next_entity_serial,
        designations,
        constructed,
        item_piles,
        containers,
        pile_takes,
        intents,
        intent_replies,
        intent_inbox,
        water_bodies,
    } = meta;
    let mut rb = Rollback::new(None);
    rb.seed_terrain_genesis(chunks);
    rb.seed_ground_genesis(ground);
    rb.seed_water_genesis(water_bodies);
    // How each stored stud row was kept; every other materialised chunk is a
    // derived surface chunk.
    let mut stud_kept: BTreeMap<StudChunkCoords, StudKept> = BTreeMap::new();
    if !stud_ground.is_empty() || !stud_chunks.is_empty() {
        let recipes: BTreeMap<ColumnCoords, &StudColumnGround> = stud_ground.iter().map(|(c, g)| (*c, g)).collect();
        let mut overrides = BTreeMap::new();
        for (c, record) in stud_chunks {
            match record {
                StudChunkRecord::Edited(chunk) => {
                    stud_kept.insert(c, StudKept::Edited);
                    overrides.insert(c, chunk);
                }
                StudChunkRecord::Pristine => {
                    let pristine = recipes.get(&ColumnCoords::new(c.x, c.z)).and_then(|g| g.pristine_chunk(c.y));
                    match pristine {
                        Some(chunk) => {
                            stud_kept.insert(c, StudKept::Pristine);
                            overrides.insert(c, chunk);
                        }
                        // The store checks its rows; a marker over air has nothing to
                        // materialise and the recipe still answers every read.
                        None => log::error!("region {id:?}: stored pristine stud chunk {c:?} has no recipe ground; skipped"),
                    }
                }
            }
        }
        drop(recipes);
        rb.seed_stud_ground_genesis(stud_ground, overrides);
        rb.set_body_scale_raw(crate::movement::BodyScale::STUD);
    }
    {
        let d = &mut rb.data;
        for (k, v) in designations {
            d.designations.insert_raw(k, v);
        }
        for (k, v) in constructed {
            d.constructed.insert_raw(k, v);
        }
        for (p, pile) in item_piles {
            d.pile_expiry.insert_raw((pile.expire_tick, p), ());
            d.item_piles.insert_raw(p, pile);
        }
        for (k, v) in containers {
            d.containers.insert_raw(k, v);
        }
        for (k, v) in pile_takes {
            d.pile_takes.insert_raw(k, v);
        }
        for (k, v) in intents {
            d.intents.insert_raw(k, v);
        }
        for (k, v) in intent_replies {
            d.intent_replies.insert_raw(k, v);
        }
        for (k, v) in intent_inbox {
            d.intent_inbox.insert_raw(k, v);
        }
    }
    // The cells have no raw setter: one forgotten transaction, like
    // `Region::jump_clock_to`.
    rb.new_transaction();
    rb.data.tick.set(tick);
    rb.data.next_game_event_id.set(next_game_event_id);
    rb.data.next_entity_serial.set(next_entity_serial);
    rb.forget();
    let mut region = Region::new(rb, None, id, None);
    region.with_data(|d| {
        for bundle in entities {
            d.apply_arrival(bundle);
        }
        if *d.data.next_entity_serial < next_entity_serial {
            d.data.next_entity_serial.set(next_entity_serial);
        }
    });
    let data = region.data();
    let mut studs = StudTerms::new();
    for (&c, &key) in data.data.stud_index.iter() {
        let Some(chunk) = data.data.ecs.stud.try_get(key).as_ref() else { continue };
        let kept = stud_kept.get(&c).copied().unwrap_or(StudKept::Derived);
        studs.insert(c, StudTerm { digest: Rollback::stud_digest(c, chunk), kept });
    }
    let terms = SaveTerms { chunks: data.chunk_terms(), studs };
    (region, terms)
}

/// The field classification of the portable format, pinned at compile
/// time: `GameData` and `Ecs` are destructured with no `..`, so a new field
/// does not build until someone decides here whether it persists, is
/// derived on load, or is dropped.
pub fn classify(d: &GameData) {
    let GameData {
        ecs,
        // Derive on load: every rapier set is rebuilt from the chunks
        // (`seed_terrain_genesis`) and the bundles (`apply_arrival`).
        physics: _,
        // Persist: `RegionMeta`.
        tick: _,
        next_game_event_id: _,
        next_entity_serial: _,
        // Derive on load: the region id the store keys the row by
        // (`Region::new`).
        region: _,
        // Derive on load: genesis-only (`set_body_scale_raw`); a restored
        // region with stud terrain gets `BodyScale::STUD`, any other the
        // default `BodyScale::VOXEL`.
        body_scale: _,
        // Players: `PortableRegion::players` → the players table.
        player_entites: _,
        clients: _,
        // Drop: the halo is reseeded by `seed_halo` at every start.
        halo_entities: _,
        halo_sent: _,
        // Derive on load: refilled by `apply_arrival`.
        arrivals: _,
        id_index: _,
        // Derive on load: refilled by `seed_terrain_genesis`.
        chunk_index: _,
        // Derive on load: refilled by `seed_stud_ground_genesis`.
        stud_index: _,
        // Persist: `PortableRegion::ground`, first snapshot only.
        ground: _,
        // Persist: `RegionMeta::water_bodies` (labelled at genesis only).
        water_bodies: _,
        // Persist: `RegionMeta`.
        designations: _,
        constructed: _,
        // Persons: the history respawns them.
        person_entities: _,
        // Persist: `RegionMeta`.
        item_piles: _,
        // Derive on load: rebuilt from `item_piles`.
        pile_expiry: _,
        // Drop: halo.
        halo_piles: _,
        // Persist: `RegionMeta`.
        pile_takes: _,
        containers: _,
        // Drop: halo.
        halo_containers: _,
        halo: _,
        halo_ground: _,
        // Drop: the stud halo (`stud_halo.rs`) — mirrors of the neighbours'
        // chunks, reseeded at every start, never in `stud_index`.
        stud_halo: _,
        stud_halo_ground: _,
        // Persist: `RegionMeta`.
        intents: _,
        intent_replies: _,
        intent_inbox: _,
        // Persist: `PortableRegion::stud_ground`, first snapshot only.
        stud_ground: _,
    } = d;
    let Ecs {
        // Derive on load: each entity gets a fresh key.
        entities: _,
        // Persist (bundle, `Carried::camera`): players only, which are not
        // stored as entities.
        camera: _,
        // Derive on load: from the body state.
        isometry: _,
        rigidbody: _,
        // Persist: `PortableRegion::chunks`.
        chunk: _,
        // Persist (bundle): `EntityBundle::kind` / `id`.
        kind: _,
        id: _,
        // Persist (bundle, `Carried`): swimming state (`Immersion`) rides in
        // `Motion`.
        motion: _,
        slime: _,
        animator: _,
        // Derive on load: rebuilt from the animator (`attach_rig_safe`).
        bones: _,
        // Persist (bundle, `Carried`).
        voxel_body: _,
        walker: _,
        wander: _,
        // Persist: `PortableRegion::stud_chunks` — only what the ground
        // recipe cannot give back (module docs); the rest is derived on load.
        stud: _,
        // Persist (bundle, `Carried::stud_body`).
        stud_body: _,
    } = &**ecs;
}
