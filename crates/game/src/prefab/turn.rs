//! The one rotation convention: +X is east, +Z is south, north is -Z.

use crate::BLOCK_VOXELS;

/// One quarter turn clockwise seen from above maps (x, z) -> (-z, x): north -> east.
/// Turns are taken mod 4.
pub fn turn_xz(x: i32, z: i32, quarter_turns: u8) -> (i32, i32) {
    let (mut x, mut z) = (x, z);
    for _ in 0..quarter_turns % 4 {
        (x, z) = (-z, x);
    }
    (x, z)
}

/// The same turn inside one block's voxels: (x, z) -> (BLOCK_VOXELS-1-z, x) per quarter turn.
pub fn turn_voxel(v: [usize; 3], quarter_turns: u8) -> [usize; 3] {
    let [mut x, y, mut z] = v;
    for _ in 0..quarter_turns % 4 {
        (x, z) = (BLOCK_VOXELS - 1 - z, x);
    }
    [x, y, z]
}
