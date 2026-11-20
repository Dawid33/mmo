//! The world map's tile pyramid (spec `2026-11-19-block-level-world-map-design.md` §5.1): the key, one
//! sample, a tile and its wire codec, and one chunk's own top per block column (the store writes it beside
//! the chunk, `chunks.top`). Level `k` is 2^k m per sample; a tile is [`MAP_TILE_SAMPLES`]² samples, its grid
//! is NOT the region grid ([`MapTileKey::regions`]). Nothing here is simulated, hashed or saved in a cut but
//! [`ChunkTop`].

use serde::{Deserialize, Serialize};

use crate::{
    BLOCK_VOXELS, BlockId, CHUNK_BLOCKS, Chunk, PacketError, REGION_BLOCKS, RegionCoords, WorldMap,
    decode_packet, encode_packet,
};

/// Samples along a tile's edge.
pub const MAP_TILE_SAMPLES: u32 = 256;

const EDGE: usize = MAP_TILE_SAMPLES as usize;
const SAMPLES: usize = EDGE * EDGE;

/// The highest level a key may have: 2^40 m per sample is past any world.
pub const MAP_LEVEL_LIMIT: u8 = 40;

/// One tile of the pyramid: level `level` (2^level m per sample), covering blocks
/// `[x·256·2^level, (x+1)·256·2^level)` (and the same in z).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MapTileKey {
    pub level: u8,
    pub x: i32,
    pub z: i32,
}

impl MapTileKey {
    /// Metres (blocks) per sample.
    pub fn sample_m(self) -> i64 {
        1i64 << self.level
    }

    /// Blocks along the tile's edge.
    pub fn span_blocks(self) -> i64 {
        MAP_TILE_SAMPLES as i64 * self.sample_m()
    }

    /// The tile's min block (x, z).
    pub fn origin_block(self) -> (i64, i64) {
        let span = self.span_blocks();
        (self.x as i64 * span, self.z as i64 * span)
    }

    /// The tile of `level` holding block `(bx, bz)` (floors toward −∞).
    pub fn containing(level: u8, bx: i64, bz: i64) -> MapTileKey {
        let span = MAP_TILE_SAMPLES as i64 * (1i64 << level);
        MapTileKey {
            level,
            x: bx.div_euclid(span) as i32,
            z: bz.div_euclid(span) as i32,
        }
    }

    /// The tile one level up holding this one (`None` at [`MAP_LEVEL_LIMIT`]).
    pub fn parent(self) -> Option<MapTileKey> {
        (self.level < MAP_LEVEL_LIMIT).then(|| MapTileKey {
            level: self.level + 1,
            x: self.x.div_euclid(2),
            z: self.z.div_euclid(2),
        })
    }

    /// Every region the tile overlaps (derived from `REGION_BLOCKS`; unwrapped coordinates).
    pub fn regions(self) -> impl Iterator<Item = RegionCoords> {
        let (ox, oz) = self.origin_block();
        let span = self.span_blocks();
        let rb = REGION_BLOCKS as i64;
        let (x0, x1) = (ox.div_euclid(rb), (ox + span - 1).div_euclid(rb));
        let (z0, z1) = (oz.div_euclid(rb), (oz + span - 1).div_euclid(rb));
        (z0..=z1).flat_map(move |z| (x0..=x1).map(move |x| RegionCoords::new(x as i32, z as i32)))
    }
}

/// The top level of the world's pyramid: the first whose tile edge covers the world's longer side (the
/// tile grid is aligned at 0, so the whole world then lies within at most 2 × 2 tiles of it).
pub fn top_level(map: &WorldMap) -> u8 {
    top_level_of(map.extent())
}

/// [`top_level`] of an extent.
pub fn top_level_of(e: crate::MapExtent) -> u8 {
    let side = e.width.max(e.height).max(1) as i64 * REGION_BLOCKS as i64;
    (0..MAP_LEVEL_LIMIT)
        .find(|&level| (MAP_TILE_SAMPLES as i64) << level >= side)
        .unwrap_or(MAP_LEVEL_LIMIT)
}

/// Bits of [`MapSample::flags`].
pub mod sample_flags {
    /// The water is frozen (glacier or sea ice).
    pub const ICE: u8 = 1;
    /// From stored terrain, not worldgen.
    pub const REAL: u8 = 2;
    /// Past the world's edge: drawn as the background.
    pub const OUTSIDE: u8 = 4;
}

/// One sample: the top block's top (m), the top block, water depth over it (m, saturating) and flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MapSample {
    pub height: i16,
    pub block: BlockId,
    pub water: u8,
    pub flags: u8,
}

/// A tile: [`MAP_TILE_SAMPLES`]² samples, row-major (z, then x).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapTile {
    pub key: MapTileKey,
    pub samples: Vec<MapSample>,
}

/// Why tile bytes did not decode.
#[derive(Debug)]
pub enum MapTileDecodeError {
    Packet(PacketError),
    /// A plane of the wrong length.
    Length,
}

impl std::fmt::Display for MapTileDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MapTileDecodeError::Packet(e) => write!(f, "map tile: {e}"),
            MapTileDecodeError::Length => write!(f, "map tile: a plane of the wrong length"),
        }
    }
}

impl std::error::Error for MapTileDecodeError {}

