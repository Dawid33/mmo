//! Insertion-ordered dicts (design D4): an entries block plus a hash index, all in the flat memory.
//!
//! # Layout
//! A `Dict` object (`len` = live entries, `aux` bit 0 = keys are heap kinds, bit 1 = values are) has four
//! payload words: the entries block, the index block, `used` (entry positions handed out, live or dead) and
//! the serial counter (the serial the next NEW key gets, from 0; a `u32`: at `u32::MAX` a new key is `HeapFull`).
//! * The entries block (`Raw`) holds `ENTRY_WORDS` words per position: `hash | 1` (0 = a dead position),
//!   key, value. Positions are handed out in insertion order and never reused until a growth compacts them.
//! * The index block (`Raw`) is open addressing with linear probing: slot = position + 1, 0 = empty. A
//!   removed entry leaves its index slot pointing at the dead position (a tombstone in the probe chain), so
//!   chains never break. Slots = the block's payload words; the entries block is the same size class, so
//!   the table is at most a quarter full and a probe always meets an empty slot.
//! * Growth happens when the entries block is full: into the next class when at least half of the
//!   positions are live, else into the same class (the dead positions were most of it). Either way the
//!   entries are copied in order without the dead ones and the index rebuilt. The dict's own pointer never
//!   changes, so `set` returns it only for symmetry with `list::push`.
//!
//! Iteration is by **serial**, never by entry index ([`next`], [`key_at`], [`val_at`]). Each entry stores a
//! serial: the dict's counter at the moment its key was inserted as a NEW key. Overwriting keeps it, a
//! removed-then-re-inserted key gets a new (larger) one, which is what puts it last. Serials increase with
//! the position, dead entries keep theirs until a growth compacts them away, and compaction preserves them, so
//! a growth never changes what a walk sees: an entry that exists when the walk reaches its serial is visited
//! exactly once whatever is inserted or removed meanwhile, and entries inserted during the walk are visited.
//!
//! # Ownership
//! Follows `object`'s rules. `set` takes the key and the value (an existing key: the passed key is released
//! and the old value is released); `get` / `key_at` / `val_at` are raw reads; `take` moves the value out
//! and leaves 0 in the entry (the key stays, so a following `set` with that key stores into the same
//! entry: the take -> mutate -> put back pattern); `remove` hands the value over and releases the stored
//! key. Lookup keys are only borrowed. Mutators need a unique dict.
//!
//! # Reported work
//! Everything that hashes, compares, probes, copies or scans an unbounded number of words returns the
//! count as a `u32` next to its result (design D6); lookups include hashing and comparing the key.

use crate::alloc::{Heap, Kind, ObjHeader};
use crate::eq::{Walk, deep_eq_in, key_hash_in};
use crate::fault::{FaultKind, VmResult};
use crate::list;
use crate::object::{
    alloc, capacity, hdr, incref, read_slot, release, require_unique, write_header, write_slot,
};
use crate::store::Store;

const ENTRY_WORDS: u32 = 4;
const E_HASH: u32 = 0;
const E_SERIAL: u32 = 1;
const E_KEY: u32 = 2;
const E_VAL: u32 = 3;

const P_ENTRIES: u32 = 0;
const P_INDEX: u32 = 1;
const P_USED: u32 = 2;
const P_SERIAL: u32 = 3;
const DICT_PAYLOAD: u32 = 4;

const FLAG_KEYS: u32 = 1;
const FLAG_VALS: u32 = 2;

/// Size class of a new dict's blocks: 14 payload words = 3 entries, 14 index slots.
const FIRST_CLASS: u8 = 4;

fn payload_of(class: u8) -> u32 {
    (1u32 << class) - 2
}

/// An opened dict: its header and its two blocks, validated. Plain copies; re-open after a mutation.
struct Dict {
    p: u32,
    h: ObjHeader,
    ep: u32,
    eh: ObjHeader,
    ip: u32,
    ih: ObjHeader,
    used: u32,
}

fn raw_block(st: &Store, heap: Heap, v: u64) -> VmResult<(u32, ObjHeader)> {
    let (p, h) = hdr(st, heap, v)?;
    if h.kind != Kind::Raw {
        return Err(FaultKind::Corrupt);
    }
    Ok((p, h))
}

fn open(st: &Store, heap: Heap, d: u64) -> VmResult<Dict> {
    let (p, h) = hdr(st, heap, d)?;
    if h.kind != Kind::Dict {
        return Err(FaultKind::Corrupt);
    }
    open_at(st, heap, p, h, true)
}

