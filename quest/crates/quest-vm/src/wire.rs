//! Self-describing wire values (design D8): namespace values, event payloads, command arguments, error
//! data in reports and saved globals travel as [`WireValue`]; [`to_wire`] / [`from_wire`] convert between
//! one and a heap value of a known type.
//!
//! # Bytes
//! A tag byte (`Unit` 0, `Bool` 1, `Int` 2, `Float` 3, `Str` 4, `List` 5, `Dict` 6, `Struct` 7, `Variant`
//! 8), then: `Bool` one byte (0 / 1); `Int` and `Float` 8 little-endian bytes (a NaN is written as
//! `0x7ff8_0000_0000_0000`); `Str` a `u32` byte length and the UTF-8; `List` a `u32` count and the values;
//! `Dict` a `u32` count and key, value pairs (insertion order); `Struct` / `Variant` a name string, a `u32`
//! field count and (name string, value) pairs.
//!
//! # Loading is structural (§8.2)
//! A struct's fields are matched by name, in any order, the same set exactly; a variant must exist in the
//! receiving enum with a matching field set; type names are carried but not compared. Anything else is
//! [`FromWire::WrongType`]. A dict repeating a key is `WrongType` too.
//!
//! # Memory
//! [`to_wire`] and [`from_wire`] are structural walks over heap memory or over a value that came from
//! outside, so both run under the same [`Walk`] budget as equality (`WORK_WORDS_MAX` words, then
//! `ValueTooLarge`; nesting past `WIRE_DEPTH_MAX`, `Corrupt`) and report the words they visited. A
//! `from_wire` that fails halfway releases what it built (the caller reaps).

use crate::alloc::{Heap, Kind};
use crate::consts::{STRUCT_FIELDS_MAX, WIRE_DEPTH_MAX};
use crate::dict;
use crate::eq::{Walk, nested};
use crate::fault::{FaultKind, VmResult};
use crate::list;
use crate::object::{check_obj_shape, hdr, obj, release};
use crate::store::Store;
use crate::string;
use crate::types::{TypeDesc, TypeId, TypeTable};

const NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

const T_UNIT: u8 = 0;
const T_BOOL: u8 = 1;
const T_INT: u8 = 2;
const T_FLOAT: u8 = 3;
const T_STR: u8 = 4;
const T_LIST: u8 = 5;
const T_DICT: u8 = 6;
const T_STRUCT: u8 = 7;
const T_VARIANT: u8 = 8;

#[derive(Clone, Debug, PartialEq)]
pub enum WireValue {
    Unit,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<WireValue>),
    Dict(Vec<(WireValue, WireValue)>),
    Struct {
        name: String,
        fields: Vec<(String, WireValue)>,
    },
    Variant {
        name: String,
        fields: Vec<(String, WireValue)>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    Truncated,
    Trailing,
    BadTag(u8),
    BadUtf8,
    TooDeep,
}

/// Any NaN becomes the one canonical NaN.
pub(crate) fn canonical_bits(f: f64) -> u64 {
    if f.is_nan() { NAN_BITS } else { f.to_bits() }
}

// ---- bytes -------------------------------------------------------------------------------------------

