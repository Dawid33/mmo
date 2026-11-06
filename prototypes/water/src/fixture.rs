//! Terrain and water builders. They write state directly, without the undo log:
//! a fixture is the initial snapshot, not a step.

use std::ops::RangeInclusive as R;

use crate::grid::Dims;
use crate::state::World;
use crate::{FULL, Mode, body};

pub fn fill_solid(w: &mut World, ys: R<u32>) {
    for y in ys {
        for z in 0..w.dims.z {
            for x in 0..w.dims.x {
                let i = w.dims.idx(x, y, z);
                w.solid.set(i, true);
            }
        }
    }
}

pub fn carve(w: &mut World, xs: R<u32>, ys: R<u32>, zs: R<u32>) {
    for y in ys {
        for z in zs.clone() {
            for x in xs.clone() {
                let i = w.dims.idx(x, y, z);
                w.solid.set(i, false);
            }
        }
    }
}

pub fn fill_water(w: &mut World, xs: R<u32>, ys: R<u32>, zs: R<u32>) {
    for y in ys {
        for z in zs.clone() {
            for x in xs.clone() {
                let i = w.dims.idx(x, y, z);
                assert!(!w.solid.get(i), "fill_water into solid at {x},{y},{z}");
                w.water[i as usize] = FULL;
            }
        }
    }
}

/// Freeze the fixture. In Bodies mode, standing water becomes bodies. Every cell
/// starts asleep and stats start at zero (a settled world costs nothing).
pub fn finish(w: &mut World) {
    if w.mode == Mode::Bodies {
        body::settle_init(w);
    }
    for i in w.active.ones() {
        w.active.set(i, false);
    }
    w.stats = Default::default();
}

/// Two stepped basins, finished: lake A (x4..=19, floor y12, water y12..=17) beside an empty
/// basin B (x21..=44, floor y4) with a one-cell wall at x = 20. The gate cells are
/// `(19, 12, 11)` in A and `(21, 12, 11)` in B.
pub fn gate_basins(mode: Mode) -> World {
    let mut w = World::new(Dims::cube(64), mode);
    fill_solid(&mut w, 0..=20);
    carve(&mut w, 4..=19, 12..=20, 4..=19);
    fill_water(&mut w, 4..=19, 12..=17, 4..=19);
    carve(&mut w, 21..=44, 4..=20, 4..=19);
    finish(&mut w);
    w
}