/// `check_len`: the live count must not exceed `used` (the reaper shrinks both as it goes).
fn open_at(st: &Store, heap: Heap, p: u32, h: ObjHeader, check_len: bool) -> VmResult<Dict> {
    if h.aux & !(FLAG_KEYS | FLAG_VALS) != 0 || capacity(&h) < DICT_PAYLOAD {
        return Err(FaultKind::Corrupt);
    }
    let (ep, eh) = raw_block(st, heap, read_slot(st, p, &h, P_ENTRIES)?)?;
    let (ip, ih) = raw_block(st, heap, read_slot(st, p, &h, P_INDEX)?)?;
    let used = u32::try_from(read_slot(st, p, &h, P_USED)?).map_err(|_| FaultKind::Corrupt)?;
    let fits = used
        .checked_mul(ENTRY_WORDS)
        .is_some_and(|w| w <= capacity(&eh));
    if !fits || (check_len && h.len > used) || capacity(&ih) < 2 {
        return Err(FaultKind::Corrupt);
    }
    Ok(Dict {
        p,
        h,
        ep,
        eh,
        ip,
        ih,
        used,
    })
}

/// Two blocks for a dict's entries and index (freed again if the second does not fit).
fn alloc_blocks(
    st: &mut Store,
    heap: Heap,
    entries_payload: u32,
    index_payload: u32,
) -> VmResult<(u32, u32)> {
    let ep = alloc(st, heap, Kind::Raw, entries_payload)?;
    match alloc(st, heap, Kind::Raw, index_payload) {
        Ok(ip) => Ok((ep, ip)),
        Err(e) => {
            heap.free(st, ep)?;
            Err(e)
        }
    }
}

/// Adds position `pos` (stored hash `hw`) to the index at the first empty slot of its probe chain.
/// Returns the slots probed.
fn index_insert(
    st: &mut Store,
    heap: Heap,
    ip: u32,
    ih: &ObjHeader,
    hw: u64,
    pos: u32,
) -> VmResult<u32> {
    let slots = capacity(ih);
    let mut s = (hw % u64::from(slots)) as u32;
    for n in 1..=slots {
        if read_slot(st, ip, ih, s)? == 0 {
            write_slot(st, heap, ip, ih, s, u64::from(pos) + 1)?;
            return Ok(n);
        }
        s = if s + 1 == slots { 0 } else { s + 1 };
    }
    Err(FaultKind::Corrupt)
}

impl Dict {
    fn keys_ref(&self) -> bool {
        self.h.aux & FLAG_KEYS != 0
    }

    fn vals_ref(&self) -> bool {
        self.h.aux & FLAG_VALS != 0
    }

    fn entry_capacity(&self) -> u32 {
        capacity(&self.eh) / ENTRY_WORDS
    }

    fn eread(&self, st: &Store, pos: u32, field: u32) -> VmResult<u64> {
        if pos >= self.used {
            return Err(FaultKind::Corrupt);
        }
        read_slot(st, self.ep, &self.eh, pos * ENTRY_WORDS + field)
    }

    fn ewrite(&self, st: &mut Store, heap: Heap, pos: u32, field: u32, v: u64) -> VmResult<()> {
        if pos >= self.used {
            return Err(FaultKind::Corrupt);
        }
        write_slot(st, heap, self.ep, &self.eh, pos * ENTRY_WORDS + field, v)
    }

    /// The live position holding `key` (stored-hash word `hw`); probes and comparisons charge `walk`.
    fn find(
        &self,
        st: &Store,
        heap: Heap,
        key: u64,
        hw: u64,
        walk: &mut Walk,
    ) -> VmResult<Option<u32>> {
        let slots = capacity(&self.ih);
        let mut s = (hw % u64::from(slots)) as u32;
        for _ in 0..slots {
            walk.charge(1)?;
            let v = read_slot(st, self.ip, &self.ih, s)?;
            if v == 0 {
                return Ok(None);
            }
            let pos = u32::try_from(v - 1).map_err(|_| FaultKind::Corrupt)?;
            if self.eread(st, pos, E_HASH)? == hw {
                let stored = self.eread(st, pos, E_KEY)?;
                if deep_eq_in(st, heap, stored, key, self.keys_ref(), walk)? {
                    return Ok(Some(pos));
                }
            }
            s = if s + 1 == slots { 0 } else { s + 1 };
        }
        Ok(None)
    }