fn put_len(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_len(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

fn put_fields(out: &mut Vec<u8>, name: &str, fields: &[(String, WireValue)]) {
    put_str(out, name);
    put_len(out, fields.len());
    for (n, v) in fields {
        put_str(out, n);
        v.encode(out);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(n).ok_or(WireError::Truncated)?;
        let s = self.bytes.get(self.pos..end).ok_or(WireError::Truncated)?;
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    fn string(&mut self) -> Result<String, WireError> {
        let n = self.u32()?;
        let b = self.take(usize::try_from(n).map_err(|_| WireError::Truncated)?)?;
        String::from_utf8(b.to_vec()).map_err(|_| WireError::BadUtf8)
    }

    fn fields(&mut self, depth: usize) -> Result<(String, Vec<(String, WireValue)>), WireError> {
        let name = self.string()?;
        let n = self.u32()?;
        // Never sized by the count: it is untrusted, and every field takes bytes, so the loop ends.
        let mut fields = Vec::new();
        for _ in 0..n {
            let fname = self.string()?;
            fields.push((fname, self.value(depth)?));
        }
        Ok((name, fields))
    }

    /// `depth`: containers entered so far.
    fn value(&mut self, depth: usize) -> Result<WireValue, WireError> {
        let tag = self.u8()?;
        if matches!(tag, T_LIST | T_DICT | T_STRUCT | T_VARIANT) && depth >= WIRE_DEPTH_MAX {
            return Err(WireError::TooDeep);
        }
        Ok(match tag {
            T_UNIT => WireValue::Unit,
            T_BOOL => match self.u8()? {
                0 => WireValue::Bool(false),
                1 => WireValue::Bool(true),
                b => return Err(WireError::BadTag(b)),
            },
            T_INT => WireValue::Int(self.u64()? as i64),
            T_FLOAT => {
                WireValue::Float(f64::from_bits(canonical_bits(f64::from_bits(self.u64()?))))
            }
            T_STR => WireValue::Str(self.string()?),
            T_LIST => {
                let n = self.u32()?;
                let mut items = Vec::new();
                for _ in 0..n {
                    items.push(self.value(depth + 1)?);
                }
                WireValue::List(items)
            }
            T_DICT => {
                let n = self.u32()?;
                let mut pairs = Vec::new();
                for _ in 0..n {
                    let k = self.value(depth + 1)?;
                    pairs.push((k, self.value(depth + 1)?));
                }
                WireValue::Dict(pairs)
            }
            T_STRUCT => {
                let (name, fields) = self.fields(depth + 1)?;
                WireValue::Struct { name, fields }
            }
            T_VARIANT => {
                let (name, fields) = self.fields(depth + 1)?;
                WireValue::Variant { name, fields }
            }
            t => return Err(WireError::BadTag(t)),
        })
    }
}

impl WireValue {
    /// Appends the bytes of this value (floats canonicalised).
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            WireValue::Unit => out.push(T_UNIT),
            WireValue::Bool(b) => out.extend_from_slice(&[T_BOOL, u8::from(*b)]),
            WireValue::Int(i) => {
                out.push(T_INT);
                out.extend_from_slice(&i.to_le_bytes());
            }
            WireValue::Float(f) => {
                out.push(T_FLOAT);
                out.extend_from_slice(&canonical_bits(*f).to_le_bytes());
            }
            WireValue::Str(s) => {
                out.push(T_STR);
                put_str(out, s);
            }
            WireValue::List(items) => {
                out.push(T_LIST);
                put_len(out, items.len());
                for v in items {
                    v.encode(out);
                }
            }
            WireValue::Dict(pairs) => {
                out.push(T_DICT);
                put_len(out, pairs.len());
                for (k, v) in pairs {
                    k.encode(out);
                    v.encode(out);
                }
            }
            WireValue::Struct { name, fields } => {
                out.push(T_STRUCT);
                put_fields(out, name, fields);
            }
            WireValue::Variant { name, fields } => {
                out.push(T_VARIANT);
                put_fields(out, name, fields);
            }
        }
    }

    /// Parses exactly one value; never panics, never allocates by an untrusted count, nesting is capped
    /// at `WIRE_DEPTH_MAX` containers.
    pub fn decode(bytes: &[u8]) -> Result<WireValue, WireError> {
        let mut r = Reader { bytes, pos: 0 };
        let v = r.value(0)?;
        if r.pos != bytes.len() {
            return Err(WireError::Trailing);
        }
        Ok(v)
    }
}

// ---- heap -> wire ------------------------------------------------------------------------------------

/// The wire form of the heap value `v` of type `t`, and the words visited.
pub fn to_wire(
    st: &Store,
    heap: Heap,
    types: &TypeTable,
    t: TypeId,
    v: u64,
) -> VmResult<(WireValue, u32)> {
    let mut walk = Walk::default();
    let w = to_w(st, heap, types, t, v, &mut walk)?;
    Ok((w, walk.used))
}

fn to_w(
    st: &Store,
    heap: Heap,
    types: &TypeTable,
    t: TypeId,
    v: u64,
    walk: &mut Walk,
) -> VmResult<WireValue> {
    walk.charge(1)?;
    Ok(match types.desc(t)? {
        TypeDesc::Unit => WireValue::Unit,
        TypeDesc::Bool => match v {
            0 => WireValue::Bool(false),
            1 => WireValue::Bool(true),
            _ => return Err(FaultKind::Corrupt),
        },
        TypeDesc::Int => WireValue::Int(v as i64),
        TypeDesc::Float => WireValue::Float(f64::from_bits(canonical_bits(f64::from_bits(v)))),
        TypeDesc::Promise(_) => return Err(FaultKind::Corrupt),
        TypeDesc::Str => nested(walk, |walk| -> VmResult<WireValue> {
            walk.charge(string::byte_len(st, heap, v)?.div_ceil(8))?;
            Ok(WireValue::Str(string::read(st, heap, v)?))
        })?,
        TypeDesc::List(e) => nested(walk, |walk| -> VmResult<WireValue> {
            let (_, h) = hdr(st, heap, v)?;
            let want = if types.is_ref(*e) {
                Kind::ListRef
            } else {
                Kind::ListImm
            };
            if h.kind != want {
                return Err(FaultKind::Corrupt);
            }
            let mut items = Vec::new();
            for i in 0..h.len {
                let slot = list::get(st, heap, v, i64::from(i))?;
                items.push(to_w(st, heap, types, *e, slot, walk)?);
            }
            Ok(WireValue::List(items))
        })?,
        TypeDesc::Dict(kt, vt) => nested(walk, |walk| -> VmResult<WireValue> {
            let (_, h) = hdr(st, heap, v)?;
            if h.kind != Kind::Dict {
                return Err(FaultKind::Corrupt);
            }
            let mut pairs = Vec::new();
            let mut pos = 0i64;
            loop {
                let (serial, work) = dict::next(st, heap, v, pos)?;
                walk.charge(work)?;
                if serial < 0 {
                    break;
                }
                let k = dict::key_at(st, heap, v, serial)?;
                let val = dict::val_at(st, heap, v, serial)?;
                let k = to_w(st, heap, types, *kt, k, walk)?;
                pairs.push((k, to_w(st, heap, types, *vt, val, walk)?));
                pos = serial + 1;
            }
            Ok(WireValue::Dict(pairs))
        })?,
        TypeDesc::Struct { name, fields } => nested(walk, |walk| -> VmResult<WireValue> {
            let names = obj_fields(st, heap, types, v, 0, fields, walk)?;
            Ok(WireValue::Struct {
                name: name.clone(),
                fields: names,
            })
        })?,
        TypeDesc::Enum { variants, .. } => {
            let tag = obj::tag(st, heap, v)?;
            let (vname, vfields) = variants.get(usize::from(tag)).ok_or(FaultKind::Corrupt)?;
            let fields = if v & 1 == 1 {
                if !vfields.is_empty() {
                    return Err(FaultKind::Corrupt);
                }
                vec![]
            } else {
                nested(walk, |walk| -> VmResult<_> {
                    obj_fields(st, heap, types, v, tag, vfields, walk)
                })?
            };
            WireValue::Variant {
                name: vname.clone(),
                fields,
            }
        }
    })
}

/// The fields of the object `v` (tag `tag`, the declared `fields`) as wire fields.
fn obj_fields(
    st: &Store,
    heap: Heap,
    types: &TypeTable,
    v: u64,
    tag: u16,
    fields: &[(String, TypeId)],
    walk: &mut Walk,
) -> VmResult<Vec<(String, WireValue)>> {
    let (_, h) = hdr(st, heap, v)?;
    if h.kind != Kind::Obj {
        return Err(FaultKind::Corrupt);
    }
    check_obj_shape(&h)?;
    if h.tag != tag || usize::try_from(h.len).ok() != Some(fields.len()) {
        return Err(FaultKind::Corrupt);
    }
    let mut out = Vec::new();
    for (i, (name, ft)) in (0u32..).zip(fields) {
        let slot = obj::get_field(st, heap, v, i)?;
        out.push((name.clone(), to_w(st, heap, types, *ft, slot, walk)?));
    }
    Ok(out)
}

// ---- wire -> heap ------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FromWire {
    /// The value does not fit the receiving type.
    WrongType,
    Fault(FaultKind),
}

