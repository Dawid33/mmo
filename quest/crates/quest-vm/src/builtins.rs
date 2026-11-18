//! The builtins of design D5, called by `CallBuiltin a, b, c` (builtin `a`, arguments in the consecutive
//! slots from `b`, the result in `c`; `Builtin::arity` is fixed for every builtin and `validate` checked
//! the run of slots).
//!
//! # Conventions
//! * Arguments are borrowed, except the element `ListPush` / `ListInsert` store: it is moved into the
//!   list (its slot is zeroed when the list holds heap kinds).
//! * The mutating builtins (`ListPush`, `ListPop`, `ListInsert`, `ListRemove`, `DictRemove`) work on the
//!   container in their first argument slot: it is made unique there, mutated, and its (possibly moved)
//!   pointer is left in that slot.
//! * A builtin returning unit (`ListPush`, `ListInsert`, `Assert`, `Print`) does not write `c`.
//! * `Print` appends its text to the segment's command buffer (`sched`); the host sees it when the
//!   segment commits, and never when it aborts.
//! * A heap-kind result releases what `c` held. `Option[T]` results (`ListPop`, `DictGet`, `DictRemove`,
//!   `ParseInt`): `None` is the immediate `(0 << 1) | 1`; `Some(x)` an `Obj` of tag 1 with one field whose
//!   `aux` bit says whether `x` is a heap kind (from the container's kind or flags; an int for
//!   `ParseInt`). Booleans are 0 / 1.
//! * Every size-dependent path charges its words through the [`Meter`] (`ceil(words / 8)`).
//!
//! # Semantics pinned here
//! * Strings: `StrLen` counts characters; `StrUpper` / `StrLower` change ASCII letters only; `StrTrim`
//!   trims ASCII whitespace (space, `\t`, `\n`, `\r`) at both ends; `StrSplit` with an empty separator
//!   gives a one-element list of the whole string; `ParseInt` accepts an optional leading `-` and ASCII
//!   digits only (no `+`, spaces, `_` or hex) and is `None` on anything else or on overflow; `Fmt` formats
//!   its first argument as the type whose id its second holds, a top-level string raw.
//! * Numbers: math goes through `libm`, every float result has its NaN canonicalised. `MinF` / `MaxF`:
//!   a NaN argument gives the canonical NaN; of `-0.0` and `0.0`, `MinF` returns `-0.0` and `MaxF`
//!   `0.0`. `AbsI(i64::MIN)` is `IntOverflow`. `Round` rounds half away from zero. `ModF` is
//!   `libm::fmod` (the sign of the dividend, as Rust's `%` on floats).
//! * `StrCmp`: byte-lexicographic order of the two strings' UTF-8 (= code point order), as an int
//!   -1 / 0 / 1.
//! * `Assert`: a condition of 0 faults `AssertFailed` with the message string (the default text when the
//!   message slot holds no string).

use std::sync::Arc;

use crate::bytecode::{Builtin, Instr};
use crate::dict;
use crate::fault::FaultKind;
use crate::fmt::fmt_value;
use crate::interp::{Exec, Fault, Meter};
use crate::list;
use crate::object::{is_ptr, obj};
use crate::string;
use crate::vm::Vm;
use crate::wire::canonical_bits;

/// `None`: the field-less variant 0 as an immediate.
const NONE: u64 = 1;
/// The variant tag of `Some`.
const SOME_TAG: u16 = 1;

/// Words a string of `bytes` bytes occupies.
fn words_of(bytes: usize) -> u32 {
    u32::try_from(bytes.div_ceil(8)).unwrap_or(u32::MAX)
}

/// Argument `i` of a run starting at `base` (`validate` checked the run lies in the static region).
fn arg(base: u16, i: u16) -> Exec<u16> {
    base.checked_add(i)
        .ok_or_else(|| Fault::corrupt("argument slot out of range"))
}

fn float(v: u64) -> f64 {
    f64::from_bits(v)
}

/// `-0.0` before `0.0`; a NaN argument gives the canonical NaN.
fn min_f(x: f64, y: f64) -> u64 {
    if x.is_nan() || y.is_nan() {
        return canonical_bits(f64::NAN);
    }
    let r = if x == y {
        if x.is_sign_negative() { x } else { y }
    } else if x < y {
        x
    } else {
        y
    };
    r.to_bits()
}

