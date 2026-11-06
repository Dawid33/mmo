//! Player edits, applied at the start of a step inside its undo log.

use crate::body::{self, layer_members};
use crate::grid::{ALL, Dir};
use crate::state::{NO_BODY, World};
use crate::FULL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    Remove(u32),
    Place(u32),
    Pour(u32, u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    NotSolid,
    Solid,
    NoRoom,
}

pub fn apply(w: &mut World, e: Edit) -> Result<(), EditError> {
    match e {
        Edit::Remove(i) => remove_solid(w, i),
        Edit::Place(i) => place_solid(w, i),
        Edit::Pour(i, q) => pour(w, i, q),
    }
}

fn wake(w: &mut World, i: u32) {
    if w.is_ca(i) {
        w.set_active(i, true);
        w.touch(i);
    }
}

fn remove_solid(w: &mut World, i: u32) -> Result<(), EditError> {
    if !w.solid.get(i) {
        return Err(EditError::NotSolid);
    }
    w.set_solid(i, false);
    wake(w, i);
    for d in ALL {
        if let Some(n) = w.dims.step(i, d) {
            wake(w, n);
            let o = w.owner[n as usize];
            if o != NO_BODY {
                let b = w.body_mut(o);
                if let Err(p) = b.shore.binary_search(&i) {
                    b.shore.insert(p, i);
                }
            }
        }
    }
    Ok(())
}

fn place_solid(w: &mut World, i: u32) -> Result<(), EditError> {
    if w.solid.get(i) {
        return Err(EditError::Solid);
    }
    match w.owner[i as usize] {
        NO_BODY => {
            let wi = w.water[i as usize];
            if wi > 0 {
                let target = [Dir::Up, Dir::PosX, Dir::PosZ, Dir::NegX, Dir::NegZ]
                    .into_iter()
                    .filter_map(|d| w.dims.step(i, d))
                    .find(|&n| w.is_ca(n) && FULL - w.water[n as usize] >= wi);
                let Some(n) = target else { return Err(EditError::NoRoom) };
                let wn = w.water[n as usize];
                w.set_water(n, wn + wi);
                w.set_water(i, 0);
                wake(w, n);
            }
            w.set_active(i, false);
        }
        id => {
            let (cap, vol, top) = {
                let b = &w.bodies[&id];
                (b.capacity(), b.volume, b.top)
            };
            // water that no longer fits once this cell's capacity is gone
            let overflow = (vol + FULL as u64).saturating_sub(cap);
            let dest = if overflow > 0 {
                let d = layer_members(w, id, top)
                    .into_iter()
                    .filter_map(|m| w.dims.step(m, Dir::Up))
                    .find(|&u| w.is_ca(u) && ((FULL - w.water[u as usize]) as u64) >= overflow);
                match d {
                    Some(u) => Some(u),
                    None => return Err(EditError::NoRoom),
                }
            } else {
                None
            };
            let y = w.dims.y_of(i);
            w.set_owner(i, NO_BODY);
            {
                let b = w.body_mut(id);
                b.area[y as usize] -= 1;
                b.cells -= 1;
                if y < b.top {
                    b.below -= 1;
                }
                b.volume -= overflow;
            }
            if let Some(u) = dest {
                let wu = w.water[u as usize];
                w.set_water(u, wu + overflow as u16);
                wake(w, u);
            }
            body::drop_empty_layers(w, id);
            if w.bodies.contains_key(&id) {
                body::split_if_disconnected(w, id);
            }
        }
    }
    w.set_solid(i, true);
    for d in ALL {
        if let Some(n) = w.dims.step(i, d) {
            wake(w, n);
        }
    }
    Ok(())
}

fn pour(w: &mut World, i: u32, q: u16) -> Result<(), EditError> {
    if w.solid.get(i) {
        return Err(EditError::Solid);
    }
    match w.owner[i as usize] {
        NO_BODY => {
            let wi = w.water[i as usize];
            if FULL - wi < q {
                return Err(EditError::NoRoom);
            }
            w.set_water(i, wi + q);
            wake(w, i);
        }
        id => {
            if w.bodies[&id].room() < q as u64 {
                return Err(EditError::NoRoom);
            }
            body::add_volume(w, id, q as u64);
        }
    }
    w.add_poured(q as u64);
    Ok(())
}