impl From<FaultKind> for FromWire {
    fn from(f: FaultKind) -> FromWire {
        FromWire::Fault(f)
    }
}

type Built<T> = Result<T, FromWire>;

/// Builds the heap value of type `t` from `w` (the caller owns the one reference of the result), and
/// reports the words visited. On an error nothing built stays referenced.
pub fn from_wire(
    st: &mut Store,
    heap: Heap,
    types: &TypeTable,
    t: TypeId,
    w: &WireValue,
) -> Result<(u64, u32), FromWire> {
    let mut walk = Walk::default();
    let v = from_w(st, heap, types, t, w, &mut walk)?;
    Ok((v, walk.used))
}

/// Releases `v` when it is a heap-kind value of type `t` (a plain immediate looks like a pointer).
fn drop_value(st: &mut Store, heap: Heap, types: &TypeTable, t: TypeId, v: u64) {
    if types.is_ref(t) {
        let _ = release(st, heap, v);
    }
}

/// `r` of filling the new container `c`: on an error `c` (with whatever it already holds) is released.
fn finish(st: &mut Store, heap: Heap, c: u64, r: Built<()>) -> Built<u64> {
    match r {
        Ok(()) => Ok(c),
        Err(e) => {
            let _ = release(st, heap, c);
            Err(e)
        }
    }
}