/// `0.0` before `-0.0`; a NaN argument gives the canonical NaN.
fn max_f(x: f64, y: f64) -> u64 {
    if x.is_nan() || y.is_nan() {
        return canonical_bits(f64::NAN);
    }
    let r = if x == y {
        if x.is_sign_negative() { y } else { x }
    } else if x > y {
        x
    } else {
        y
    };
    r.to_bits()
}

/// An optional `-` then ASCII digits; `None` on anything else or on overflow.
fn parse_int(s: &str) -> Option<i64> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: i64 = 0;
    for b in digits.bytes() {
        let d = i64::from(b - b'0');
        v = v.checked_mul(10)?;
        // Accumulating on the sign's side reaches i64::MIN.
        v = if neg {
            v.checked_sub(d)?
        } else {
            v.checked_add(d)?
        };
    }
    Some(v)
}

fn is_ascii_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

impl Vm {
    /// `Some(v)`; ownership of `v` moves into it.
    fn some(&mut self, v: u64, is_ref: bool) -> Exec<u64> {
        let o = obj::new(&mut self.store, self.heap, 1, u32::from(is_ref), SOME_TAG)?;
        obj::set_field(&mut self.store, self.heap, o, 0, v)?;
        Ok(o)
    }

    /// The text of the string in `slot`, charging the words read.
    fn text_in(&self, slot: u16, m: &mut Meter) -> Exec<String> {
        let s = string::read(&self.store, self.heap, self.rd(slot)?)?;
        m.words(words_of(s.len()));
        Ok(s)
    }

    /// A new string of `s` into the result slot, charging the words written.
    fn put_text(&mut self, slot: u16, s: &str, m: &mut Meter) -> Exec<()> {
        let v = string::new(&mut self.store, self.heap, s)?;
        m.words(words_of(s.len()));
        self.put(slot, v, true)
    }

    pub(crate) fn call_builtin(&mut self, ins: Instr, m: &mut Meter) -> Exec<()> {
        use Builtin::*;
        let b = Builtin::from_u16(ins.a).ok_or_else(|| Fault::corrupt("unknown builtin"))?;
        let (args, res) = (ins.b, ins.c);
        match b {
            ListPush | ListPop | ListInsert | ListRemove | ListSlice | ListContains => {
                self.list_builtin(b, args, res, m)
            }
            DictGet | DictRemove | DictContains | DictKeys | DictValues | DictLen | DictNext
            | DictKeyAt | DictValAt => self.dict_builtin(b, args, res, m),
            StrLen | StrConcat | StrSplit | StrStartsWith | StrEndsWith | StrContains | StrTrim
            | StrUpper | StrLower | ParseInt | Fmt | StrCmp => self.str_builtin(b, args, res, m),
            AbsI | MinI | MaxI | AbsF | MinF | MaxF | Sqrt | Floor | Ceil | Round | Sin | Cos
            | Pow | ModF => self.math_builtin(b, args, res),
            Assert => self.assert_builtin(args),
            Print => {
                let text = self.text_in(args, m)?;
                self.buffer_print(&text, m)
            }
        }
    }

    fn list_builtin(&mut self, b: Builtin, args: u16, res: u16, m: &mut Meter) -> Exec<()> {
        let heap = self.heap;
        match b {
            Builtin::ListPush | Builtin::ListInsert => {
                let l = self.unique_in(args, m)?;
                let refs = self.list_refs(l)?;
                let (at, src) = if b == Builtin::ListPush {
                    let n = list::len(&self.store, heap, l)?;
                    (i64::from(n), arg(args, 1)?)
                } else {
                    (self.int_in(arg(args, 1)?)?, arg(args, 2)?)
                };
                let v = self.rd(src)?;
                let (nl, words) = list::insert(&mut self.store, heap, l, at, v)?;
                m.words(words);
                self.wt(args, nl)?;
                if refs { self.wt(src, 0) } else { Ok(()) }
            }
            Builtin::ListPop => {
                let l = self.unique_in(args, m)?;
                let refs = self.list_refs(l)?;
                let out = match list::pop(&mut self.store, heap, l)? {
                    Some(v) => self.some(v, refs)?,
                    None => NONE,
                };
                self.put(res, out, true)
            }
            Builtin::ListRemove => {
                let l = self.unique_in(args, m)?;
                let refs = self.list_refs(l)?;
                let i = self.int_in(arg(args, 1)?)?;
                let (v, words) = list::remove(&mut self.store, heap, l, i)?;
                m.words(words);
                self.put(res, v, refs)
            }
            Builtin::ListSlice => {
                let l = self.rd(args)?;
                let (lo, hi) = (self.int_in(arg(args, 1)?)?, self.int_in(arg(args, 2)?)?);
                let (out, words) = list::slice(&mut self.store, heap, l, lo, hi)?;
                m.words(words);
                self.put(res, out, true)
            }
            Builtin::ListContains => {
                let (l, v) = (self.rd(args)?, self.rd(arg(args, 1)?)?);
                let (found, words) = list::contains(&self.store, heap, l, v)?;
                m.words(words);
                self.wt(res, u64::from(found))
            }
            _ => Err(Fault::corrupt("not a list builtin")),
        }
    }

