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
//! `ValueTooLarge`) and report the words they visited. A `from_wire` that fails halfway releases what it
//! built (the caller reaps).
//!
//! # No recursion
//! Values have no nesting cap (recursive data types are allowed, decision 17). Every walk here — encode,
//! decode, clone, compare, drop, [`to_wire`], [`from_wire`], [`fits`] — is a loop over an explicit stack
//! that visits in the order a recursive pre-order walk would (the same bytes, the same charges, the same
//! allocation order). Untrusted bytes are bounded by their length, heap walks by the budget.

use crate::alloc::{Heap, Kind};
use crate::consts::STRUCT_FIELDS_MAX;
use crate::dict;
use crate::eq::Walk;
use crate::fault::{FaultKind, VmResult};
use crate::list;
use crate::object::{check_obj_shape, hdr, obj, release};
use crate::store::Store;
use crate::string;
use crate::types::{TypeDesc, TypeId, TypeTable};
use state_hash::{StateHash, hash_len};
use std::hash::Hasher;

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

/// A self-describing value. `Clone`, `PartialEq` (floats by `f64` `==`), `Debug` and `Drop` are written by
/// hand as loops: a value can nest arbitrarily deep, and the derived ones would recurse. Code outside this
/// crate takes a value apart with [`WireValue::into_list`], [`WireValue::into_fields`] and
/// [`WireValue::into_pairs`] (`Drop` forbids moving fields out by a pattern).
#[derive(Default)]
pub enum WireValue {
    #[default]
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
}

/// Any NaN becomes the one canonical NaN.
pub(crate) fn canonical_bits(f: f64) -> u64 {
    if f.is_nan() { NAN_BITS } else { f.to_bits() }
}

// ---- tree plumbing -----------------------------------------------------------------------------------

impl WireValue {
    /// The `i`-th child: a list's items, a dict's keys and values alternating, a struct's or variant's
    /// field values; `None` past the last (and always for a scalar).
    fn child(&self, i: usize) -> Option<&WireValue> {
        match self {
            WireValue::List(items) => items.get(i),
            WireValue::Dict(pairs) => pairs
                .get(i / 2)
                .map(|(k, v)| if i.is_multiple_of(2) { k } else { v }),
            WireValue::Struct { fields, .. } | WireValue::Variant { fields, .. } => {
                fields.get(i).map(|(_, v)| v)
            }
            _ => None,
        }
    }

    /// This value with `kids` (in [`WireValue::child`] order) in place of its children.
    fn rebuild(&self, kids: Vec<WireValue>) -> WireValue {
        let named = |fields: &[(String, WireValue)], kids: Vec<WireValue>| {
            fields.iter().map(|(n, _)| n.clone()).zip(kids).collect()
        };
        match self {
            WireValue::Unit => WireValue::Unit,
            WireValue::Bool(b) => WireValue::Bool(*b),
            WireValue::Int(i) => WireValue::Int(*i),
            WireValue::Float(f) => WireValue::Float(*f),
            WireValue::Str(s) => WireValue::Str(s.clone()),
            WireValue::List(_) => WireValue::List(kids),
            WireValue::Dict(_) => {
                let mut pairs = Vec::with_capacity(kids.len() / 2);
                let mut it = kids.into_iter();
                while let (Some(k), Some(v)) = (it.next(), it.next()) {
                    pairs.push((k, v));
                }
                WireValue::Dict(pairs)
            }
            WireValue::Struct { name, fields } => WireValue::Struct {
                name: name.clone(),
                fields: named(fields, kids),
            },
            WireValue::Variant { name, fields } => WireValue::Variant {
                name: name.clone(),
                fields: named(fields, kids),
            },
        }
    }

    /// The items of a `List` (`Drop` forbids moving them out by a pattern).
    pub fn into_list(mut self) -> Option<Vec<WireValue>> {
        match &mut self {
            WireValue::List(items) => Some(std::mem::take(items)),
            _ => None,
        }
    }

    /// The name and fields of a `Struct` or `Variant`.
    pub fn into_fields(mut self) -> Option<(String, Vec<(String, WireValue)>)> {
        match &mut self {
            WireValue::Struct { name, fields } | WireValue::Variant { name, fields } => {
                Some((std::mem::take(name), std::mem::take(fields)))
            }
            _ => None,
        }
    }

    /// The pairs of a `Dict`, in order.
    pub fn into_pairs(mut self) -> Option<Vec<(WireValue, WireValue)>> {
        match &mut self {
            WireValue::Dict(pairs) => Some(std::mem::take(pairs)),
            _ => None,
        }
    }