    /// Hash, probe and compare under one budget; also returns the words visited.
    fn lookup(&self, st: &Store, heap: Heap, key: u64) -> VmResult<(Option<u32>, u32)> {
        let mut walk = Walk::default();
        let hash = key_hash_in(st, heap, key, self.keys_ref(), &mut walk)?;
        let pos = self.find(st, heap, key, hash | 1, &mut walk)?;
        Ok((pos, walk.used))
    }

    /// Moves the live entries, in order, into fresh blocks of `class` and rebuilds the index. Returns the
    /// work (positions scanned, words moved, probes, and the old blocks' payload words zeroed).
    fn rebuild(&self, st: &mut Store, heap: Heap, class: u8) -> VmResult<u32> {
        let (ne, ni) = alloc_blocks(st, heap, payload_of(class), payload_of(class))?;
        let neh = heap.header(st, ne)?;
        let nih = heap.header(st, ni)?;
        let mut work = 0u32;
        let mut n = 0u32;
        for pos in 0..self.used {
            work = work.saturating_add(1);
            let hw = self.eread(st, pos, E_HASH)?;
            if hw == 0 {
                continue;
            }
            for f in 0..ENTRY_WORDS {
                let w = self.eread(st, pos, f)?;
                write_slot(st, heap, ne, &neh, n * ENTRY_WORDS + f, w)?;
            }
            work = work.saturating_add(ENTRY_WORDS);
            work = work.saturating_add(index_insert(st, heap, ni, &nih, hw, n)?);
            n += 1;
        }
        if n != self.h.len {
            return Err(FaultKind::Corrupt);
        }
        write_slot(st, heap, self.p, &self.h, P_ENTRIES, u64::from(ne))?;
        write_slot(st, heap, self.p, &self.h, P_INDEX, u64::from(ni))?;
        write_slot(st, heap, self.p, &self.h, P_USED, u64::from(n))?;
        heap.free(st, self.ep)?;
        heap.free(st, self.ip)?;
        Ok(work
            .saturating_add(capacity(&self.eh))
            .saturating_add(capacity(&self.ih)))
    }
}

pub fn new(st: &mut Store, heap: Heap, keys_are_refs: bool, vals_are_refs: bool) -> VmResult<u64> {
    let p = alloc(st, heap, Kind::Dict, DICT_PAYLOAD)?;
    let (ep, ip) = match alloc_blocks(st, heap, payload_of(FIRST_CLASS), payload_of(FIRST_CLASS)) {
        Ok(b) => b,
        Err(e) => {
            heap.free(st, p)?;
            return Err(e);
        }
    };
    let mut h = heap.header(st, p)?;
    h.aux = (u32::from(keys_are_refs) * FLAG_KEYS) | (u32::from(vals_are_refs) * FLAG_VALS);
    write_header(st, heap, p, h)?;
    write_slot(st, heap, p, &h, P_ENTRIES, u64::from(ep))?;
    write_slot(st, heap, p, &h, P_INDEX, u64::from(ip))?;
    Ok(u64::from(p))
}

/// Live entries.
pub fn len(st: &Store, heap: Heap, d: u64) -> VmResult<u32> {
    Ok(open(st, heap, d)?.h.len)
}

/// The raw value stored under `key`; no refcount changes.
pub fn get(st: &Store, heap: Heap, d: u64, key: u64) -> VmResult<(Option<u64>, u32)> {
    let dd = open(st, heap, d)?;
    let (pos, work) = dd.lookup(st, heap, key)?;
    let v = match pos {
        Some(pos) => Some(dd.eread(st, pos, E_VAL)?),
        None => None,
    };
    Ok((v, work))
}

pub fn contains(st: &Store, heap: Heap, d: u64, key: u64) -> VmResult<(bool, u32)> {
    let (pos, work) = open(st, heap, d)?.lookup(st, heap, key)?;
    Ok((pos.is_some(), work))
}

/// Moves the value out, leaving 0 in the entry (the key stays); needs a unique dict.
pub fn take(st: &mut Store, heap: Heap, d: u64, key: u64) -> VmResult<(Option<u64>, u32)> {
    let dd = open(st, heap, d)?;
    require_unique(&dd.h)?;
    let (pos, work) = dd.lookup(st, heap, key)?;
    let Some(pos) = pos else {
        return Ok((None, work));
    };
    let v = dd.eread(st, pos, E_VAL)?;
    dd.ewrite(st, heap, pos, E_VAL, 0)?;
    Ok((Some(v), work))
}

