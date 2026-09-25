//! Cell loading inside one region (spec:
//! docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md).
//! A cell is a `cell_chunks` × `cell_chunks` square of chunk columns; the
//! server activates cells around players through `CellChanges` events.

use std::collections::BTreeSet;

use crate::{Chunk, ChunkCoords, EntityBundle, REGION_CHUNKS, REGION_SIZE};

/// Region-local cell address, `0..cells_per_side` on each axis.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash,
    serde::Serialize, serde::Deserialize,
)]
pub struct CellCoords {
    pub x: u32,
    pub z: u32,
}

impl CellCoords {
    pub fn new(x: u32, z: u32) -> Self {
        Self { x, z }
    }
}

/// Cells along one region edge. Panics unless `cell_chunks` is a power of
/// two that divides `REGION_CHUNKS`.
pub fn cells_per_side(cell_chunks: usize) -> u32 {
    assert!(
        cell_chunks.is_power_of_two() && cell_chunks <= REGION_CHUNKS,
        "cell_chunks {cell_chunks} must be a power of two dividing REGION_CHUNKS {REGION_CHUNKS}"
    );
    (REGION_CHUNKS / cell_chunks) as u32
}

/// Cell edge in world units (32 units per chunk).
pub fn cell_size_units(cell_chunks: usize) -> f32 {
    (cell_chunks * 32) as f32
}

pub fn cell_of_chunk(c: ChunkCoords, cell_chunks: usize) -> CellCoords {
    CellCoords::new((c.x / cell_chunks) as u32, (c.z / cell_chunks) as u32)
}

/// The cell containing region-local `(x, z)`, `None` outside the region.
/// `floor` semantics: a point exactly on an edge belongs to the higher cell.
pub fn cell_of_pos(x: f32, z: f32, cell_chunks: usize) -> Option<CellCoords> {
    if !(0.0..REGION_SIZE).contains(&x) || !(0.0..REGION_SIZE).contains(&z) {
        return None;
    }
    let size = cell_size_units(cell_chunks);
    Some(CellCoords::new((x / size).floor() as u32, (z / size).floor() as u32))
}

/// The cell containing block `pos` (block units), `None` outside the region.
pub fn cell_of_block(pos: [i32; 3], cell_chunks: usize) -> Option<CellCoords> {
    let cb = crate::CHUNK_BLOCKS as i32;
    let (cx, cz) = (pos[0].div_euclid(cb), pos[2].div_euclid(cb));
    let n = REGION_CHUNKS as i32;
    if cx < 0 || cz < 0 || cx >= n || cz >= n {
        return None;
    }
    Some(cell_of_chunk(ChunkCoords::new(cx as usize, 0, cz as usize), cell_chunks))
}

pub fn chebyshev(a: CellCoords, b: CellCoords) -> u32 {
    a.x.abs_diff(b.x).max(a.z.abs_diff(b.z))
}

/// Every cell within Chebyshev `radius` of `center`, clipped to the grid,
/// sorted.
pub fn cells_within(center: CellCoords, radius: u32, cell_chunks: usize) -> Vec<CellCoords> {
    let n = cells_per_side(cell_chunks);
    let lo = |v: u32| v.saturating_sub(radius);
    let hi = |v: u32| (v + radius).min(n - 1);
    let mut out = Vec::new();
    for x in lo(center.x)..=hi(center.x) {
        for z in lo(center.z)..=hi(center.z) {
            out.push(CellCoords::new(x, z));
        }
    }
    out
}

/// Sim-side cell bookkeeping (tier-2 field `GameData.cells`). Mutated ONLY
/// raw, inside the `CellChanges` arm, which every replica forgets at once.
#[derive(Debug, Clone, Default, Hash, serde::Serialize, serde::Deserialize)]
pub struct CellState {
    /// 0 = legacy region (fully materialized, no cells).
    pub cell_chunks: usize,
    pub active: BTreeSet<CellCoords>,
}

impl CellState {
    pub fn enabled(&self) -> bool {
        self.cell_chunks != 0
    }
}

/// A cell's full content while it is not in the sim: the server's store
/// entry, and the activation payload clients materialize.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DormantCell {
    /// Sorted by `ChunkCoords`.
    pub chunks: Vec<(ChunkCoords, Chunk)>,
    /// Frozen non-player entities, in extraction order.
    pub entities: Vec<EntityBundle>,
}

impl DormantCell {
    /// crc32 of the bincode encoding — the store's change detector.
    pub fn content_crc(&self) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(&bincode::serialize(self).expect("dormant cell serializes"));
        h.finalize()
    }
}