    /// Moves this value's children onto `out`, leaving it without any.
    fn take_children(&mut self, out: &mut Vec<WireValue>) {
        match self {
            WireValue::List(items) => out.append(items),
            WireValue::Dict(pairs) => {
                for (k, v) in std::mem::take(pairs) {
                    out.push(k);
                    out.push(v);
                }
            }
            WireValue::Struct { fields, .. } | WireValue::Variant { fields, .. } => {
                out.extend(std::mem::take(fields).into_iter().map(|(_, v)| v));
            }
            _ => {}
        }
    }
}

impl Drop for WireValue {
    /// Flat: the children are emptied onto a local stack one container at a time, so every value dropped
    /// here has no children left and nothing recurses.
    fn drop(&mut self) {
        let mut stack = Vec::new();
        self.take_children(&mut stack);
        while let Some(mut v) = stack.pop() {
            v.take_children(&mut stack);
        }
    }
}

/// A piece of [`WireValue`]'s `Debug` output still to be written.
enum DebugTok<'a> {
    Lit(&'static str),
    Name(&'a str),
    Val(&'a WireValue),
}

impl std::fmt::Debug for WireValue {
    /// The text the derived `Debug` writes (`List([Int(1), Str("a")])`, `Struct { name: "P", fields:
    /// [("n", Int(1))] }`), from an explicit stack of pieces so nothing recurses. The alternate form
    /// (`{:#?}`) is the same single line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use DebugTok::{Lit, Name, Val};
        let mut stack = vec![Val(self)];
        while let Some(tok) = stack.pop() {
            let v = match tok {
                Lit(s) => {
                    f.write_str(s)?;
                    continue;
                }
                Name(n) => {
                    write!(f, "{n:?}")?;
                    continue;
                }
                Val(v) => v,
            };
            match v {
                WireValue::Unit => f.write_str("Unit")?,
                WireValue::Bool(b) => write!(f, "Bool({b:?})")?,
                WireValue::Int(i) => write!(f, "Int({i:?})")?,
                WireValue::Float(x) => write!(f, "Float({x:?})")?,
                WireValue::Str(s) => write!(f, "Str({s:?})")?,
                WireValue::List(items) => {
                    f.write_str("List([")?;
                    stack.push(Lit("])"));
                    for (i, item) in items.iter().enumerate().rev() {
                        stack.push(Val(item));
                        if i > 0 {
                            stack.push(Lit(", "));
                        }
                    }
                }
                WireValue::Dict(pairs) => {
                    f.write_str("Dict([")?;
                    stack.push(Lit("])"));
                    for (i, (k, v)) in pairs.iter().enumerate().rev() {
                        stack.extend([Lit(")"), Val(v), Lit(", "), Val(k), Lit("(")]);
                        if i > 0 {
                            stack.push(Lit(", "));
                        }
                    }
                }
                WireValue::Struct { name, fields } | WireValue::Variant { name, fields } => {
                    let kind = if matches!(v, WireValue::Struct { .. }) {
                        "Struct"
                    } else {
                        "Variant"
                    };
                    write!(f, "{kind} {{ name: {name:?}, fields: [")?;
                    stack.push(Lit("] }"));
                    for (i, (n, v)) in fields.iter().enumerate().rev() {
                        stack.extend([Lit(")"), Val(v), Lit(", "), Name(n), Lit("(")]);
                        if i > 0 {
                            stack.push(Lit(", "));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Clone for WireValue {
    fn clone(&self) -> WireValue {
        // Each frame: a source container and the copies of its children so far.
        let mut stack: Vec<(&WireValue, Vec<WireValue>)> = vec![(self, Vec::new())];
        let mut done = WireValue::Unit;
        while let Some((src, kids)) = stack.last_mut() {
            let src: &WireValue = src;
            if let Some(c) = src.child(kids.len()) {
                stack.push((c, Vec::new()));
                continue;
            }
            let kids = std::mem::take(kids);
            stack.pop();
            let copy = src.rebuild(kids);
            match stack.last_mut() {
                Some((_, parent)) => parent.push(copy),
                None => done = copy,
            }
        }
        done
    }
}

impl WireValue {
    /// Equality by representation: as `==`, except that floats compare by their bits, any NaN canonical
    /// (`-0.0` differs from `0.0`, NaN equals NaN) — what the state hash tells apart.
    pub fn bit_eq(&self, other: &WireValue) -> bool {
        let mut stack = vec![(self, other)];
        while let Some((a, b)) = stack.pop() {
            let same = match (a, b) {
                (WireValue::Float(x), WireValue::Float(y)) => {
                    canonical_bits(*x) == canonical_bits(*y)
                }
                (WireValue::Float(_), _) | (_, WireValue::Float(_)) => false,
                // Same shape (the `==` of everything but the children, which go on the stack).
                _ => a.shape_eq(b),
            };
            if !same {
                return false;
            }
            let mut i = 0;
            while let (Some(x), Some(y)) = (a.child(i), b.child(i)) {
                stack.push((x, y));
                i += 1;
            }
        }
        true
    }

    /// The two values' kinds, scalars, lengths and names agree (children not compared).
    fn shape_eq(&self, other: &WireValue) -> bool {
        match (self, other) {
            (WireValue::Unit, WireValue::Unit) => true,
            (WireValue::Bool(x), WireValue::Bool(y)) => x == y,
            (WireValue::Int(x), WireValue::Int(y)) => x == y,
            (WireValue::Float(x), WireValue::Float(y)) => x == y,
            (WireValue::Str(x), WireValue::Str(y)) => x == y,
            (WireValue::List(x), WireValue::List(y)) => x.len() == y.len(),
            (WireValue::Dict(x), WireValue::Dict(y)) => x.len() == y.len(),
            (
                WireValue::Struct { name: n, fields: f },
                WireValue::Struct { name: m, fields: g },
            )
            | (
                WireValue::Variant { name: n, fields: f },
                WireValue::Variant { name: m, fields: g },
            ) => n == m && f.len() == g.len() && f.iter().zip(g).all(|((x, _), (y, _))| x == y),
            _ => false,
        }
    }
}

impl PartialEq for WireValue {
    fn eq(&self, other: &WireValue) -> bool {
        let mut stack = vec![(self, other)];
        while let Some((a, b)) = stack.pop() {
            if !a.shape_eq(b) {
                return false;
            }
            let mut i = 0;
            while let (Some(x), Some(y)) = (a.child(i), b.child(i)) {
                stack.push((x, y));
                i += 1;
            }
        }
        true
    }
}

// ---- bytes -------------------------------------------------------------------------------------------

fn put_len(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_len(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

/// A container being decoded: what it holds so far and how many values it still needs (at least one).
enum Open {
    List {
        left: u32,
        items: Vec<WireValue>,
    },
    Dict {
        left: u32,
        pairs: Vec<(WireValue, WireValue)>,
        key: Option<WireValue>,
    },
    Fields {
        variant: bool,
        name: String,
        left: u32,
        fields: Vec<(String, WireValue)>,
        /// The name of the field whose value is read next.
        field: String,
    },
}

impl Open {
    /// Takes the next value; returns the container once it is complete.
    fn accept(&mut self, v: WireValue) -> Option<WireValue> {
        use std::mem::take;
        match self {
            Open::List { left, items } => {
                items.push(v);
                *left -= 1;
                (*left == 0).then(|| WireValue::List(take(items)))
            }
            Open::Dict { left, pairs, key } => {
                let Some(k) = key.take() else {
                    *key = Some(v);
                    return None;
                };
                pairs.push((k, v));
                *left -= 1;
                (*left == 0).then(|| WireValue::Dict(take(pairs)))
            }
            Open::Fields {
                variant,
                name,
                left,
                fields,
                field,
            } => {
                fields.push((take(field), v));
                *left -= 1;
                (*left == 0).then(|| fields_value(*variant, take(name), take(fields)))
            }
        }
    }
}

fn fields_value(variant: bool, name: String, fields: Vec<(String, WireValue)>) -> WireValue {
    if variant {
        WireValue::Variant { name, fields }
    } else {
        WireValue::Struct { name, fields }
    }
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

    /// One value. Containers are never sized by their untrusted counts, and every value takes bytes, so
    /// both the loop and the stack of open containers are bounded by the input's length.
    fn value(&mut self) -> Result<WireValue, WireError> {
        let mut stack: Vec<Open> = Vec::new();
        loop {
            if let Some(Open::Fields { field, .. }) = stack.last_mut() {
                *field = self.string()?;
            }
            let mut done = match self.u8()? {
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
                T_LIST => match self.u32()? {
                    0 => WireValue::List(Vec::new()),
                    left => {
                        stack.push(Open::List {
                            left,
                            items: Vec::new(),
                        });
                        continue;
                    }
                },
                T_DICT => match self.u32()? {
                    0 => WireValue::Dict(Vec::new()),
                    left => {
                        stack.push(Open::Dict {
                            left,
                            pairs: Vec::new(),
                            key: None,
                        });
                        continue;
                    }
                },
                tag @ (T_STRUCT | T_VARIANT) => {
                    let variant = tag == T_VARIANT;
                    let name = self.string()?;
                    match self.u32()? {
                        0 => fields_value(variant, name, Vec::new()),
                        left => {
                            stack.push(Open::Fields {
                                variant,
                                name,
                                left,
                                fields: Vec::new(),
                                field: String::new(),
                            });
                            continue;
                        }
                    }
                }
                t => return Err(WireError::BadTag(t)),
            };
            // Hand the finished value up through every container it completes.
            loop {
                let Some(top) = stack.last_mut() else {
                    return Ok(done);
                };
                match top.accept(done) {
                    None => break,
                    Some(full) => {
                        stack.pop();
                        done = full;
                    }
                }
            }
        }
    }
}

impl WireValue {
    /// Appends the bytes of this value (floats canonicalised).
    pub fn encode(&self, out: &mut Vec<u8>) {
        enum Item<'a> {
            Value(&'a WireValue),
            Name(&'a str),
        }
        let mut stack = vec![Item::Value(self)];
        while let Some(item) = stack.pop() {
            let v = match item {
                Item::Name(n) => {
                    put_str(out, n);
                    continue;
                }
                Item::Value(v) => v,
            };
            match v {
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
                    stack.extend(items.iter().rev().map(Item::Value));
                }
                WireValue::Dict(pairs) => {
                    out.push(T_DICT);
                    put_len(out, pairs.len());
                    for (k, v) in pairs.iter().rev() {
                        stack.push(Item::Value(v));
                        stack.push(Item::Value(k));
                    }
                }
                WireValue::Struct { name, fields } | WireValue::Variant { name, fields } => {
                    let tag = if matches!(v, WireValue::Struct { .. }) {
                        T_STRUCT
                    } else {
                        T_VARIANT
                    };
                    out.push(tag);
                    put_str(out, name);
                    put_len(out, fields.len());
                    for (n, v) in fields.iter().rev() {
                        stack.push(Item::Value(v));
                        stack.push(Item::Name(n));
                    }
                }
            }
        }
    }

    /// Parses exactly one value; never panics, never allocates by an untrusted count, never recurses.
    pub fn decode(bytes: &[u8]) -> Result<WireValue, WireError> {
        let mut r = Reader { bytes, pos: 0 };
        let v = r.value()?;
        if r.pos != bytes.len() {
            return Err(WireError::Trailing);
        }
        Ok(v)
    }
}

// ---- heap -> wire ------------------------------------------------------------------------------------

/// A container being read out of the heap by [`to_wire`], innermost last.
enum ToFrame<'t> {
    List {
        v: u64,
        e: TypeId,
        len: u32,
        items: Vec<WireValue>,
    },
    Dict {
        v: u64,
        kt: TypeId,
        vt: TypeId,
        /// Where the next `dict::next` starts.
        pos: i64,
        /// The value of the entry whose key was handed out last, and that key once read.
        val: u64,
        key: Option<WireValue>,
        pairs: Vec<(WireValue, WireValue)>,
    },
    Obj {
        v: u64,
        name: &'t str,
        variant: bool,
        fields: &'t [(String, TypeId)],
        out: Vec<(String, WireValue)>,
    },
}

/// What opening a slot gives: a finished value, or a container whose children are read next.
enum Opened<V, F> {
    Leaf(V),
    Frame(F),
}

impl ToFrame<'_> {
    /// The next child to read (its type and slot), or `None` when the container is complete.
    fn next(&mut self, st: &Store, heap: Heap, walk: &mut Walk) -> VmResult<Option<(TypeId, u64)>> {
        Ok(match self {
            ToFrame::List { v, e, len, items } => {
                let i = u32::try_from(items.len()).map_err(|_| FaultKind::Corrupt)?;
                if i >= *len {
                    return Ok(None);
                }
                Some((*e, list::get(st, heap, *v, i64::from(i))?))
            }
            ToFrame::Dict {
                v,
                kt,
                vt,
                pos,
                val,
                key,
                ..
            } => {
                if key.is_some() {
                    return Ok(Some((*vt, *val)));
                }
                let (serial, work) = dict::next(st, heap, *v, *pos)?;
                walk.charge(work)?;
                if serial < 0 {
                    return Ok(None);
                }
                // `next` charged the walk to this entry: the two searches below find it again.
                let (k, _) = dict::key_at(st, heap, *v, serial)?;
                (*val, _) = dict::val_at(st, heap, *v, serial)?;
                *pos = serial + 1;
                Some((*kt, k))
            }
            ToFrame::Obj { v, fields, out, .. } => {
                let Some((_, ft)) = fields.get(out.len()) else {
                    return Ok(None);
                };
                let i = u32::try_from(out.len()).map_err(|_| FaultKind::Corrupt)?;
                Some((*ft, obj::get_field(st, heap, *v, i)?))
            }
        })
    }

    /// Takes the wire form of the child [`ToFrame::next`] handed out last.
    fn accept(&mut self, w: WireValue) {
        match self {
            ToFrame::List { items, .. } => items.push(w),
            ToFrame::Dict { key, pairs, .. } => match key.take() {
                None => *key = Some(w),
                Some(k) => pairs.push((k, w)),
            },
            ToFrame::Obj { fields, out, .. } => {
                if let Some((name, _)) = fields.get(out.len()) {
                    out.push((name.clone(), w));
                }
            }
        }
    }

    fn finish(self) -> WireValue {
        match self {
            ToFrame::List { items, .. } => WireValue::List(items),
            ToFrame::Dict { pairs, .. } => WireValue::Dict(pairs),
            ToFrame::Obj {
                name, variant, out, ..
            } => fields_value(variant, name.to_owned(), out),
        }
    }
}

/// The wire form of the heap value `v` of type `t`, and the words visited.
pub fn to_wire(
    st: &Store,
    heap: Heap,
    types: &TypeTable,
    t: TypeId,
    v: u64,
) -> VmResult<(WireValue, u32)> {
    let mut walk = Walk::default();
    let mut stack = match to_open(st, heap, types, t, v, &mut walk)? {
        Opened::Leaf(w) => return Ok((w, walk.used)),
        Opened::Frame(f) => vec![f],
    };
    let mut done = WireValue::Unit;
    while let Some(top) = stack.last_mut() {
        if let Some((t, v)) = top.next(st, heap, &mut walk)? {
            match to_open(st, heap, types, t, v, &mut walk)? {
                Opened::Leaf(w) => top.accept(w),
                Opened::Frame(f) => stack.push(f),
            }
        } else if let Some(f) = stack.pop() {
            let w = f.finish();
            match stack.last_mut() {
                Some(parent) => parent.accept(w),
                None => done = w,
            }
        }
    }
    Ok((done, walk.used))
}

/// One slot of [`to_wire`]: charges it, checks the heap object against the type and reads a scalar
/// whole; a container becomes a frame.
fn to_open<'t>(
    st: &Store,
    heap: Heap,
    types: &'t TypeTable,
    t: TypeId,
    v: u64,
    walk: &mut Walk,
) -> VmResult<Opened<WireValue, ToFrame<'t>>> {
    walk.charge(1)?;
    let leaf = match types.desc(t)? {
        TypeDesc::Unit => WireValue::Unit,
        TypeDesc::Bool => match v {
            0 => WireValue::Bool(false),
            1 => WireValue::Bool(true),
            _ => return Err(FaultKind::Corrupt),
        },
        TypeDesc::Int => WireValue::Int(v as i64),
        TypeDesc::Float => WireValue::Float(f64::from_bits(canonical_bits(f64::from_bits(v)))),
        TypeDesc::Promise(_) => return Err(FaultKind::Corrupt),
        TypeDesc::Str => {
            walk.charge(string::byte_len(st, heap, v)?.div_ceil(8))?;
            WireValue::Str(string::read(st, heap, v)?)
        }
        TypeDesc::List(e) => {
            let (_, h) = hdr(st, heap, v)?;
            let want = if types.is_ref(*e) {
                Kind::ListRef
            } else {
                Kind::ListImm
            };
            if h.kind != want {
                return Err(FaultKind::Corrupt);
            }
            return Ok(Opened::Frame(ToFrame::List {
                v,
                e: *e,
                len: h.len,
                items: Vec::new(),
            }));
        }
        TypeDesc::Dict(kt, vt) => {
            let (_, h) = hdr(st, heap, v)?;
            if h.kind != Kind::Dict {
                return Err(FaultKind::Corrupt);
            }
            return Ok(Opened::Frame(ToFrame::Dict {
                v,
                kt: *kt,
                vt: *vt,
                pos: 0,
                val: 0,
                key: None,
                pairs: Vec::new(),
            }));
        }
        TypeDesc::Struct { name, fields } => {
            check_obj(st, heap, v, 0, fields)?;
            return Ok(Opened::Frame(ToFrame::Obj {
                v,
                name,
                variant: false,
                fields,
                out: Vec::new(),
            }));
        }
        TypeDesc::Enum { variants, .. } => {
            let tag = obj::tag(st, heap, v)?;
            let (vname, vfields) = variants.get(usize::from(tag)).ok_or(FaultKind::Corrupt)?;
            if v & 1 == 1 {
                if !vfields.is_empty() {
                    return Err(FaultKind::Corrupt);
                }
                WireValue::Variant {
                    name: vname.clone(),
                    fields: vec![],
                }
            } else {
                check_obj(st, heap, v, tag, vfields)?;
                return Ok(Opened::Frame(ToFrame::Obj {
                    v,
                    name: vname,
                    variant: true,
                    fields: vfields,
                    out: Vec::new(),
                }));
            }
        }
    };
    Ok(Opened::Leaf(leaf))
}

/// The object `v` has tag `tag` and the declared `fields`' count.
fn check_obj(
    st: &Store,
    heap: Heap,
    v: u64,
    tag: u16,
    fields: &[(String, TypeId)],
) -> VmResult<()> {
    let (_, h) = hdr(st, heap, v)?;
    if h.kind != Kind::Obj {
        return Err(FaultKind::Corrupt);
    }
    check_obj_shape(&h)?;
    if h.tag != tag || usize::try_from(h.len).ok() != Some(fields.len()) {
        return Err(FaultKind::Corrupt);
    }
    Ok(())
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
    let mut stack = Vec::new();
    match build(st, heap, types, t, w, &mut walk, &mut stack) {
        Ok(v) => Ok((v, walk.used)),
        Err(e) => {
            // innermost first, as each level of the recursive build released its own on the way out
            while let Some(f) = stack.pop() {
                f.release(st, heap, types);
            }
            Err(e)
        }
    }
}

/// A container being built by [`from_wire`], innermost last. Its heap object exists before its children
/// are built; on an error the caller releases every frame's.
enum FromFrame<'w, 't> {
    List {
        l: u64,
        e: TypeId,
        items: &'w [WireValue],
        i: usize,
    },
    Dict {
        d: u64,
        kt: TypeId,
        vt: TypeId,
        pairs: &'w [(WireValue, WireValue)],
        i: usize,
        /// The key of pair `i`, built, while its value is built.
        key: Option<u64>,
    },
    Obj {
        o: u64,
        fields: &'t [(String, TypeId)],
        wf: &'w [(String, WireValue)],
        /// Which wire field fills each declared one.
        order: Vec<usize>,
        i: usize,
    },
}

impl<'w> FromFrame<'w, '_> {
    /// The next child to build (its type and wire value), or `None` when the container is complete.
    fn next(&self) -> Option<(TypeId, &'w WireValue)> {
        match self {
            FromFrame::List { e, items, i, .. } => items.get(*i).map(|w| (*e, w)),
            FromFrame::Dict {
                kt,
                vt,
                pairs,
                i,
                key,
                ..
            } => pairs
                .get(*i)
                .map(|(k, v)| if key.is_none() { (*kt, k) } else { (*vt, v) }),
            FromFrame::Obj {
                fields,
                wf,
                order,
                i,
                ..
            } => {
                let (_, ft) = fields.get(*i)?;
                let (_, w) = wf.get(*order.get(*i)?)?;
                Some((*ft, w))
            }
        }
    }

    /// Takes the child [`FromFrame::next`] handed out last (the frame owns it from here, also on an
    /// error).
    fn accept(
        &mut self,
        st: &mut Store,
        heap: Heap,
        types: &TypeTable,
        v: u64,
        walk: &mut Walk,
    ) -> Built<()> {
        match self {
            FromFrame::List { l, e, i, .. } => {
                *i += 1;
                match list::push(st, heap, *l, v) {
                    Ok((nl, work)) => {
                        *l = nl;
                        walk.charge(work)?;
                    }
                    Err(f) => {
                        drop_value(st, heap, types, *e, v);
                        return Err(f.into());
                    }
                }
            }
            FromFrame::Dict {
                d, kt, vt, i, key, ..
            } => {
                let Some(k) = key.take() else {
                    *key = Some(v);
                    return Ok(());
                };
                *i += 1;
                let before = dict::len(st, heap, *d)?;
                match dict::set(st, heap, *d, k, v) {
                    Ok((_, work)) => walk.charge(work)?,
                    Err(f) => {
                        drop_value(st, heap, types, *kt, k);
                        drop_value(st, heap, types, *vt, v);
                        return Err(f.into());
                    }
                }
                if dict::len(st, heap, *d)? == before {
                    return Err(FromWire::WrongType); // the key was there already
                }
            }
            FromFrame::Obj { o, fields, i, .. } => {
                let ft = fields.get(*i).map(|(_, ft)| *ft);
                let at = u32::try_from(*i).map_err(|_| FaultKind::Corrupt)?;
                *i += 1;
                if let Err(f) = obj::set_field(st, heap, *o, at, v) {
                    if let Some(ft) = ft {
                        drop_value(st, heap, types, ft, v);
                    }
                    return Err(f.into());
                }
            }
        }
        Ok(())
    }

    /// The finished container.
    fn value(&self) -> u64 {
        match self {
            FromFrame::List { l, .. } => *l,
            FromFrame::Dict { d, .. } => *d,
            FromFrame::Obj { o, .. } => *o,
        }
    }

    /// After an error: a key held for its value, then the container with whatever it already holds.
    fn release(self, st: &mut Store, heap: Heap, types: &TypeTable) {
        if let FromFrame::Dict {
            kt, key: Some(k), ..
        } = self
        {
            drop_value(st, heap, types, kt, k);
        }
        let _ = release(st, heap, self.value());
    }
}

/// [`from_wire`]'s walk; on an error the frames left on `stack` hold what was built.
fn build<'w, 't>(
    st: &mut Store,
    heap: Heap,
    types: &'t TypeTable,
    t: TypeId,
    w: &'w WireValue,
    walk: &mut Walk,
    stack: &mut Vec<FromFrame<'w, 't>>,
) -> Built<u64> {
    match from_open(st, heap, types, t, w, walk)? {
        Opened::Leaf(v) => return Ok(v),
        Opened::Frame(f) => stack.push(f),
    }
    let mut done = 0;
    while let Some(top) = stack.last_mut() {
        if let Some((t, w)) = top.next() {
            match from_open(st, heap, types, t, w, walk)? {
                Opened::Leaf(v) => top.accept(st, heap, types, v, walk)?,
                Opened::Frame(f) => stack.push(f),
            }
        } else if let Some(f) = stack.pop() {
            let v = f.value();
            match stack.last_mut() {
                Some(parent) => parent.accept(st, heap, types, v, walk)?,
                None => done = v,
            }
        }
    }
    Ok(done)
}

/// One wire value of [`from_wire`]: charges it, matches it against the type and builds a scalar or a
/// string whole; a container is allocated (before its children, as before) and becomes a frame.
fn from_open<'w, 't>(
    st: &mut Store,
    heap: Heap,
    types: &'t TypeTable,
    t: TypeId,
    w: &'w WireValue,
    walk: &mut Walk,
) -> Built<Opened<u64, FromFrame<'w, 't>>> {
    walk.charge(1)?;
    let frame = match (types.desc(t)?, w) {
        (TypeDesc::Unit, WireValue::Unit) => return Ok(Opened::Leaf(0)),
        (TypeDesc::Bool, WireValue::Bool(b)) => return Ok(Opened::Leaf(u64::from(*b))),
        (TypeDesc::Int, WireValue::Int(i)) => return Ok(Opened::Leaf(*i as u64)),
        (TypeDesc::Float, WireValue::Float(f)) => return Ok(Opened::Leaf(canonical_bits(*f))),
        (TypeDesc::Str, WireValue::Str(s)) => {
            walk.charge(u32::try_from(s.len().div_ceil(8)).unwrap_or(u32::MAX))?;
            return Ok(Opened::Leaf(string::new(st, heap, s)?));
        }
        (TypeDesc::List(e), WireValue::List(items)) => FromFrame::List {
            l: list::new(st, heap, types.is_ref(*e))?,
            e: *e,
            items,
            i: 0,
        },
        (TypeDesc::Dict(kt, vt), WireValue::Dict(pairs)) => FromFrame::Dict {
            d: dict::new(st, heap, types.is_ref(*kt), types.is_ref(*vt))?,
            kt: *kt,
            vt: *vt,
            pairs,
            i: 0,
            key: None,
        },
        (TypeDesc::Struct { fields, .. }, WireValue::Struct { fields: wf, .. }) => {
            obj_frame(st, heap, types, 0, fields, wf)?
        }
        (TypeDesc::Enum { variants, .. }, WireValue::Variant { name, fields: wf }) => {
            let idx = variants
                .iter()
                .position(|(n, _)| n == name)
                .ok_or(FromWire::WrongType)?;
            let tag = u16::try_from(idx).map_err(|_| FaultKind::Corrupt)?;
            let fields = &variants[idx].1;
            if fields.is_empty() && wf.is_empty() {
                return Ok(Opened::Leaf((u64::from(tag) << 1) | 1));
            }
            obj_frame(st, heap, types, tag, fields, wf)?
        }
        _ => return Err(FromWire::WrongType),
    };
    Ok(Opened::Frame(frame))
}

/// A struct (`tag` 0) or a variant with fields: `wf` must name exactly the declared `fields`.
fn obj_frame<'w, 't>(
    st: &mut Store,
    heap: Heap,
    types: &TypeTable,
    tag: u16,
    fields: &'t [(String, TypeId)],
    wf: &'w [(String, WireValue)],
) -> Built<FromFrame<'w, 't>> {
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
    Ok(FromFrame::Obj {
        o: obj::new(st, heap, n, ref_bits, tag)?,
        fields,
        wf,
        order,
        i: 0,
    })
}

/// Whether [`from_wire`] of `w` into type `t` would find the shapes matching, decided without building
/// anything (a dry run of its structural checks: the same field matching, variant lookup and duplicate
/// dict keys). It does not check that the heap has room or that the walk stays under `WORK_WORDS_MAX`.
/// Duplicate dict keys are found by their encoded bytes.
pub fn fits(types: &TypeTable, t: TypeId, w: &WireValue) -> bool {
    let mut stack = vec![(t, w)];
    while let Some((t, w)) = stack.pop() {
        let Ok(d) = types.desc(t) else {
            return false;
        };
        let ok = match (d, w) {
            (TypeDesc::Unit, WireValue::Unit)
            | (TypeDesc::Bool, WireValue::Bool(_))
            | (TypeDesc::Int, WireValue::Int(_))
            | (TypeDesc::Float, WireValue::Float(_))
            | (TypeDesc::Str, WireValue::Str(_)) => true,
            (TypeDesc::List(e), WireValue::List(items)) => {
                stack.extend(items.iter().map(|i| (*e, i)));
                true
            }
            (TypeDesc::Dict(kt, vt), WireValue::Dict(pairs)) => {
                let mut keys = std::collections::BTreeSet::new();
                pairs.iter().all(|(k, v)| {
                    let mut bytes = Vec::new();
                    k.encode(&mut bytes);
                    stack.push((*kt, k));
                    stack.push((*vt, v));
                    keys.insert(bytes)
                })
            }
            (TypeDesc::Struct { fields, .. }, WireValue::Struct { fields: wf, .. }) => {
                fields_fit(fields, wf, &mut stack)
            }
            (TypeDesc::Enum { variants, .. }, WireValue::Variant { name, fields: wf }) => {
                match variants.iter().find(|(n, _)| n == name) {
                    Some((_, fields)) if fields.is_empty() && wf.is_empty() => true,
                    Some((_, fields)) => fields_fit(fields, wf, &mut stack),
                    None => false,
                }
            }
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// `obj_frame`'s matching: the same number of fields, each declared name found in a wire field of its
/// own; each value is pushed onto `stack` to fit its declared type.
fn fields_fit<'w>(
    fields: &[(String, TypeId)],
    wf: &'w [(String, WireValue)],
    stack: &mut Vec<(TypeId, &'w WireValue)>,
) -> bool {
    if fields.len() != wf.len() || fields.len() > STRUCT_FIELDS_MAX {
        return false;
    }
    let mut used = vec![false; wf.len()];
    fields
        .iter()
        .all(|(name, ft)| match wf.iter().position(|(n, _)| n == name) {
            Some(j) if !std::mem::replace(&mut used[j], true) => {
                stack.push((*ft, &wf[j].1));
                true
            }
            _ => false,
        })
}

/// Releases `v` when it is a heap-kind value of type `t` (a plain immediate looks like a pointer).
fn drop_value(st: &mut Store, heap: Heap, types: &TypeTable, t: TypeId, v: u64) {
    if types.is_ref(t) {
        let _ = release(st, heap, v);
    }
}

// ---- state hash --------------------------------------------------------------------------------------

/// Kinds of [`WireValue`] in its state hash.
const H_UNIT: u8 = 0;
const H_BOOL: u8 = 1;
const H_INT: u8 = 2;
const H_FLOAT: u8 = 3;
const H_STR: u8 = 4;
const H_LIST: u8 = 5;
const H_DICT: u8 = 6;
const H_STRUCT: u8 = 7;
const H_VARIANT: u8 = 8;

impl StateHash for WireValue {
    /// Pre-order: each value's kind, then its scalar, or its length (and name) before its children (a
    /// struct's field names with them). Floats by canonical bits. From an explicit stack: a value can
    /// nest arbitrarily deep.
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        enum Tok<'a> {
            Val(&'a WireValue),
            Name(&'a str),
        }
        let mut stack = vec![Tok::Val(self)];
        while let Some(tok) = stack.pop() {
            let v = match tok {
                Tok::Name(n) => {
                    n.state_hash(h);
                    continue;
                }
                Tok::Val(v) => v,
            };
            match v {
                WireValue::Unit => h.write_u8(H_UNIT),
                WireValue::Bool(b) => {
                    h.write_u8(H_BOOL);
                    b.state_hash(h);
                }
                WireValue::Int(i) => {
                    h.write_u8(H_INT);
                    i.state_hash(h);
                }
                WireValue::Float(x) => {
                    // Its bits (any NaN canonical): `-0.0` is another value than `0.0` here, as it is
                    // in the instance's memory (`f64::state_hash` would fold the two).
                    h.write_u8(H_FLOAT);
                    h.write_u64(canonical_bits(*x));
                }
                WireValue::Str(s) => {
                    h.write_u8(H_STR);
                    s.state_hash(h);
                }
                WireValue::List(items) => {
                    h.write_u8(H_LIST);
                    hash_len(items.len(), h);
                    stack.extend(items.iter().rev().map(Tok::Val));
                }
                WireValue::Dict(pairs) => {
                    h.write_u8(H_DICT);
                    hash_len(pairs.len(), h);
                    for (k, v) in pairs.iter().rev() {
                        stack.push(Tok::Val(v));
                        stack.push(Tok::Val(k));
                    }
                }
                WireValue::Struct { name, fields } | WireValue::Variant { name, fields } => {
                    let kind = if matches!(v, WireValue::Struct { .. }) {
                        H_STRUCT
                    } else {
                        H_VARIANT
                    };
                    h.write_u8(kind);
                    name.state_hash(h);
                    hash_len(fields.len(), h);
                    for (n, v) in fields.iter().rev() {
                        stack.push(Tok::Val(v));
                        stack.push(Tok::Name(n));
                    }
                }
            }
        }
    }
}
