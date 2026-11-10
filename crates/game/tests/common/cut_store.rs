//! A stand-in for the world store (Task 6 writes the real one): what the
//! store holds after committing a sequence of save cuts
//! ([`game::WorldCut`]), and the places an entity is saved in.
use game::{
    ChunkCoords, Chunk, EntityId, InFlightRow, PlayerId, PlayerRecord, RegionCoords, StoredRegion, WorldCut,
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
            let mut studs: BTreeMap<_, _> = std::mem::take(&mut stored.stud_chunks).into_iter().collect();
            for (c, record) in &p.stud_chunks {
                match record {
                    Some(r) => studs.insert(*c, r.clone()),
                    None => studs.remove(c),
                };
            }
            stored.stud_chunks = studs.into_iter().collect();
            if let Some(ground) = &p.stud_ground {
                stored.stud_ground = ground.clone();
            }
        }
        self.in_flight = cut.in_flight.clone();
        self.players.extend(cut.players.iter().cloned());
        if cut.history.is_some() {
            self.history = cut.history.clone();
        }
        self.tick = cut.tick;
    }

    /// Where `id` is saved: `(region, "state")` for each stored region whose
    /// entities hold it, `(target, "row")` for each in-flight crossing of it.
    pub fn places(&self, id: EntityId) -> Vec<(RegionCoords, &'static str)> {
        let mut at = Vec::new();
        for (rc, stored) in &self.regions {
            at.extend(stored.entities.iter().filter(|b| b.id == id).map(|_| (*rc, "state")));
        }
        for row in &self.in_flight {
            if let InFlightRow::Transfer { target, bundle, .. } = row {
                if bundle.id == id {
                    at.push((*target, "row"));
                }
            }
        }
        at
    }
}
