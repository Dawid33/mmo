//! Hand-built block terrain for the fork's tests: chunks of 1 m blocks, each
//! air, full or partly filled at 1/16 m, turned into the multi-scale `Voxels`
//! cell lists the fork hunks were written against — whole blocks at block
//! scale, full 8³ octants of a partial block at half-block scale, the rest at
//! unit scale. The game's terrain is studs now (`stud_voxels.rs` pins the
//! fork on its colliders); these grids stay as the fixture of the
//! mixed-scale seam tests.

use game::{BLOCK_UNITS, CHUNK_BLOCKS};
use rapier3d::math::IVector;

/// Half a block: the middle cell scale.
pub const HALF: usize = BLOCK_UNITS / 2;

/// One block of a [`Chunk`].
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Air,
    Full,
    /// Solidity of its `BLOCK_UNITS`³ unit cells (x fastest, then y, then z).
    Partial(Box<[bool]>),
}

/// `CHUNK_BLOCKS`³ blocks (x fastest, then y, then z).
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    cells: Vec<Cell>,
}

impl Default for Chunk {
    /// All air.
    fn default() -> Self {
        Self { cells: vec![Cell::Air; CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS] }
    }
}

fn index(bx: usize, by: usize, bz: usize) -> usize {
    bx + by * CHUNK_BLOCKS + bz * CHUNK_BLOCKS * CHUNK_BLOCKS
}

fn unit_index(vx: usize, vy: usize, vz: usize) -> usize {
    vx + vy * BLOCK_UNITS + vz * BLOCK_UNITS * BLOCK_UNITS
}

impl Chunk {
    /// Every block full.
    pub fn solid() -> Self {
        Self { cells: vec![Cell::Full; CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS] }
    }

    /// Solid below `depth` units, air above; the block row holding `depth`
    /// (when it is not a block boundary) is partial.
    pub fn flat_floor(depth: u32) -> Self {
        let depth = depth as usize;
        let mut chunk = Self::default();
        for by in 0..CHUNK_BLOCKS {
            let (y0, y1) = (by * BLOCK_UNITS, (by + 1) * BLOCK_UNITS);
            for bx in 0..CHUNK_BLOCKS {
                for bz in 0..CHUNK_BLOCKS {
                    if y1 <= depth {
                        chunk.set(bx, by, bz, Cell::Full);
                    } else if y0 < depth {
                        chunk.set_partial(bx, by, bz, |_, vy, _| vy < depth - y0);
                    }
                }
            }
        }
        chunk
    }

    pub fn get(&self, bx: usize, by: usize, bz: usize) -> &Cell {
        &self.cells[index(bx, by, bz)]
    }

    pub fn set(&mut self, bx: usize, by: usize, bz: usize, cell: Cell) {
        self.cells[index(bx, by, bz)] = cell;
    }

    /// Makes block `(bx, by, bz)` partial: unit cell `(vx, vy, vz)` solid where `f` says so.
    pub fn set_partial(&mut self, bx: usize, by: usize, bz: usize, f: impl Fn(usize, usize, usize) -> bool) {
        let mut units = vec![false; BLOCK_UNITS * BLOCK_UNITS * BLOCK_UNITS];
        for vz in 0..BLOCK_UNITS {
            for vy in 0..BLOCK_UNITS {
                for vx in 0..BLOCK_UNITS {
                    units[unit_index(vx, vy, vz)] = f(vx, vy, vz);
                }
            }
        }
        self.set(bx, by, bz, Cell::Partial(units.into_boxed_slice()));
    }
}

/// A chunk's collider cells, one list per `Voxels` shape.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ColliderCells {
    /// Block-grid cells (`BLOCK_UNITS` units).
    pub blocks: Vec<IVector>,
    /// Half-block cells ([`HALF`] units).
    pub halves: Vec<IVector>,
    /// Unit cells.
    pub voxels: Vec<IVector>,
}

impl ColliderCells {
    /// The non-empty grids as `(cell size in units, keys)`, coarsest first.
    pub fn grids(&self) -> impl Iterator<Item = (f32, &[IVector])> {
        [
            (BLOCK_UNITS as f32, self.blocks.as_slice()),
            (HALF as f32, self.halves.as_slice()),
            (1.0, self.voxels.as_slice()),
        ]
        .into_iter()
        .filter(|(_, cells)| !cells.is_empty())
    }

    /// Every block of the chunk is a block-grid cell.
    pub fn is_full_cube(&self) -> bool {
        self.blocks.len() == CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS
    }
}

/// Each solid unit cell of `chunk` in exactly one cell of the coarsest grid
/// that fits it: a full block, else a full half-block octant, else itself.
pub fn collider_cells(chunk: &Chunk) -> ColliderCells {
    let mut out = ColliderCells::default();
    for (i, cell) in chunk.cells.iter().enumerate() {
        let (bx, by, bz) = (i % CHUNK_BLOCKS, (i / CHUNK_BLOCKS) % CHUNK_BLOCKS, i / (CHUNK_BLOCKS * CHUNK_BLOCKS));
        let block = IVector::new(bx as i32, by as i32, bz as i32);
        let solid = match cell {
            Cell::Air => continue,
            Cell::Full => {
                out.blocks.push(block);
                continue;
            }
            Cell::Partial(solid) => solid,
        };
        if solid.iter().all(|&s| s) {
            out.blocks.push(block);
            continue;
        }
        for (ox, oy, oz) in (0..8).map(|o| (o % 2, (o / 2) % 2, o / 4)) {
            let (x0, y0, z0) = (ox * HALF, oy * HALF, oz * HALF);
            let octant = |f: &mut dyn FnMut(usize, usize, usize)| {
                for vx in x0..x0 + HALF {
                    for vy in y0..y0 + HALF {
                        for vz in z0..z0 + HALF {
                            f(vx, vy, vz);
                        }
                    }
                }
            };
            let mut filled = 0;
            octant(&mut |x, y, z| filled += solid[unit_index(x, y, z)] as usize);
            if filled == HALF * HALF * HALF {
                out.halves.push(IVector::new((bx * 2 + ox) as i32, (by * 2 + oy) as i32, (bz * 2 + oz) as i32));
                continue;
            }
            octant(&mut |x, y, z| {
                if solid[unit_index(x, y, z)] {
                    out.voxels.push(IVector::new(
                        (bx * BLOCK_UNITS + x) as i32,
                        (by * BLOCK_UNITS + y) as i32,
                        (bz * BLOCK_UNITS + z) as i32,
                    ));
                }
            });
        }
    }
    out
}
