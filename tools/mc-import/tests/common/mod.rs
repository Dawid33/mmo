//! Copied from crates/mc-import/tests/common/mod.rs (two crates' tests cannot share a module): the
//! NBT + gzip fixture writer, without the volume helper.

use std::io::Write;

pub enum V {
    Short(i16),
    Int(i32),
    Str(String),
    Bytes(Vec<u8>),
    Ints(Vec<i32>),
    List(u8, Vec<V>),
    Compound(Vec<(String, V)>),
}

pub fn c(fields: Vec<(&str, V)>) -> V {
    V::Compound(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

pub fn s(x: &str) -> V {
    V::Str(x.to_string())
}

fn id(v: &V) -> u8 {
    match v {
        V::Short(_) => 2,
        V::Int(_) => 3,
        V::Bytes(_) => 7,
        V::Str(_) => 8,
        V::List(..) => 9,
        V::Compound(_) => 10,
        V::Ints(_) => 11,
    }
}

fn put_str(out: &mut Vec<u8>, x: &str) {
    out.extend((x.len() as u16).to_be_bytes());
    out.extend(x.as_bytes());
}

fn payload(out: &mut Vec<u8>, v: &V) {
    match v {
        V::Short(x) => out.extend(x.to_be_bytes()),
        V::Int(x) => out.extend(x.to_be_bytes()),
        V::Str(x) => put_str(out, x),
        V::Bytes(b) => {
            out.extend((b.len() as i32).to_be_bytes());
            out.extend(b);
        }
        V::Ints(a) => {
            out.extend((a.len() as i32).to_be_bytes());
            for x in a {
                out.extend(x.to_be_bytes());
            }
        }
        V::List(elem, items) => {
            out.push(*elem);
            out.extend((items.len() as i32).to_be_bytes());
            for i in items {
                payload(out, i);
            }
        }
        V::Compound(fields) => {
            for (k, f) in fields {
                out.push(id(f));
                put_str(out, k);
                payload(out, f);
            }
            out.push(0);
        }
    }
}

/// An uncompressed named root tag.
pub fn nbt(name: &str, v: &V) -> Vec<u8> {
    let mut out = vec![id(v)];
    put_str(&mut out, name);
    payload(&mut out, v);
    out
}

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

pub fn varints(cells: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for &c in cells {
        let mut c = c;
        loop {
            let b = (c & 0x7f) as u8;
            c >>= 7;
            if c == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
    }
    out
}

