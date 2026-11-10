//! Pure derivation of a stud chunk's colliders: a coarse and a fine anisotropic parry `Voxels`
//! grid (solid cells plus stud caps) and one convex hull per non-box part.
//!
//! Grids: fine cell = 1 stud x half plate x 1 stud (8 x 1.6 x 8), coarse cell = 4 studs x 15
//! plates x 4 studs (32 x 48 x 32). Every builder goes through [`stud_collider_shapes`] so
//! genesis and runtime agree on the list and its order.

use rapier3d::math::{IVector, Vector};
use crate::parry::shape::{ConvexPolyhedron, SharedShape};

use crate::stud::{
    plate_units, rotate_point, StudBrick, StudChunk, StudShape, LDU_UNITS, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS, STUD_UNITS,
};

pub const STUD_FINE_CELL: Vector = Vector::new(8.0, 1.6, 8.0);
/// 4 studs x 15 plates x 4 studs.
pub const STUD_COARSE_CELL: Vector = Vector::new(32.0, 48.0, 32.0);

const COARSE_STUDS: i32 = 4;
const COARSE_PLATES: i32 = 15;
const COARSE_X: i32 = STUD_CHUNK_STUDS as i32 / COARSE_STUDS;
const COARSE_Y: i32 = STUD_CHUNK_PLATES as i32 / COARSE_PLATES;

/// Collider cell keys of a chunk, each sorted by `(y, z, x)` without duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StudColliderCells {
    /// Coarse keys `(cx, cy, cz)` in 8 x 6 x 8 of wholly solid coarse cells.
    pub coarse: Vec<IVector>,
    /// Fine keys `(x, 2y, z)` / `(x, 2y+1, z)`: solid cells outside coarse cells, plus stud caps.
    pub fine: Vec<IVector>,
}

impl StudColliderCells {
    pub fn is_empty(&self) -> bool {
        self.coarse.is_empty() && self.fine.is_empty()
    }
}

fn sort_yzx(v: &mut Vec<IVector>) {
    v.sort_by_key(|k| (k.y, k.z, k.x));
    v.dedup();
}

pub fn stud_collider_cells(chunk: &StudChunk) -> StudColliderCells {
    let occ = chunk.occupancy();
    // Solid = cells of box bricks only (hull parts collide through their hull).
    let mut solid = crate::stud::StudOccupancy::default();
    for b in chunk.bricks().filter(|b| b.is_box()) {
        for c in b.cells() {
            solid.set(c.map(i32::from), true);
        }
    }

    let mut out = StudColliderCells::default();
    let mut coarse_full = [[[false; COARSE_X as usize]; COARSE_X as usize]; COARSE_Y as usize];
    for cy in 0..COARSE_Y {
        for cz in 0..COARSE_X {
            for cx in 0..COARSE_X {
                let full = (0..COARSE_PLATES).all(|dy| {
                    (0..COARSE_STUDS).all(|dz| {
                        (0..COARSE_STUDS).all(|dx| {
                            solid.get([
                                cx * COARSE_STUDS + dx,
                                cy * COARSE_PLATES + dy,
                                cz * COARSE_STUDS + dz,
                            ])
                        })
                    })
                });
                if full {
                    coarse_full[cy as usize][cz as usize][cx as usize] = true;
                    out.coarse.push(IVector::new(cx, cy, cz));
                }
            }
        }
    }

    for b in chunk.bricks().filter(|b| b.is_box()) {
        for c in b.cells() {
            let [x, y, z] = c.map(i32::from);
            if coarse_full[(y / COARSE_PLATES) as usize][(z / COARSE_STUDS) as usize][(x / COARSE_STUDS) as usize] {
                continue;
            }
            out.fine.push(IVector::new(x, 2 * y, z));
            out.fine.push(IVector::new(x, 2 * y + 1, z));
        }
    }
    // Caps: a stud is a thin slab on the cell above the brick unless something fills that cell.
    for b in chunk.bricks() {
        for c in b.stud_cells() {
            let c = c.map(i32::from);
            if !occ.get(c) {
                out.fine.push(IVector::new(c[0], 2 * c[1], c[2]));
            }
        }
    }
    sort_yzx(&mut out.coarse);
    sort_yzx(&mut out.fine);
    out
}

/// One point list per non-box brick in `BTreeMap` order, in chunk-local sim units.
pub fn stud_hulls(chunk: &StudChunk) -> Vec<Vec<Vector>> {
    brick_hulls(chunk).into_iter().map(|(_, pts)| pts).collect()
}

/// [`stud_hulls`] paired with the brick each hull belongs to.
fn brick_hulls(chunk: &StudChunk) -> Vec<(&StudBrick, Vec<Vector>)> {
    chunk
        .bricks()
        .filter(|b| !b.is_box())
        .filter_map(|b| {
            let def = b.part()?;
            let StudShape::Part { rot, .. } = b.shape else { return None };
            let size_ldu = [def.size[0] as i32 * 20, 0, def.size[2] as i32 * 20];
            let offset = Vector::new(
                b.min[0] as f32 * STUD_UNITS as f32,
                plate_units(b.min[1] as i32),
                b.min[2] as f32 * STUD_UNITS as f32,
            );
            Some((
                b,
                def.hull
                    .iter()
                    .map(|p| {
                        let r = rotate_point(rot, [p[0] as i32, p[1] as i32, p[2] as i32], size_ldu);
                        Vector::new(r[0] as f32, r[1] as f32, r[2] as f32) * LDU_UNITS + offset
                    })
                    .collect(),
            ))
        })
        .collect()
}

/// The wanted collider list, in fixed order: coarse voxels, fine voxels, then one hull per part.
pub fn stud_collider_shapes(chunk: &StudChunk) -> Vec<SharedShape> {
    let cells = stud_collider_cells(chunk);
    let mut shapes = Vec::new();
    if !cells.coarse.is_empty() {
        shapes.push(SharedShape::voxels(STUD_COARSE_CELL, &cells.coarse));
    }
    if !cells.fine.is_empty() {
        shapes.push(SharedShape::voxels(STUD_FINE_CELL, &cells.fine));
    }
    for (b, pts) in brick_hulls(chunk) {
        match ConvexPolyhedron::from_convex_hull(&pts) {
            Some(h) => shapes.push(SharedShape::new(h)),
            // A brick with no collider is a fall-through: say so.
            None => log::warn!(
                "[stud] convex hull failed: part {} at min {:?} has no hull collider",
                b.part().map_or("?", |d| d.number),
                b.min
            ),
        }
    }
    shapes
}
