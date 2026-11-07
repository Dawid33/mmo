//! The portable region format (world store R0+R1, Task 3): what the world
//! store keeps of a region, independent of its in-memory layout (rapier
//! handles, slotmap keys, undo wrappers). A region is its meta maps, its
//! terrain chunks, its ground recipes and its owned entities as
//! [`EntityBundle`]s — the same bundles a seam crossing sends. Players go to
//! the store's players table, people are respawned by the history, and
//! everything else is derived on load or dropped; [`classify`] pins that
//! decision for every `GameData` and `Ecs` field.
//!
//! Saves are incremental: [`Rollback::portable_snapshot`] takes the chunk
//! terms ([`ChunkTerms`]) of the last save and returns only the chunks
//! whose term changed, plus the full current terms for the next call.
//! [`restore_portable`] rebuilds a running region from a [`StoredRegion`].
//! A restored region does not hash-match its pre-save self (physics handles
//! and entity keys are rebuilt); its portable snapshot matches.

use std::collections::BTreeMap;

use crate::{
    Chunk, ChunkCoords, ClientId, ColumnCoords, ColumnGround, Ecs, EntityBundle, EntityKind, GameData, PlayerRecord,
    Region, RegionId, Rollback, Tick,
};

/// Every terrain chunk's `chunk_terrain_term` (its coordinates + content
/// digest), by chunk: what a save compares against to send only the
/// chunks that changed.
pub type ChunkTerms = BTreeMap<ChunkCoords, u32>;

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
}

impl PortableRegion {
    /// Folds a newer snapshot of the same region into this one (a save that
    /// has not been committed yet): chunks union with the newer winning,
    /// meta, entities and players replaced, the ground the first `Some`.
    pub fn merge(&mut self, newer: PortableRegion) {
        let PortableRegion { meta, chunks, ground, entities, players } = newer;
        self.meta = meta;
        self.chunks.extend(chunks);
        if self.ground.is_none() {
            self.ground = ground;
        }
        self.entities = entities;
        self.players = players;
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

    /// This region in the portable format, read-only, with the chunks whose
    /// term differs from `saved` (every chunk when `saved` is empty, which
    /// also adds the ground recipes), and the full current terms.
    pub fn portable_snapshot(&self, saved: &ChunkTerms) -> (PortableRegion, ChunkTerms) {
        let d = &self.data;
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
        let ground = saved.is_empty().then(|| d.ground.iter().map(|(c, g)| (*c, g.clone())).collect());
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
        (PortableRegion { meta, chunks, ground, entities, players }, terms)
    }
}

/// Rebuilds region `id` from its stored form, and returns its chunk terms
/// (what the next save compares against):
/// 1. the terrain, ground recipes and water bodies, raw and unlogged like
///    genesis (the water bodies as stored — never relabelled);
/// 2. the meta maps, raw; `pile_expiry` rebuilt from `item_piles`; the
///    clock and counters in one forgotten transaction;
/// 3. the region around it, then the entities through `apply_arrival`;
/// 4. `next_entity_serial` raised to at least the stored value.
///
/// The rollback is built before the `Region`, so the region's
/// `base_event_id` is the stored `next_game_event_id`.
pub fn restore_portable(id: RegionId, stored: StoredRegion) -> (Region, ChunkTerms) {
    let StoredRegion { meta, chunks, ground, entities } = stored;
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
    let terms = region.data().chunk_terms();
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
        // Persist: `RegionMeta`.
        intents: _,
        intent_replies: _,
        intent_inbox: _,
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
    } = &**ecs;
}
