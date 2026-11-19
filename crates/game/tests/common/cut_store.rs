//! A stand-in for the world store (Task 6 writes the real one): what the
//! store holds after committing a sequence of save cuts
//! ([`game::WorldCut`]), and the places an entity is saved in.
use game::{
    ChunkCoords, Chunk, InFlightRow, PlayerId, PlayerRecord, RegionCoords, StoredRegion, WorldCut,
};
use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub struct CutStore {
    pub regions: BTreeMap<RegionCoords, StoredRegion>,
    /// The latest cut's in-flight rows (each cut replaces them).
    pub in_flight: Vec<InFlightRow>,
    pub players: BTreeMap<PlayerId, PlayerRecord>,
    pub history: Option<Vec<u8>>,
    pub tick: u64,
}

impl CutStore {
    /// Commits `cut`: each region's meta and entities replaced, its chunks
    /// upserted, its ground set once; the in-flight rows replaced; the
    /// players upserted.
    pub fn apply(&mut self, cut: &WorldCut) {
        for (rc, p) in &cut.regions {
            let stored = self.regions.entry(*rc).or_default();
            stored.meta = p.meta.clone();
            let mut chunks: BTreeMap<ChunkCoords, Chunk> = std::mem::take(&mut stored.chunks).into_iter().collect();
            chunks.extend(p.chunks.iter().map(|(c, k)| (*c, k.clone())));
            stored.chunks = chunks.into_iter().collect();
            if let Some(ground) = &p.ground {
                stored.ground = ground.clone();
            }
            stored.entities = p.entities.clone();
        }
        self.in_flight = cut.in_flight.clone();
        self.players.extend(cut.players.iter().cloned());
        if cut.history.is_some() {
            self.history = cut.history.clone();
        }
        self.tick = cut.tick;
    }
}