// `Chunk` has no PartialEq; compare cells by their encoding.
impl PartialEq for DormantCell {
    fn eq(&self, other: &Self) -> bool {
        bincode::serialize(self).ok() == bincode::serialize(other).ok()
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellPayload {
    pub cell: CellCoords,
    /// Store version this payload was cut from.
    pub version: u64,
    /// `Rollback::cell_hash` of the cell as materialized in the SERVER sim.
    /// 0 when the server builds the event; the server's `handle_event`
    /// fills it before broadcast; clients verify it.
    pub materialized_hash: u32,
    pub content: DormantCell,
}

/// The one server-authored cell event per tick: deactivations first, then
/// activations, each sorted by `CellCoords`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellChanges {
    pub deactivate: Vec<CellCoords>,
    pub activate: Vec<CellPayload>,
}

impl CellChanges {
    pub fn is_empty(&self) -> bool {
        self.deactivate.is_empty() && self.activate.is_empty()
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    #[test]
    fn grid_divides_the_region() {
        assert_eq!(cells_per_side(crate::REGION_CHUNKS), 1);
        assert_eq!(cells_per_side(crate::REGION_CHUNKS / 4), 4);
        assert_eq!(cell_size_units(8), 256.0);
    }

    #[test]
    #[should_panic]
    fn non_dividing_cell_size_panics() {
        cells_per_side(3);
    }

    #[test]
    fn chunk_and_position_map_to_the_same_cell() {
        let c = ChunkCoords::new(9, -2, 17);
        assert_eq!(cell_of_chunk(c, 8), CellCoords::new(1, 2));
        // Chunk (9, _, 17) starts at voxel (288, _, 544).
        assert_eq!(cell_of_pos(288.0, 544.0, 8), Some(CellCoords::new(1, 2)));
    }

    #[test]
    fn a_position_exactly_on_a_cell_edge_belongs_to_the_higher_cell() {
        let edge = cell_size_units(8);
        assert_eq!(cell_of_pos(edge, 0.0, 8), Some(CellCoords::new(1, 0)));
        assert_eq!(cell_of_pos(edge - 0.001, 0.0, 8), Some(CellCoords::new(0, 0)));
    }

    #[test]
    fn positions_outside_the_region_have_no_cell() {
        assert_eq!(cell_of_pos(-0.5, 10.0, 8), None);
        assert_eq!(cell_of_pos(crate::REGION_SIZE, 10.0, 8), None);
    }

    #[test]
    fn blocks_map_through_their_chunk() {
        // CHUNK_BLOCKS = 2 blocks per chunk: block 16 is chunk 8 → cell 1 at 8-chunk cells.
        assert_eq!(cell_of_block([16, 5, 0], 8), Some(CellCoords::new(1, 0)));
        assert_eq!(cell_of_block([-1, 5, 0], 8), None);
    }

    #[test]
    fn cells_within_is_clipped_and_sorted() {
        let n = cells_per_side(8);
        let around_corner = cells_within(CellCoords::new(0, 0), 1, 8);
        assert_eq!(
            around_corner,
            vec![CellCoords::new(0, 0), CellCoords::new(0, 1), CellCoords::new(1, 0), CellCoords::new(1, 1)]
        );
        let all = cells_within(CellCoords::new(1, 1), 10, 8);
        assert_eq!(all.len() as u32, n * n);
        assert!(all.windows(2).all(|w| w[0] < w[1]));
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use crate::{Chunk, GameEventKind};

    fn sample() -> CellChanges {
        CellChanges {
            deactivate: vec![CellCoords::new(2, 3)],
            activate: vec![CellPayload {
                cell: CellCoords::new(0, 0),
                version: 4,
                materialized_hash: 0,
                content: DormantCell {
                    chunks: vec![(ChunkCoords::new(1, 0, 1), Chunk::flat_floor(8))],
                    entities: vec![],
                },
            }],
        }
    }

    #[test]
    fn cell_changes_are_a_server_event() {
        assert_eq!(GameEventKind::CellChanges(sample()).origin_client(), None);
    }

    #[test]
    fn cell_changes_round_trip_through_the_wire_codec() {
        let kind = GameEventKind::CellChanges(sample());
        let bytes = bincode::serialize(&kind).unwrap();
        let back: GameEventKind = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn content_crc_tracks_content() {
        let a = sample().activate.remove(0).content;
        let mut b = a.clone();
        assert_eq!(a.content_crc(), b.content_crc());
        b.chunks.clear();
        assert_ne!(a.content_crc(), b.content_crc());
        assert!(CellChanges::default().is_empty());
    }
}
