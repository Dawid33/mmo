//! Fixture writer: builds big-endian Java NBT and gzips it.
#![allow(dead_code)]
use mc_import::{BlockState, BlockVolume};
use std::collections::BTreeMap;
use std::io::Write;

pub enum V {
    Byte(i8),
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
        V::Byte(_) => 1,
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
        V::Byte(x) => out.push(*x as u8),
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

/// A volume from a size and a closure naming each cell's state, building the palette as it goes.
pub fn vol(size: [u32; 3], mut f: impl FnMut(u32, u32, u32) -> String) -> BlockVolume {
    let mut palette: Vec<BlockState> = Vec::new();
    let mut seen: BTreeMap<String, u16> = BTreeMap::new();
    let mut blocks = Vec::new();
    for y in 0..size[1] {
        for z in 0..size[2] {
            for x in 0..size[0] {
                let s = f(x, y, z);
                let id = *seen.entry(s.clone()).or_insert_with(|| {
                    palette.push(BlockState::parse(&s).unwrap());
                    (palette.len() - 1) as u16
                });
                blocks.push(id);
            }
        }
    }
    BlockVolume {
        size,
        palette,
        blocks,
        block_entities: vec![],
        data_version: 3463,
        name: String::new(),
        author: String::new(),
    }
}