fn from_w(
    st: &mut Store,
    heap: Heap,
    types: &TypeTable,
    t: TypeId,
    w: &WireValue,
    walk: &mut Walk,
) -> Built<u64> {
    walk.charge(1)?;
    match (types.desc(t)?, w) {
        (TypeDesc::Unit, WireValue::Unit) => Ok(0),
        (TypeDesc::Bool, WireValue::Bool(b)) => Ok(u64::from(*b)),
        (TypeDesc::Int, WireValue::Int(i)) => Ok(*i as u64),
        (TypeDesc::Float, WireValue::Float(f)) => Ok(canonical_bits(*f)),
        (TypeDesc::Str, WireValue::Str(s)) => {
            walk.charge(u32::try_from(s.len().div_ceil(8)).unwrap_or(u32::MAX))?;
            Ok(string::new(st, heap, s)?)
        }
        (TypeDesc::List(e), WireValue::List(items)) => nested(walk, |walk| {
            let mut l = list::new(st, heap, types.is_ref(*e))?;
            let r = fill_list(st, heap, types, *e, items, &mut l, walk);
            finish(st, heap, l, r)
        }),
        (TypeDesc::Dict(kt, vt), WireValue::Dict(pairs)) => nested(walk, |walk| {
            let d = dict::new(st, heap, types.is_ref(*kt), types.is_ref(*vt))?;
            let r = fill_dict(st, heap, types, (*kt, *vt), pairs, d, walk);
            finish(st, heap, d, r)
        }),
        (TypeDesc::Struct { fields, .. }, WireValue::Struct { fields: wf, .. }) => {
            nested(walk, |walk| build_obj(st, heap, types, 0, fields, wf, walk))
        }
        (TypeDesc::Enum { variants, .. }, WireValue::Variant { name, fields: wf }) => {
            let idx = variants
                .iter()
                .position(|(n, _)| n == name)
                .ok_or(FromWire::WrongType)?;
            let tag = u16::try_from(idx).map_err(|_| FaultKind::Corrupt)?;
            let fields = &variants[idx].1;
            if fields.is_empty() && wf.is_empty() {
                return Ok((u64::from(tag) << 1) | 1);
            }
            nested(walk, |walk| {
                build_obj(st, heap, types, tag, fields, wf, walk)
            })
        }
        _ => Err(FromWire::WrongType),
    }
}

