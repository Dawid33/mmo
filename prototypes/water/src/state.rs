//! The rollback state of one region's water, the per-step undo log, and hashing.
//! Scratch fields (log, imaged, touched, expand_requests, mark) are rebuilt every
//! step and are never hashed; `stats` is diagnostics only.

use std::collections::BTreeMap;

use crate::body::Body;
use crate::grid::{Bits, Dims, Dir};
use crate::{FULL, Mode};

pub type BodyId = u32;
pub const NO_BODY: BodyId = 0;

/// One before-image. `World::undo` replays a step's log in reverse.
#[derive(Clone, Debug)]
pub enum Undo {
    Water(u32, u16),
    Owner(u32, BodyId),
    Solid(u32, bool),
    Active(u32, bool),
    Touch(u32, u32),
    Body(BodyId, Option<Box<Body>>),
    NextId(BodyId),
    Step(u32),
    Poured(u64),
}

impl Undo {
    /// What this entry would cost in a packed log: 1 tag byte + payload.
    pub fn bytes(&self) -> usize {
        match self {
            Undo::Water(..) => 7,
            Undo::Owner(..) | Undo::Touch(..) | Undo::Poured(..) => 9,
            Undo::Solid(..) | Undo::Active(..) => 6,
            Undo::NextId(..) | Undo::Step(..) => 5,
            Undo::Body(_, b) => 5 + b.as_ref().map_or(0, |b| b.byte_size()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub cells_visited: u64,
    pub transfers: u64,
    pub flood_cells: u64,
    pub seeds: u64,
    pub absorbs: u64,
    pub expansions: u64,
    pub expansion_refusals: u64,
    pub layer_drops: u64,
    pub splits: u64,
    pub merges: u64,
}

/// FNV-1a, 64-bit: tiny, dependency-free, identical on every target.
pub struct Fnv(u64);

impl Fnv {
    pub fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    pub fn u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    pub fn u32(&mut self, v: u32) {
        self.u64(v as u64)
    }
    pub fn finish(&self) -> u64 {
        self.0
    }
}

impl Default for Fnv {
    fn default() -> Self {
        Self::new()
    }
}

pub struct World {
    pub dims: Dims,
    pub mode: Mode,
    pub solid: Bits,
    pub water: Vec<u16>,
    pub owner: Vec<BodyId>,
    pub active: Bits,
    pub last_touch: Vec<u32>,
    pub bodies: BTreeMap<BodyId, Body>,
    pub next_id: BodyId,
    pub step: u32,
    /// Units added by `Edit::Pour` since the fixture was built (the mass ledger).
    pub poured: u64,
    pub stats: Stats,
    log: Option<Vec<Undo>>,
    imaged: Vec<BodyId>,
    /// Bodies changed this step, with their volume before the first change.
    pub(crate) touched: Vec<(BodyId, u64)>,
    pub(crate) expand_requests: Vec<BodyId>,
    /// Flood-fill visited marks, generation-stamped (scratch).
    pub(crate) mark: Vec<u32>,
    pub(crate) mark_gen: u32,
}

impl World {
    pub fn new(dims: Dims, mode: Mode) -> Self {
        let n = dims.len();
        World {
            dims,
            mode,
            solid: Bits::new(n),
            water: vec![0; n],
            owner: vec![NO_BODY; n],
            active: Bits::new(n),
            last_touch: vec![0; n],
            bodies: BTreeMap::new(),
            next_id: 1,
            step: 0,
            poured: 0,
            stats: Stats::default(),
            log: None,
            imaged: Vec::new(),
            touched: Vec::new(),
            expand_requests: Vec::new(),
            mark: vec![0; n],
            mark_gen: 0,
        }
    }

    /// Non-solid and not a body member.
    pub fn is_ca(&self, i: u32) -> bool {
        !self.solid.get(i) && self.owner[i as usize] == NO_BODY
    }

    /// Water in `i` can rest: below is the box floor, solid, a member, or a full CA cell.
    pub fn supported(&self, i: u32) -> bool {
        match self.dims.step(i, Dir::Down) {
            None => true,
            Some(b) => {
                self.solid.get(b)
                    || self.owner[b as usize] != NO_BODY
                    || self.water[b as usize] == FULL
            }
        }
    }

    fn push(&mut self, u: Undo) {
        if let Some(log) = &mut self.log {
            log.push(u);
        }
    }

    pub fn set_water(&mut self, i: u32, v: u16) {
        let old = self.water[i as usize];
        if old != v {
            self.push(Undo::Water(i, old));
            self.water[i as usize] = v;
        }
    }
    pub fn set_owner(&mut self, i: u32, v: BodyId) {
        let old = self.owner[i as usize];
        if old != v {
            self.push(Undo::Owner(i, old));
            self.owner[i as usize] = v;
        }
    }
    pub fn set_solid(&mut self, i: u32, v: bool) {
        let old = self.solid.get(i);
        if old != v {
            self.push(Undo::Solid(i, old));
            self.solid.set(i, v);
        }
    }
    pub fn set_active(&mut self, i: u32, v: bool) {
        let old = self.active.get(i);
        if old != v {
            self.push(Undo::Active(i, old));
            self.active.set(i, v);
        }
    }
    /// Record that a transfer touched `i` this step (keeps it awake).
    pub fn touch(&mut self, i: u32) {
        let old = self.last_touch[i as usize];
        if old != self.step {
            self.push(Undo::Touch(i, old));
            self.last_touch[i as usize] = self.step;
        }
    }
    pub fn set_step(&mut self, s: u32) {
        self.push(Undo::Step(self.step));
        self.step = s;
    }
    pub fn add_poured(&mut self, q: u64) {
        self.push(Undo::Poured(self.poured));
        self.poured += q;
    }
    pub fn alloc_id(&mut self) -> BodyId {
        let id = self.next_id;
        self.push(Undo::NextId(id));
        self.next_id += 1;
        id
    }

    /// Before-image body `id` (or its absence) once per logged step.
    fn image(&mut self, id: BodyId) {
        if self.log.is_some() && !self.imaged.contains(&id) {
            let img = self.bodies.get(&id).cloned().map(Box::new);
            self.push(Undo::Body(id, img));
            self.imaged.push(id);
        }
    }
    fn note_touched(&mut self, id: BodyId) {
        if !self.touched.iter().any(|t| t.0 == id) {
            let v = self.bodies.get(&id).map_or(0, |b| b.volume);
            self.touched.push((id, v));
        }
    }
    pub fn insert_body(&mut self, b: Body) {
        self.image(b.id);
        self.bodies.insert(b.id, b);
    }
    pub fn remove_body(&mut self, id: BodyId) -> Body {
        self.image(id);
        self.bodies.remove(&id).expect("remove_body: no such body")
    }
    pub fn body_mut(&mut self, id: BodyId) -> &mut Body {
        self.image(id);
        self.note_touched(id);
        self.bodies.get_mut(&id).expect("body_mut: no such body")
    }

    pub fn begin_log(&mut self) {
        self.log = Some(Vec::new());
        self.imaged.clear();
        self.touched.clear();
    }
    pub fn end_log(&mut self) -> Vec<Undo> {
        self.imaged.clear();
        self.log.take().unwrap_or_default()
    }
    /// Restore the state from before the logged step.
    pub fn undo(&mut self, log: Vec<Undo>) {
        assert!(self.log.is_none(), "undo while a step is being logged");
        for u in log.into_iter().rev() {
            match u {
                Undo::Water(i, v) => self.water[i as usize] = v,
                Undo::Owner(i, v) => self.owner[i as usize] = v,
                Undo::Solid(i, v) => self.solid.set(i, v),
                Undo::Active(i, v) => self.active.set(i, v),
                Undo::Touch(i, v) => self.last_touch[i as usize] = v,
                Undo::Body(id, Some(b)) => {
                    self.bodies.insert(id, *b);
                }
                Undo::Body(id, None) => {
                    self.bodies.remove(&id);
                }
                Undo::NextId(v) => self.next_id = v,
                Undo::Step(v) => self.step = v,
                Undo::Poured(v) => self.poured = v,
            }
        }
    }

    /// Hash of all rollback state (not scratch, not stats).
    pub fn hash(&self) -> u64 {
        let mut h = Fnv::new();
        h.u32(self.step);
        h.u32(self.next_id);
        h.u64(self.poured);
        for w in self.solid.words() {
            h.u64(*w);
        }
        for w in self.active.words() {
            h.u64(*w);
        }
        for (i, &v) in self.water.iter().enumerate() {
            if v != 0 {
                h.u32(i as u32);
                h.u32(v as u32);
            }
        }
        for (i, &o) in self.owner.iter().enumerate() {
            if o != NO_BODY {
                h.u32(i as u32);
                h.u32(o);
            }
        }
        for (i, &t) in self.last_touch.iter().enumerate() {
            if t != 0 {
                h.u32(i as u32);
                h.u32(t);
            }
        }
        for b in self.bodies.values() {
            b.hash_into(&mut h);
        }
        h.finish()
    }

    /// All water: CA cells plus body volumes.
    pub fn mass(&self) -> u64 {
        self.water.iter().map(|&w| w as u64).sum::<u64>()
            + self.bodies.values().map(|b| b.volume).sum::<u64>()
    }
}
