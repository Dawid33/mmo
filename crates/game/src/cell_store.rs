//! In-memory dormant-cell store (spec §3, hot layer only; Postgres is phase 5).
//! Server-side: the source of every activation payload.

use std::collections::BTreeMap;

use crate::{
    cell_of_chunk, cells_per_side, CellCoords, CellPayload, Chunk, ChunkCoords, Column,
    ColumnCoords, DormantCell, EntityBundle, TerrainSnapshot,
};

#[derive(Debug, Clone)]
struct StoredCell {
    version: u64,
    content: DormantCell,
}

/// Generates one cell's worldgen columns (region-local coordinates) on
/// demand — `worldgen::generate_cell` bound to a region, seed and materials.
pub type CellGenerator = Box<dyn Fn(CellCoords) -> Vec<(ColumnCoords, Column)> + Send>;

pub struct CellStore {
    /// Materialized cells. Eager stores hold every grid cell; lazy stores
    /// only those generated (or written back) so far.
    cells: BTreeMap<CellCoords, StoredCell>,
    /// Cells per region edge.
    side: u32,
    /// Lazy stores: generates a missing in-grid cell on first request.
    generate: Option<CellGenerator>,
}

impl std::fmt::Debug for CellStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellStore")
            .field("materialized", &self.cells.len())
            .field("side", &self.side)
            .field("lazy", &self.generate.is_some())
            .finish()
    }
}

impl CellStore {
    /// Every grid cell gets an entry (possibly empty); chunks land sorted in
    /// their cell.
    pub fn from_chunks(chunks: Vec<(ChunkCoords, Chunk)>, cell_chunks: usize) -> Self {
        let n = cells_per_side(cell_chunks);
        let mut cells: BTreeMap<CellCoords, StoredCell> = BTreeMap::new();
        for x in 0..n {
            for z in 0..n {
                cells.insert(
                    CellCoords::new(x, z),
                    StoredCell { version: 0, content: DormantCell::default() },
                );
            }
        }
        for (coords, chunk) in chunks {
            let cell = cell_of_chunk(coords, cell_chunks);
            cells
                .get_mut(&cell)
                .expect("chunk inside the region")
                .content
                .chunks
                .push((coords, chunk));
        }
        for c in cells.values_mut() {
            c.content.chunks.sort_by_key(|(k, _)| (k.x, k.y, k.z));
        }
        Self { cells, side: n, generate: None }
    }

    /// A store that generates each cell the first time it is needed — the
    /// whole region is never built up front (large regions: 2 km+ would take
    /// minutes and gigabytes).
    pub fn lazy(cell_chunks: usize, generate: CellGenerator) -> Self {
        Self { cells: BTreeMap::new(), side: cells_per_side(cell_chunks), generate: Some(generate) }
    }

    fn in_grid(&self, cell: CellCoords) -> bool {
        cell.x < self.side && cell.z < self.side
    }

    /// Make sure `cell` is materialized (lazy stores generate it now).
    fn materialize(&mut self, cell: CellCoords) -> Option<&mut StoredCell> {
        if !self.cells.contains_key(&cell) {
            if !self.in_grid(cell) {
                return None;
            }
            let generate = self.generate.as_ref()?;
            let mut chunks = TerrainSnapshot::from_columns(generate(cell)).chunks;
            chunks.sort_by_key(|(k, _)| (k.x, k.y, k.z));
            self.cells.insert(
                cell,
                StoredCell { version: 0, content: DormantCell { chunks, entities: Vec::new() } },
            );
        }
        self.cells.get_mut(&cell)
    }

    /// Cells materialized so far.
    pub fn generated(&self) -> usize {
        self.cells.len()
    }

    /// Worldgen output for a whole region, split into cells.
    pub fn from_columns(columns: Vec<(ColumnCoords, Column)>, cell_chunks: usize) -> Self {
        Self::from_chunks(TerrainSnapshot::from_columns(columns).chunks, cell_chunks)
    }

    /// The activation payload for `cell` (hash left 0 for the server sim to fill).
    pub fn payload(&mut self, cell: CellCoords) -> Option<CellPayload> {
        self.materialize(cell).map(|s| CellPayload {
            cell,
            version: s.version,
            materialized_hash: 0,
            content: s.content.clone(),
        })
    }

    /// Store a deactivated cell's content; bumps the version iff it changed.
    pub fn put(&mut self, cell: CellCoords, content: DormantCell) {
        let entry = self
            .cells
            .entry(cell)
            .or_insert(StoredCell { version: 0, content: DormantCell::default() });
        if entry.content.content_crc() != content.content_crc() {
            entry.version += 1;
            entry.content = content;
        }
    }

