//! Value formatting for f-strings and reports (design D8).
//!
//! `42`, `1.5`, `2.0`, `NaN`, `inf`, `-inf`, `true`, `()`; a string raw at the top level and quoted when
//! nested (`"` `\` and newlines, tabs and carriage returns escaped); `[1, 2]`; `{"a": 1}`;
//! `Ledger(owner="ana", entries=[])`; `Stage::Accepted(tick=5)`; `Stage::Asked`; `Option` as `Some(3)` /
//! `None`; an error (a variant of the program's error enum, [`ERROR_ENUM_NAME`]) by its name alone: `NotEnough(have=3)`,
//! `ChestEmpty`. Floats print as Rust's `{:?}` (shortest round-trip).
//!
//! The heap value is read through [`to_wire`] (same walk budget, same reported work) and the result is
//! written by type, so the two cannot disagree on what a value contains.

use crate::alloc::Heap;
use crate::fault::{FaultKind, VmResult};
use crate::store::Store;
use crate::types::{ERROR_ENUM_NAME, TypeDesc, TypeId, TypeTable};
use crate::wire::{WireValue, to_wire};
use std::fmt::Write;

/// Appends the text of the value `v` of type `t` to `out`; `top_level` is whether a string prints raw.
/// Returns the words the walk visited. On an error `out` may hold a prefix.
pub fn fmt_value(
    st: &Store,
    heap: Heap,
    types: &TypeTable,
    t: TypeId,
    v: u64,
    top_level: bool,
    out: &mut String,
) -> VmResult<u32> {
    let (w, work) = to_wire(st, heap, types, t, v)?;
    write_value(types, t, &w, top_level, out)?;
    Ok(work)
}

fn quoted(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// What is still to be written, last first: a value, or text.
enum Item<'a> {
    Value(TypeId, &'a WireValue, bool),
    Text(&'a str),
}

/// `(f=v, g=w)` for the declared `types` of the wire `fields`, matched by position (`to_wire` writes them
/// in declaration order): writes the `(` and pushes the rest.
fn write_fields<'a>(
    declared: &'a [(String, TypeId)],
    fields: &'a [(String, WireValue)],
    out: &mut String,
    stack: &mut Vec<Item<'a>>,
) -> VmResult<()> {
    if declared.len() != fields.len() {
        return Err(FaultKind::Corrupt);
    }
    out.push('(');
    stack.push(Item::Text(")"));
    for (i, ((name, ft), (_, w))) in declared.iter().zip(fields).enumerate().rev() {
        stack.push(Item::Value(*ft, w, false));
        stack.push(Item::Text("="));
        stack.push(Item::Text(name));
        if i > 0 {
            stack.push(Item::Text(", "));
        }
    }
    Ok(())
}

/// Writes in the order a recursive walk would (a value can nest arbitrarily deep, decision 17): each
/// value's own text at once, its parts pushed onto the stack last first.
fn write_value(
    types: &TypeTable,
    t: TypeId,
    w: &WireValue,
    top_level: bool,
    out: &mut String,
) -> VmResult<()> {
    let mut stack = vec![Item::Value(t, w, top_level)];
    while let Some(item) = stack.pop() {
        let (t, w, top_level) = match item {
            Item::Text(s) => {
                out.push_str(s);
                continue;
            }
            Item::Value(t, w, top_level) => (t, w, top_level),
        };
        match (types.desc(t)?, w) {
            (TypeDesc::Unit, WireValue::Unit) => out.push_str("()"),
            (TypeDesc::Bool, WireValue::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
            (TypeDesc::Int, WireValue::Int(i)) => {
                let _ = write!(out, "{i}");
            }
            (TypeDesc::Float, WireValue::Float(f)) => {
                let _ = write!(out, "{f:?}");
            }
            (TypeDesc::Str, WireValue::Str(s)) => {
                if top_level {
                    out.push_str(s);
                } else {
                    quoted(s, out);
                }
            }
            (TypeDesc::List(e), WireValue::List(items)) => {
                out.push('[');
                stack.push(Item::Text("]"));
                for (i, item) in items.iter().enumerate().rev() {
                    stack.push(Item::Value(*e, item, false));
                    if i > 0 {
                        stack.push(Item::Text(", "));
                    }
                }
            }
            (TypeDesc::Dict(kt, vt), WireValue::Dict(pairs)) => {
                out.push('{');
                stack.push(Item::Text("}"));
                for (i, (k, v)) in pairs.iter().enumerate().rev() {
                    stack.push(Item::Value(*vt, v, false));
                    stack.push(Item::Text(": "));
                    stack.push(Item::Value(*kt, k, false));
                    if i > 0 {
                        stack.push(Item::Text(", "));
                    }
                }
            }
            (
                TypeDesc::Struct {
                    name,
                    fields: declared,
                },
                WireValue::Struct { fields, .. },
            ) => {
                out.push_str(name);
                write_fields(declared, fields, out, &mut stack)?;
            }
            (
                TypeDesc::Enum { name, variants },
                WireValue::Variant {
                    name: vname,
                    fields,
                },
            ) => {
                let (_, declared) = variants
                    .iter()
                    .find(|(n, _)| n == vname)
                    .ok_or(FaultKind::Corrupt)?;
                let is_option = name == "Option"
                    && variants.len() == 2
                    && variants[0].0 == "None"
                    && variants[1].0 == "Some";
                // The program's errors (codegen carry-over 9) print by their name alone.
                if is_option || name == ERROR_ENUM_NAME {
                    out.push_str(vname);
                } else {
                    out.push_str(name);
                    out.push_str("::");
                    out.push_str(vname);
                }
                if is_option && vname == "Some" && declared.len() == 1 && fields.len() == 1 {
                    out.push('(');
                    stack.push(Item::Text(")"));
                    stack.push(Item::Value(declared[0].1, &fields[0].1, false));
                } else if !declared.is_empty() || !fields.is_empty() {
                    write_fields(declared, fields, out, &mut stack)?;
                }
            }
            _ => return Err(FaultKind::Corrupt),
        }
    }
    Ok(())
}
