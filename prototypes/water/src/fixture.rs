//! Terrain and water builders. They write state directly, without the undo log:
//! a fixture is the initial snapshot, not a step.

use std::ops::RangeInclusive as R;

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