    /// A stray entity frozen into a dormant cell.
    pub fn add_entity(&mut self, cell: CellCoords, bundle: EntityBundle) {
        if let Some(entry) = self.materialize(cell) {
            entry.content.entities.push(bundle);
            entry.version += 1;
        } else {
            log::error!("[cells] frozen entity for unknown cell {cell:?} dropped");
        }
    }

    /// Chunks of every stored cell NOT in `active` (active cells' truth is
    /// the sim), for whole-region terrain views such as LOD.
    pub fn dormant_chunks(
        &self,
        active: &std::collections::BTreeSet<CellCoords>,
    ) -> Vec<(ChunkCoords, Chunk)> {
        self.cells
            .iter()
            .filter(|(c, _)| !active.contains(c))
            .flat_map(|(_, s)| s.content.chunks.iter().cloned())
            .collect()
    }

    pub fn version(&self, cell: CellCoords) -> Option<u64> {
        self.cells.get(&cell).map(|s| s.version)
    }

    /// Grid cells (materialized or not).
    pub fn len(&self) -> usize {
        (self.side * self.side) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks() -> Vec<(ChunkCoords, Chunk)> {
        vec![
            (ChunkCoords::new(9, 0, 1), Chunk::flat_floor(8)),
            (ChunkCoords::new(0, 0, 0), Chunk::flat_floor(4)),
            (ChunkCoords::new(1, -1, 0), Chunk::flat_floor(2)),
        ]
    }

    #[test]
    fn every_grid_cell_exists_and_chunks_land_sorted_in_their_cell() {
        let mut s = CellStore::from_chunks(chunks(), 8);
        assert_eq!(s.len(), 16);
        let p = s.payload(CellCoords::new(0, 0)).unwrap();
        let coords: Vec<_> = p.content.chunks.iter().map(|(c, _)| *c).collect();
        assert_eq!(coords, vec![ChunkCoords::new(0, 0, 0), ChunkCoords::new(1, -1, 0)]);
        assert_eq!(s.payload(CellCoords::new(1, 0)).unwrap().content.chunks.len(), 1);
        assert!(s.payload(CellCoords::new(3, 3)).unwrap().content.chunks.is_empty());
        assert_eq!(p.materialized_hash, 0);
        assert_eq!(p.version, 0);
    }

    #[test]
    fn put_bumps_the_version_only_on_change() {
        let mut s = CellStore::from_chunks(chunks(), 8);
        let c = CellCoords::new(0, 0);
        let same = s.payload(c).unwrap().content;
        s.put(c, same);
        assert_eq!(s.version(c), Some(0));
        s.put(c, DormantCell::default());
        assert_eq!(s.version(c), Some(1));
    }

    fn counting_store(cell_chunks: usize) -> (CellStore, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let store = CellStore::lazy(
            cell_chunks,
            Box::new(move |cell: CellCoords| {
                c.fetch_add(1, Ordering::SeqCst);
                let x = cell.x as usize * cell_chunks;
                let z = cell.z as usize * cell_chunks;
                let sub = crate::SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
                let air = crate::Block::new(crate::BlockId::AIR);
                vec![(ColumnCoords::new(x, z), crate::Column::new(0, air, air, vec![sub]))]
            }),
        );
        (store, calls)
    }

    #[test]
    fn a_lazy_store_generates_only_the_cells_asked_for() {
        use std::sync::atomic::Ordering;
        let (mut s, calls) = counting_store(8);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing generated up front");
        let p = s.payload(CellCoords::new(2, 1)).expect("inside the grid");
        assert_eq!(p.content.chunks.len(), 1);
        assert_eq!(p.content.chunks[0].0, ChunkCoords::new(16, 0, 8));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        s.payload(CellCoords::new(2, 1));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "generated once, then stored");
        assert_eq!(s.generated(), 1);
    }

    #[test]
    fn dormant_chunks_never_force_generation() {
        use std::sync::atomic::Ordering;
        let (mut s, calls) = counting_store(8);
        assert!(s.dormant_chunks(&Default::default()).is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        s.payload(CellCoords::new(0, 0));
        assert_eq!(s.dormant_chunks(&Default::default()).len(), 1);
    }

    #[test]
    fn a_lazy_store_has_nothing_outside_the_grid() {
        let (mut s, _) = counting_store(8);
        assert!(s.payload(CellCoords::new(99, 0)).is_none());
    }

    #[test]
    fn unknown_cells_have_no_payload() {
        let mut s = CellStore::from_chunks(chunks(), 8);
        assert!(s.payload(CellCoords::new(9, 9)).is_none());
    }
}