    fn dict_builtin(&mut self, b: Builtin, args: u16, res: u16, m: &mut Meter) -> Exec<()> {
        let heap = self.heap;
        let d = if b == Builtin::DictRemove {
            self.unique_in(args, m)?
        } else {
            self.rd(args)?
        };
        let (kr, vr) = dict::flags(&self.store, heap, d)?;
        // Every dict builtin but the unary ones reads a second argument.
        let second = || -> Exec<u64> { self.rd(arg(args, 1)?) };
        match b {
            Builtin::DictGet => {
                let (found, words) = dict::get(&self.store, heap, d, second()?)?;
                m.words(words);
                let out = match found {
                    Some(v) => {
                        let v = self.dup_out(v, vr)?;
                        self.some(v, vr)?
                    }
                    None => NONE,
                };
                self.put(res, out, true)
            }
            Builtin::DictRemove => {
                let k = second()?;
                let (found, words) = dict::remove(&mut self.store, heap, d, k)?;
                m.words(words);
                let out = match found {
                    Some(v) => self.some(v, vr)?,
                    None => NONE,
                };
                self.put(res, out, true)
            }
            Builtin::DictContains => {
                let (found, words) = dict::contains(&self.store, heap, d, second()?)?;
                m.words(words);
                self.wt(res, u64::from(found))
            }
            Builtin::DictKeys | Builtin::DictValues => {
                let (l, words) = if b == Builtin::DictKeys {
                    dict::keys(&mut self.store, heap, d)?
                } else {
                    dict::values(&mut self.store, heap, d)?
                };
                m.words(words);
                self.put(res, l, true)
            }
            Builtin::DictLen => {
                let n = dict::len(&self.store, heap, d)?;
                self.wt(res, u64::from(n))
            }
            Builtin::DictNext => {
                let pos = second()? as i64;
                let (serial, words) = dict::next(&self.store, heap, d, pos)?;
                m.words(words);
                self.wt(res, serial as u64)
            }
            Builtin::DictKeyAt | Builtin::DictValAt => {
                let serial = second()? as i64;
                let ((v, words), r) = if b == Builtin::DictKeyAt {
                    (dict::key_at(&self.store, heap, d, serial)?, kr)
                } else {
                    (dict::val_at(&self.store, heap, d, serial)?, vr)
                };
                m.words(words);
                let v = self.dup_out(v, r)?;
                self.put(res, v, r)
            }
            _ => Err(Fault::corrupt("not a dict builtin")),
        }
    }