/// Stores `val` under `key`, taking both. A key that exists keeps its position and stored key: the passed
/// key and the old value are released. A new key goes last, growing the blocks when they are full.
/// Returns the dict pointer (it never moves) and the work.
pub fn set(st: &mut Store, heap: Heap, d: u64, key: u64, val: u64) -> VmResult<(u64, u32)> {
    let mut dd = open(st, heap, d)?;
    require_unique(&dd.h)?;
    let mut walk = Walk::default();
    let hw = key_hash_in(st, heap, key, dd.keys_ref(), &mut walk)? | 1;
    let found = dd.find(st, heap, key, hw, &mut walk)?;
    let mut work = walk.used;
    if let Some(pos) = found {
        let old = dd.eread(st, pos, E_VAL)?;
        dd.ewrite(st, heap, pos, E_VAL, val)?;
        if dd.vals_ref() {
            release(st, heap, old)?;
        }
        if dd.keys_ref() {
            release(st, heap, key)?;
        }
        return Ok((d, work));
    }
    let serial =
        u32::try_from(read_slot(st, dd.p, &dd.h, P_SERIAL)?).map_err(|_| FaultKind::Corrupt)?;
    let next_serial = serial.checked_add(1).ok_or(FaultKind::HeapFull)?;
    if dd.used >= dd.entry_capacity() {
        let class = if u64::from(dd.h.len) * 2 >= u64::from(dd.entry_capacity()) {
            dd.eh.class + 1
        } else {
            dd.eh.class
        };
        work = work.saturating_add(dd.rebuild(st, heap, class)?);
        dd = open(st, heap, d)?;
    }
    let pos = dd.used;
    write_slot(st, heap, dd.p, &dd.h, P_USED, u64::from(pos) + 1)?;
    dd.used = pos + 1;
    write_slot(st, heap, dd.p, &dd.h, P_SERIAL, u64::from(next_serial))?;
    dd.ewrite(st, heap, pos, E_HASH, hw)?;
    dd.ewrite(st, heap, pos, E_SERIAL, u64::from(serial))?;
    dd.ewrite(st, heap, pos, E_KEY, key)?;
    dd.ewrite(st, heap, pos, E_VAL, val)?;
    work = work.saturating_add(index_insert(st, heap, dd.ip, &dd.ih, hw, pos)?);
    let mut h = dd.h;
    h.len += 1;
    write_header(st, heap, dd.p, h)?;
    Ok((d, work))
}

/// Removes `key`, handing its value to the caller and releasing the stored key; `None` when absent.
pub fn remove(st: &mut Store, heap: Heap, d: u64, key: u64) -> VmResult<(Option<u64>, u32)> {
    let dd = open(st, heap, d)?;
    require_unique(&dd.h)?;
    let (pos, work) = dd.lookup(st, heap, key)?;
    let Some(pos) = pos else {
        return Ok((None, work));
    };
    let stored = dd.eread(st, pos, E_KEY)?;
    let v = dd.eread(st, pos, E_VAL)?;
    // The serial stays: serials of all positions, dead or alive, increase with the position.
    for f in [E_HASH, E_KEY, E_VAL] {
        dd.ewrite(st, heap, pos, f, 0)?;
    }
    if dd.keys_ref() {
        release(st, heap, stored)?;
    }
    let mut h = dd.h;
    h.len = h.len.checked_sub(1).ok_or(FaultKind::Corrupt)?;
    write_header(st, heap, dd.p, h)?;
    Ok((Some(v), work))
}