impl From<PacketError> for MapTileDecodeError {
    fn from(e: PacketError) -> Self {
        MapTileDecodeError::Packet(e)
    }
}

/// The wire form: three planes, each its own `encode_packet` frame — heights delta-coded along each row
/// (i16 LE), blocks (u16 LE), then water and flags (u8 each, all water first).
#[derive(Serialize, Deserialize)]
struct TileWire {
    key: MapTileKey,
    height: Vec<u8>,
    block: Vec<u8>,
    water_flags: Vec<u8>,
}

fn frame(bytes: &[u8]) -> Vec<u8> {
    encode_packet(&bytes).expect("a byte slice always serialises")
}

impl MapTile {
    /// An empty tile: every sample default.
    pub fn blank(key: MapTileKey) -> MapTile {
        MapTile {
            key,
            samples: vec![MapSample::default(); SAMPLES],
        }
    }

    /// The sample at column `sx`, row `sz` (each `0..MAP_TILE_SAMPLES`).
    pub fn at(&self, sx: usize, sz: usize) -> MapSample {
        self.samples[sx + EDGE * sz]
    }

    pub fn encode(&self) -> Vec<u8> {
        assert_eq!(
            self.samples.len(),
            SAMPLES,
            "a map tile holds MAP_TILE_SAMPLES² samples"
        );
        let mut height = Vec::with_capacity(SAMPLES * 2);
        for row in self.samples.chunks_exact(EDGE) {
            let mut prev = 0i16;
            for s in row {
                height.extend_from_slice(&s.height.wrapping_sub(prev).to_le_bytes());
                prev = s.height;
            }
        }
        let block: Vec<u8> = self
            .samples
            .iter()
            .flat_map(|s| s.block.0.to_le_bytes())
            .collect();
        let water_flags: Vec<u8> = self
            .samples
            .iter()
            .map(|s| s.water)
            .chain(self.samples.iter().map(|s| s.flags))
            .collect();
        let wire = TileWire {
            key: self.key,
            height: frame(&height),
            block: frame(&block),
            water_flags: frame(&water_flags),
        };
        encode_packet(&wire).expect("a map tile always serialises")
    }

    pub fn decode(bytes: &[u8]) -> Result<MapTile, MapTileDecodeError> {
        let wire: TileWire = decode_packet(bytes)?;
        let height: Vec<u8> = decode_packet(&wire.height)?;
        let block: Vec<u8> = decode_packet(&wire.block)?;
        let water_flags: Vec<u8> = decode_packet(&wire.water_flags)?;
        if height.len() != SAMPLES * 2
            || block.len() != SAMPLES * 2
            || water_flags.len() != SAMPLES * 2
        {
            return Err(MapTileDecodeError::Length);
        }
        let mut samples = Vec::with_capacity(SAMPLES);
        for (r, row) in height.chunks_exact(EDGE * 2).enumerate() {
            let mut prev = 0i16;
            for (c, h) in row.chunks_exact(2).enumerate() {
                let i = r * EDGE + c;
                prev = prev.wrapping_add(i16::from_le_bytes([h[0], h[1]]));
                samples.push(MapSample {
                    height: prev,
                    block: BlockId(u16::from_le_bytes([block[2 * i], block[2 * i + 1]])),
                    water: water_flags[i],
                    flags: water_flags[SAMPLES + i],
                });
            }
        }
        Ok(MapTile {
            key: wire.key,
            samples,
        })
    }
}

/// One chunk's own top per block column (`bx + CHUNK_BLOCKS * bz`): the top face of its topmost shown
/// voxel at the column's centre, in voxels above the chunk's floor (`1..=CHUNK_UNITS`), and that voxel's
/// block; `None` = nothing in this chunk's column. A slope counts as a full block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkTop(pub Vec<Option<(u16, BlockId)>>);

/// [`ChunkTop`] of `chunk`, straight from its blocks and chisel.
pub fn chunk_top(chunk: &Chunk) -> ChunkTop {
    let mut tops = vec![None; CHUNK_BLOCKS * CHUNK_BLOCKS];
    if chunk.is_all_air() {
        return ChunkTop(tops);
    }
    let centre = BLOCK_VOXELS / 2;
    for bz in 0..CHUNK_BLOCKS {
        for bx in 0..CHUNK_BLOCKS {
            let (vx, vz) = (bx * BLOCK_VOXELS + centre, bz * BLOCK_VOXELS + centre);
            tops[bx + CHUNK_BLOCKS * bz] = chunk
                .column_top(vx as u32, vz as u32)
                .map(|(y, id)| ((y + 1) as u16, id));
        }
    }
    // Slopes count as full blocks (the centre-column sample is half height on some facings).
    for (bi, chisel) in &chunk.chisel {
        if crate::slope::classify(chisel).is_none() {
            continue;
        }
        let id = chunk.blocks.get(*bi).id;
        if id == BlockId::AIR {
            continue;
        }
        let (bx, by, bz) = bi.xyz();
        let column = bx + CHUNK_BLOCKS * bz;
        let top = ((by + 1) * BLOCK_VOXELS) as u16;
        if tops[column].is_none_or(|(y, _)| y < top) {
            tops[column] = Some((top, id));
        }
    }
    ChunkTop(tops)
}
