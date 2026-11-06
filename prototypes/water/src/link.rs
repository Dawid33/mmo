//! Links: point-to-point water transfers between two cells (gates, pumps) under one shared
//! edge rule. All wants are computed from the state before the phase, capped (destination room,
//! then proportional sharing of a scarce source), then applied in ascending link id through
//! `take`/`give`, which reuse the logged body/CA mutation paths.
//!
//! Apply rule (cap-before-take): each amount is first capped by the destination's CURRENT room,
//! then taken from the PLANNED source (a CA cell, or a body by id even if an earlier link dropped
//! its top layer or split it), then given in full. Nothing is ever given back: taking from one
//! source cannot change another source's destination room, because the link phase only shrinks
//! or splits bodies, never merges or expands them.

use std::collections::BTreeMap;

use crate::body;
use crate::state::{NO_BODY, World};
use crate::{FULL, GATE_CAP};

pub type LinkId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Gate { open: bool },
    Pump { rate: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub a: u32,
    pub b: u32,
    pub kind: LinkKind,
}

/// Surface height at `i` in units, `None` for a solid cell.
pub fn head(w: &World, i: u32) -> Option<u64> {
    if w.solid.get(i) {
        return None;
    }
    match w.owner[i as usize] {
        NO_BODY => Some(w.dims.y_of(i) as u64 * FULL as u64 + w.water[i as usize] as u64),
        id => Some(w.bodies[&id].level()),
    }
}

/// Water the cell (or its body) can give.
pub fn avail(w: &World, i: u32) -> u64 {
    if w.solid.get(i) {
        return 0;
    }
    match w.owner[i as usize] {
        NO_BODY => w.water[i as usize] as u64,
        id => w.bodies[&id].volume,
    }
}

/// Water the cell (or its body) can take.
pub fn room(w: &World, i: u32) -> u64 {
    if w.solid.get(i) {
        return 0;
    }
    match w.owner[i as usize] {
        NO_BODY => (FULL - w.water[i as usize]) as u64,
        id => w.bodies[&id].room(),
    }
}

/// Remove up to `q` from the cell (or its body); returns what was taken.
pub fn take(w: &mut World, i: u32, q: u64) -> u64 {
    if w.solid.get(i) {
        return 0;
    }
    match w.owner[i as usize] {
        NO_BODY => {
            let q = q.min(w.water[i as usize] as u64);
            if q == 0 {
                return 0;
            }
            let v = w.water[i as usize];
            w.set_water(i, v - q as u16);
            crate::ca::wake(w, i);
            crate::ca::wake_donors(w, i);
            q
        }
        id => take_body(w, id, q),
    }
}

/// Remove up to `q` from body `id` as a whole.
pub(crate) fn take_body(w: &mut World, id: u32, q: u64) -> u64 {
    let Some(b) = w.bodies.get(&id) else { return 0 };
    let q = q.min(b.volume);
    if q == 0 {
        return 0;
    }
    body::remove_volume(w, id, q);
    // a lowered level lets wet neighbours at the shore flow in
    if let Some(b) = w.bodies.get(&id) {
        let shore = b.shore.clone();
        for s in shore {
            if w.is_ca(s) && w.water[s as usize] > 0 {
                crate::ca::wake(w, s);
            }
        }
    }
    q
}

/// Take from the source as planned: a CA cell, or its body by id. If the body has since split,
/// from the part that holds `cell`; if it was removed, nothing.
pub(crate) fn take_planned(w: &mut World, key: (u8, u32), cell: u32, q: u64) -> u64 {
    match key {
        (0, _) => take(w, cell, q),
        (_, id) => match w.owner[cell as usize] {
            NO_BODY => take_body(w, id, q),
            part => take_body(w, part, q),
        },
    }
}

/// Add up to `q` to the cell (or its body, never expanding it); returns what was added.
pub fn give(w: &mut World, i: u32, q: u64) -> u64 {
    let q = q.min(room(w, i));
    if q == 0 {
        return 0;
    }
    match w.owner[i as usize] {
        NO_BODY => {
            let v = w.water[i as usize];
            w.set_water(i, v + q as u16);
            crate::ca::wake(w, i);
        }
        id => body::add_volume(w, id, q),
    }
    q
}

/// A source is a CA cell or a whole body.
pub(crate) fn source_key(w: &World, i: u32) -> (u8, u32) {
    match w.owner[i as usize] {
        NO_BODY => (0, i),
        id => (1, id),
    }
}

pub fn flux_all(w: &mut World) {
    w.stats.link_moved_last_step = 0;
    if w.links.is_empty() {
        return;
    }
    // 1-2: wants from the pre-phase state, capped by destination room
    let mut wants: Vec<(u32, u32, u64, (u8, u32))> = Vec::new(); // (src, dst, q, planned source)
    for l in w.links.values() {
        let (src, dst, q) = match l.kind {
            LinkKind::Gate { open: false } => continue,
            LinkKind::Gate { open: true } => {
                let (Some(ha), Some(hb)) = (head(w, l.a), head(w, l.b)) else { continue };
                if ha >= hb {
                    (l.a, l.b, GATE_CAP.min((ha - hb) / 2))
                } else {
                    (l.b, l.a, GATE_CAP.min((hb - ha) / 2))
                }
            }
            LinkKind::Pump { rate } => (l.a, l.b, rate as u64),
        };
        if source_key(w, src) == source_key(w, dst) {
            continue;
        }
        let q = q.min(room(w, dst));
        if q > 0 {
            wants.push((src, dst, q, source_key(w, src)));
        }
    }
    // 3: a source asked for more than it has shares it proportionally
    let mut out: BTreeMap<(u8, u32), u64> = BTreeMap::new();
    for &(_, _, q, k) in &wants {
        *out.entry(k).or_default() += q;
    }
    for m in &mut wants {
        let total = out[&m.3];
        let have = avail(w, m.0);
        if total > have {
            m.2 = m.2 * have / total;
        }
    }
    // 4: apply in link order, cap-before-take
    let mut moved = 0;
    for (src, dst, q, key) in wants {
        if q == 0 {
            continue;
        }
        let q = q.min(room(w, dst));
        let got = take_planned(w, key, src, q);
        let given = give(w, dst, got);
        debug_assert_eq!(given, got, "link: destination refused water it had room for");
        moved += given;
    }
    w.stats.link_moved_last_step = moved;
}