/// The first position of the entries block whose serial is `>= serial` (`used` when there is none).
/// Serials increase with the position, dead entries keep theirs, so this is a binary search.
fn lower_bound(dd: &Dict, st: &Store, serial: u32) -> VmResult<u32> {
    let (mut lo, mut hi) = (0u32, dd.used);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if dd.eread(st, mid, E_SERIAL)? < u64::from(serial) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

/// The serial of the first live entry whose serial is `>= pos` (a negative `pos` counts as 0), or -1; also
/// the work (the search plus the dead positions skipped). A loop is `pos = 0; s = next(pos); ...; pos = s + 1`:
/// an entry that exists when the walk reaches its serial is visited exactly once whatever is inserted or
/// removed meanwhile, and entries inserted during the walk (larger serials) are visited.
pub fn next(st: &Store, heap: Heap, d: u64, pos: i64) -> VmResult<(i64, u32)> {
    let dd = open(st, heap, d)?;
    let Ok(start) = u32::try_from(pos.max(0)) else {
        return Ok((-1, 1));
    };
    let mut p = lower_bound(&dd, st, start)?;
    let mut work = 32 - dd.used.leading_zeros();
    while p < dd.used {
        work = work.saturating_add(1);
        if dd.eread(st, p, E_HASH)? != 0 {
            let serial = dd.eread(st, p, E_SERIAL)?;
            return Ok((i64::try_from(serial).map_err(|_| FaultKind::Corrupt)?, work));
        }
        p += 1;
    }
    Ok((-1, work))
}

/// The position of the live entry with this serial; anything else is a caller bug (`Corrupt`).
fn live_position(dd: &Dict, st: &Store, serial: i64) -> VmResult<u32> {
    let s = u32::try_from(serial).map_err(|_| FaultKind::Corrupt)?;
    let p = lower_bound(dd, st, s)?;
    if p < dd.used && dd.eread(st, p, E_SERIAL)? == u64::from(s) && dd.eread(st, p, E_HASH)? != 0 {
        Ok(p)
    } else {
        Err(FaultKind::Corrupt)
    }
}

/// The raw key of the live entry with this serial (one `next` returned); `Corrupt` if there is none.
pub fn key_at(st: &Store, heap: Heap, d: u64, serial: i64) -> VmResult<u64> {
    let dd = open(st, heap, d)?;
    dd.eread(st, live_position(&dd, st, serial)?, E_KEY)
}

/// The raw value of the live entry with this serial (one `next` returned); `Corrupt` if there is none.
pub fn val_at(st: &Store, heap: Heap, d: u64, serial: i64) -> VmResult<u64> {
    let dd = open(st, heap, d)?;
    dd.eread(st, live_position(&dd, st, serial)?, E_VAL)
}

fn collect(st: &mut Store, heap: Heap, d: u64, field: u32) -> VmResult<(u64, u32)> {
    let dd = open(st, heap, d)?;
    let refs = if field == E_KEY {
        dd.keys_ref()
    } else {
        dd.vals_ref()
    };
    let mut l = list::new(st, heap, refs)?;
    let mut work = 0u32;
    for pos in 0..dd.used {
        work = work.saturating_add(1);
        if dd.eread(st, pos, E_HASH)? == 0 {
            continue;
        }
        let v = dd.eread(st, pos, field)?;
        if refs {
            incref(st, heap, v)?;
        }
        let (nl, w) = list::push(st, heap, l, v)?;
        l = nl;
        work = work.saturating_add(w).saturating_add(1);
    }
    Ok((l, work))
}

/// Order-insensitive equality: the same live count, and every key of `a` found in `b` (by the stored hash
/// and `deep_eq`) with a value `val_eq` accepts (called as `(a's value, b's value, values are heap kinds)`).
/// Everything charges the caller's `walk`.
pub(crate) fn equal(
    st: &Store,
    heap: Heap,
    a: u64,
    b: u64,
    walk: &mut Walk,
    val_eq: &mut dyn FnMut(u64, u64, bool, &mut Walk) -> VmResult<bool>,
) -> VmResult<bool> {
    let (da, db) = (open(st, heap, a)?, open(st, heap, b)?);
    if da.h.len != db.h.len || da.h.aux != db.h.aux {
        return Ok(false);
    }
    for pos in 0..da.used {
        walk.charge(1)?;
        let hw = da.eread(st, pos, E_HASH)?;
        if hw == 0 {
            continue;
        }
        let key = da.eread(st, pos, E_KEY)?;
        let Some(q) = db.find(st, heap, key, hw, walk)? else {
            return Ok(false);
        };
        let (x, y) = (da.eread(st, pos, E_VAL)?, db.eread(st, q, E_VAL)?);
        if !val_eq(x, y, da.vals_ref(), walk)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A new list of the keys in iteration order (heap-kind keys get a reference); also the work.
pub fn keys(st: &mut Store, heap: Heap, d: u64) -> VmResult<(u64, u32)> {
    collect(st, heap, d, E_KEY)
}

/// A new list of the values in iteration order (heap-kind values get a reference); also the work.
pub fn values(st: &mut Store, heap: Heap, d: u64) -> VmResult<(u64, u32)> {
    collect(st, heap, d, E_VAL)
}

/// `make_unique` of a shared dict: a copy with its own blocks (same positions, dead ones included) and a
/// reference on every heap-kind key and value. The source's count is the caller's to lower. Returns the
/// copy and the words copied.
pub(crate) fn clone(st: &mut Store, heap: Heap, src: u32, sh: ObjHeader) -> VmResult<(u64, u32)> {
    let dd = open_at(st, heap, src, sh, true)?;
    let q = alloc(st, heap, Kind::Dict, DICT_PAYLOAD)?;
    let (ne, ni) = match alloc_blocks(st, heap, capacity(&dd.eh), capacity(&dd.ih)) {
        Ok(b) => b,
        Err(e) => {
            heap.free(st, q)?;
            return Err(e);
        }
    };
    let mut nh = heap.header(st, q)?;
    nh.tag = sh.tag;
    nh.len = sh.len;
    nh.aux = sh.aux;
    write_header(st, heap, q, nh)?;
    let neh = heap.header(st, ne)?;
    let nih = heap.header(st, ni)?;
    write_slot(st, heap, q, &nh, P_ENTRIES, u64::from(ne))?;
    write_slot(st, heap, q, &nh, P_INDEX, u64::from(ni))?;
    write_slot(st, heap, q, &nh, P_USED, u64::from(dd.used))?;
    let counter = read_slot(st, dd.p, &dd.h, P_SERIAL)?;
    write_slot(st, heap, q, &nh, P_SERIAL, counter)?;
    for w in 0..dd.used * ENTRY_WORDS {
        let v = read_slot(st, dd.ep, &dd.eh, w)?;
        write_slot(st, heap, ne, &neh, w, v)?;
        let field = w % ENTRY_WORDS;
        let is_ref = (field == E_KEY && dd.keys_ref()) || (field == E_VAL && dd.vals_ref());
        if is_ref {
            incref(st, heap, v)?;
        }
    }
    for s in 0..capacity(&dd.ih) {
        let v = read_slot(st, dd.ip, &dd.ih, s)?;
        write_slot(st, heap, ni, &nih, s, v)?;
    }
    let words = (dd.used * ENTRY_WORDS).saturating_add(capacity(&dd.ih));
    Ok((u64::from(q), words))
}

/// One reap step's share for a dead dict: releases the keys and values of the last positions (at most
/// the reap budget (`REAP_CHILDREN_MAX`); a dead position or an entry costs at least one), shrinking `used` and
/// `h.len` as it goes so the dict's own state is the cursor. When no position is left it frees the entries
/// and index blocks and returns `true` (the caller frees the dict block).
pub(crate) fn reap(
    st: &mut Store,
    heap: Heap,
    p: u32,
    h: &mut ObjHeader,
    budget: &mut usize,
    work: &mut u32,
) -> VmResult<bool> {
    let dd = open_at(st, heap, p, *h, false)?;
    let need = usize::from(dd.keys_ref()) + usize::from(dd.vals_ref());
    let mut used = dd.used;
    while used > 0 && *budget > 0 {
        let pos = used - 1;
        let live = read_slot(st, dd.ep, &dd.eh, pos * ENTRY_WORDS + E_HASH)? != 0;
        let cost = if live { need.max(1) } else { 1 };
        if *budget < cost {
            break;
        }
        if live {
            let k = read_slot(st, dd.ep, &dd.eh, pos * ENTRY_WORDS + E_KEY)?;
            let v = read_slot(st, dd.ep, &dd.eh, pos * ENTRY_WORDS + E_VAL)?;
            write_slot(st, heap, dd.ep, &dd.eh, pos * ENTRY_WORDS + E_HASH, 0)?;
            if dd.keys_ref() {
                release(st, heap, k)?;
            }
            if dd.vals_ref() {
                release(st, heap, v)?;
            }
            h.len = h.len.checked_sub(1).ok_or(FaultKind::Corrupt)?;
            *work = work.saturating_add(need as u32);
        }
        used -= 1;
        write_slot(st, heap, p, h, P_USED, u64::from(used))?;
        *budget -= cost;
        *work = work.saturating_add(1);
    }
    if used > 0 {
        return Ok(false);
    }
    heap.free(st, dd.ep)?;
    heap.free(st, dd.ip)?;
    *work = work
        .saturating_add(capacity(&dd.eh))
        .saturating_add(capacity(&dd.ih));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_never_fills() {
        // The entries block of class c holds (2^c - 2) / 3 entries, the index of the same class 2^c - 2
        // slots: at most a third full, for every class a dict can reach.
        for c in FIRST_CLASS..=crate::alloc::CLASS_MAX {
            assert!(payload_of(c) / ENTRY_WORDS < payload_of(c));
        }
    }
}
