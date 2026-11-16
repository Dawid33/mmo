//! Strings: UTF-8 bytes packed little-endian into the payload words, `len` = byte length, unused tail
//! bytes zero (so equal strings have equal words). Immutable; copy-on-write never needs to mutate one.

use crate::alloc::{Heap, Kind, ObjHeader};
use crate::fault::{FaultKind, VmResult};
use crate::object::{alloc, hdr, read_slot, write_header, write_slot};
use crate::store::Store;

fn string(st: &Store, heap: Heap, v: u64) -> VmResult<(u32, ObjHeader)> {
    let (p, h) = hdr(st, heap, v)?;
    if h.kind != Kind::Str {
        return Err(FaultKind::Corrupt);
    }
    Ok((p, h))
}

fn bytes_of(st: &Store, heap: Heap, v: u64) -> VmResult<Vec<u8>> {
    let (p, h) = string(st, heap, v)?;
    let mut out = Vec::new();
    for i in 0..h.len.div_ceil(8) {
        out.extend_from_slice(&read_slot(st, p, &h, i)?.to_le_bytes());
    }
    out.truncate(h.len as usize);
    Ok(out)
}

fn new_bytes(st: &mut Store, heap: Heap, bytes: &[u8]) -> VmResult<u64> {
    let len = u32::try_from(bytes.len()).map_err(|_| FaultKind::HeapFull)?;
    let p = alloc(st, heap, Kind::Str, len.div_ceil(8))?;
    let mut h = heap.header(st, p)?;
    h.len = len;
    write_header(st, heap, p, h)?;
    for (i, chunk) in bytes.chunks(8).enumerate() {
        let mut w = [0u8; 8];
        w[..chunk.len()].copy_from_slice(chunk);
        write_slot(st, heap, p, &h, i as u32, u64::from_le_bytes(w))?;
    }
    Ok(u64::from(p))
}

pub fn new(st: &mut Store, heap: Heap, s: &str) -> VmResult<u64> {
    new_bytes(st, heap, s.as_bytes())
}

pub fn read(st: &Store, heap: Heap, v: u64) -> VmResult<String> {
    String::from_utf8(bytes_of(st, heap, v)?).map_err(|_| FaultKind::Corrupt)
}

pub fn byte_len(st: &Store, heap: Heap, v: u64) -> VmResult<u32> {
    Ok(string(st, heap, v)?.1.len)
}

/// Characters (Unicode scalar values), as `str.len()` counts them.
pub fn char_len(st: &Store, heap: Heap, v: u64) -> VmResult<u32> {
    let n = bytes_of(st, heap, v)?
        .iter()
        .filter(|b| (**b & 0xC0) != 0x80)
        .count();
    u32::try_from(n).map_err(|_| FaultKind::Corrupt)
}

/// A new string of the parts in order; the parts keep their references. Also returns the payload words
/// of the result (what was copied).
pub fn concat(st: &mut Store, heap: Heap, parts: &[u64]) -> VmResult<(u64, u32)> {
    let mut all = Vec::new();
    for &p in parts {
        all.extend_from_slice(&bytes_of(st, heap, p)?);
    }
    let words = u32::try_from(all.len().div_ceil(8)).map_err(|_| FaultKind::HeapFull)?;
    Ok((new_bytes(st, heap, &all)?, words))
}

/// Content equality of two string values; `Ok(false)` when either is not a string.
pub(crate) fn same_text(st: &Store, heap: Heap, a: u64, b: u64) -> VmResult<bool> {
    let (pa, ha) = hdr(st, heap, a)?;
    let (pb, hb) = hdr(st, heap, b)?;
    if ha.kind != Kind::Str || hb.kind != Kind::Str || ha.len != hb.len {
        return Ok(false);
    }
    for i in 0..ha.len.div_ceil(8) {
        if read_slot(st, pa, &ha, i)? != read_slot(st, pb, &hb, i)? {
            return Ok(false);
        }
    }
    Ok(true)
}