    fn str_builtin(&mut self, b: Builtin, args: u16, res: u16, m: &mut Meter) -> Exec<()> {
        let heap = self.heap;
        match b {
            Builtin::StrConcat => {
                let parts = [self.rd(args)?, self.rd(arg(args, 1)?)?];
                let (v, words) = string::concat(&mut self.store, heap, &parts)?;
                m.words(words);
                self.put(res, v, true)
            }
            Builtin::Fmt => {
                let v = self.rd(args)?;
                let ty = u16::try_from(self.rd(arg(args, 1)?)?)
                    .map_err(|_| Fault::corrupt("Fmt of a bad type id"))?;
                let p = Arc::clone(&self.program);
                let mut out = String::new();
                let words = fmt_value(&self.store, heap, &p.types, ty, v, true, &mut out)?;
                m.words(words);
                self.put_text(res, &out, m)
            }
            Builtin::StrSplit => {
                let s = self.text_in(args, m)?;
                let sep = self.text_in(arg(args, 1)?, m)?;
                let parts: Vec<&str> = if sep.is_empty() {
                    vec![&s]
                } else {
                    s.split(sep.as_str()).collect()
                };
                let mut l = list::new(&mut self.store, heap, true)?;
                for part in parts {
                    let v = string::new(&mut self.store, heap, part)?;
                    let (nl, words) = list::push(&mut self.store, heap, l, v)?;
                    l = nl;
                    m.words(words.saturating_add(words_of(part.len())));
                }
                self.put(res, l, true)
            }
            Builtin::StrStartsWith | Builtin::StrEndsWith | Builtin::StrContains => {
                let s = self.text_in(args, m)?;
                let t = self.text_in(arg(args, 1)?, m)?;
                let r = match b {
                    Builtin::StrStartsWith => s.starts_with(t.as_str()),
                    Builtin::StrEndsWith => s.ends_with(t.as_str()),
                    _ => s.contains(t.as_str()),
                };
                self.wt(res, u64::from(r))
            }
            Builtin::StrCmp => {
                let s = string::bytes_of(&self.store, heap, self.rd(args)?)?;
                let t = string::bytes_of(&self.store, heap, self.rd(arg(args, 1)?)?)?;
                m.words(words_of(s.len()).saturating_add(words_of(t.len())));
                let r: i64 = match s.cmp(&t) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                self.wt(res, r as u64)
            }
            Builtin::StrLen => {
                let s = self.text_in(args, m)?;
                let n = s.chars().count() as u64;
                self.wt(res, n)
            }
            Builtin::StrTrim => {
                let s = self.text_in(args, m)?;
                self.put_text(res, s.trim_matches(is_ascii_space), m)
            }
            Builtin::StrUpper => {
                let s = self.text_in(args, m)?;
                self.put_text(res, &s.to_ascii_uppercase(), m)
            }
            Builtin::StrLower => {
                let s = self.text_in(args, m)?;
                self.put_text(res, &s.to_ascii_lowercase(), m)
            }
            Builtin::ParseInt => {
                let s = self.text_in(args, m)?;
                let out = match parse_int(&s) {
                    Some(n) => self.some(n as u64, false)?,
                    None => NONE,
                };
                self.put(res, out, true)
            }
            _ => Err(Fault::corrupt("not a string builtin")),
        }
    }

    fn math_builtin(&mut self, b: Builtin, args: u16, res: u16) -> Exec<()> {
        use Builtin::*;
        let x = self.rd(args)?;
        let binary = matches!(b, MinI | MaxI | MinF | MaxF | Pow | ModF);
        let y = if binary { self.rd(arg(args, 1)?)? } else { 0 };
        let (xi, yi, xf, yf) = (x as i64, y as i64, float(x), float(y));
        let v = match b {
            AbsI => xi.checked_abs().ok_or(FaultKind::IntOverflow)? as u64,
            MinI => xi.min(yi) as u64,
            MaxI => xi.max(yi) as u64,
            AbsF => canonical_bits(libm::fabs(xf)),
            MinF => min_f(xf, yf),
            MaxF => max_f(xf, yf),
            Sqrt => canonical_bits(libm::sqrt(xf)),
            Floor => canonical_bits(libm::floor(xf)),
            Ceil => canonical_bits(libm::ceil(xf)),
            Round => canonical_bits(libm::round(xf)),
            Sin => canonical_bits(libm::sin(xf)),
            Cos => canonical_bits(libm::cos(xf)),
            Pow => canonical_bits(libm::pow(xf, yf)),
            ModF => canonical_bits(libm::fmod(xf, yf)),
            _ => return Err(Fault::corrupt("not a math builtin")),
        };
        self.wt(res, v)
    }

    fn assert_builtin(&mut self, args: u16) -> Exec<()> {
        if self.rd(args)? != 0 {
            return Ok(());
        }
        let msg = self.rd(arg(args, 1)?)?;
        let message = if is_ptr(msg) {
            string::read(&self.store, self.heap, msg)?
        } else {
            FaultKind::AssertFailed.describe().to_string()
        };
        Err(Fault {
            kind: FaultKind::AssertFailed,
            message,
        })
    }
}