fn fill_list(
    st: &mut Store,
    heap: Heap,
    types: &TypeTable,
    e: TypeId,
    items: &[WireValue],
    l: &mut u64,
    walk: &mut Walk,
) -> Built<()> {
    for item in items {
        let v = from_w(st, heap, types, e, item, walk)?;
        match list::push(st, heap, *l, v) {
            Ok((nl, work)) => {
                *l = nl;
                walk.charge(work)?;
            }
            Err(f) => {
                drop_value(st, heap, types, e, v);
                return Err(f.into());
            }
        }
    }
    Ok(())
}

fn fill_dict(
    st: &mut Store,
    heap: Heap,
    types: &TypeTable,
    (kt, vt): (TypeId, TypeId),
    pairs: &[(WireValue, WireValue)],
    d: u64,
    walk: &mut Walk,
) -> Built<()> {
    for (kw, vw) in pairs {
        let k = from_w(st, heap, types, kt, kw, walk)?;
        let v = match from_w(st, heap, types, vt, vw, walk) {
            Ok(v) => v,
            Err(e) => {
                drop_value(st, heap, types, kt, k);
                return Err(e);
            }
        };
        let before = dict::len(st, heap, d)?;
        match dict::set(st, heap, d, k, v) {
            Ok((_, work)) => walk.charge(work)?,
            Err(f) => {
                drop_value(st, heap, types, kt, k);
                drop_value(st, heap, types, vt, v);
                return Err(f.into());
            }
        }
        if dict::len(st, heap, d)? == before {
            return Err(FromWire::WrongType); // the key was there already
        }
    }
    Ok(())
}

/// A struct (`tag` 0) or a variant with fields: `wf` must name exactly the declared `fields`.
fn build_obj(
    st: &mut Store,
    heap: Heap,
    types: &TypeTable,
    tag: u16,
    fields: &[(String, TypeId)],
    wf: &[(String, WireValue)],
    walk: &mut Walk,
) -> Built<u64> {
    if fields.len() != wf.len() {
        return Err(FromWire::WrongType);
    }
    if fields.len() > STRUCT_FIELDS_MAX {
        return Err(FaultKind::Corrupt.into());
    }
    // Which wire field fills each declared one: every name present, none used twice.
    let mut used = vec![false; wf.len()];
    let mut order = Vec::new();
    for (name, _) in fields {
        let j = wf
            .iter()
            .position(|(n, _)| n == name)
            .ok_or(FromWire::WrongType)?;
        if std::mem::replace(&mut used[j], true) {
            return Err(FromWire::WrongType);
        }
        order.push(j);
    }
    let mut ref_bits = 0u32;
    for (i, (_, ft)) in fields.iter().enumerate() {
        ref_bits |= u32::from(types.is_ref(*ft)) << i;
    }
    let n = u8::try_from(fields.len()).map_err(|_| FaultKind::Corrupt)?;
    let o = obj::new(st, heap, n, ref_bits, tag)?;
    let r = fill_obj(st, heap, types, o, fields, wf, &order, walk);
    finish(st, heap, o, r)
}

#[allow(clippy::too_many_arguments)]
fn fill_obj(
    st: &mut Store,
    heap: Heap,
    types: &TypeTable,
    o: u64,
    fields: &[(String, TypeId)],
    wf: &[(String, WireValue)],
    order: &[usize],
    walk: &mut Walk,
) -> Built<()> {
    for (i, ((_, ft), &j)) in (0u32..).zip(fields.iter().zip(order)) {
        let v = from_w(st, heap, types, *ft, &wf[j].1, walk)?;
        if let Err(f) = obj::set_field(st, heap, o, i, v) {
            drop_value(st, heap, types, *ft, v);
            return Err(f.into());
        }
    }
    Ok(())
}
