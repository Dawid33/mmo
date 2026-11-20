//! A minimal, safe reader for Java-edition (big-endian) NBT.

use crate::ImportError;
use std::collections::BTreeMap;
use std::io::Read;

/// Deepest list/compound nesting accepted.
pub const NBT_MAX_DEPTH: usize = 64;
/// Most tag payloads (list elements and compound entries alike) one parse may produce; a tiny gzip can
/// otherwise expand into gigabytes of `Tag`s.
pub const NBT_MAX_TAGS: usize = 1 << 22;

#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<Tag>),
    Compound(BTreeMap<String, Tag>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    /// A compound's child by name (`None` on any other tag).
    pub fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(m) => m.get(key),
            _ => None,
        }
    }
    /// Byte, short or int, widened.
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Tag::Byte(v) => Some(*v as i32),
            Tag::Short(v) => Some(*v as i32),
            Tag::Int(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_compound(&self) -> Option<&BTreeMap<String, Tag>> {
        match self {
            Tag::Compound(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Tag]> {
        match self {
            Tag::List(l) => Some(l),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Tag::ByteArray(b) => Some(b),
            _ => None,
        }
    }
    pub fn as_ints(&self) -> Option<&[i32]> {
        match self {
            Tag::IntArray(a) => Some(a),
            _ => None,
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    tags: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ImportError> {
        if n > self.remaining() {
            return Err(ImportError::Truncated);
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ImportError> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, ImportError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ImportError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, ImportError> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    /// A non-negative length; the caller still checks it against `remaining()`.
    fn len(&mut self) -> Result<usize, ImportError> {
        let n = self.i32()?;
        usize::try_from(n).map_err(|_| ImportError::BadNbt(format!("negative length {n}")))
    }

    /// A length of `n` elements of `elem` bytes each, checked against the bytes left.
    fn checked_len(&mut self, elem: usize) -> Result<usize, ImportError> {
        let n = self.len()?;
        match n.checked_mul(elem) {
            Some(bytes) if bytes <= self.remaining() => Ok(n),
            _ => Err(ImportError::Truncated),
        }
    }

    fn string(&mut self) -> Result<String, ImportError> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    fn payload(&mut self, id: u8, depth: usize) -> Result<Tag, ImportError> {
        if depth > NBT_MAX_DEPTH {
            return Err(ImportError::BadNbt("nested too deeply".into()));
        }
        self.tags += 1;
        if self.tags > NBT_MAX_TAGS {
            return Err(ImportError::TooLarge);
        }
        Ok(match id {
            1 => Tag::Byte(self.u8()? as i8),
            2 => Tag::Short(i16::from_be_bytes(self.array()?)),
            3 => Tag::Int(self.i32()?),
            4 => Tag::Long(i64::from_be_bytes(self.array()?)),
            5 => Tag::Float(f32::from_be_bytes(self.array()?)),
            6 => Tag::Double(f64::from_be_bytes(self.array()?)),
            7 => {
                let n = self.checked_len(1)?;
                Tag::ByteArray(self.take(n)?.to_vec())
            }
            8 => Tag::String(self.string()?),
            9 => {
                let elem = self.u8()?;
                if elem > 12 {
                    return Err(ImportError::BadNbt(format!("unknown tag id {elem}")));
                }
                let n = self.len()?;
                if elem == 0 {
                    if n != 0 {
                        return Err(ImportError::BadNbt("list of End tags".into()));
                    }
                    return Ok(Tag::List(Vec::new()));
                }
                // Every element takes at least one byte.
                if n > self.remaining() {
                    return Err(ImportError::Truncated);
                }
                let mut items = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    items.push(self.payload(elem, depth + 1)?);
                }
                Tag::List(items)
            }
            10 => {
                let mut m = BTreeMap::new();
                loop {
                    let id = self.u8()?;
                    if id == 0 {
                        break;
                    }
                    let name = self.string()?;
                    let tag = self.payload(id, depth + 1)?;
                    m.insert(name, tag);
                }
                Tag::Compound(m)
            }
            11 => {
                let n = self.checked_len(4)?;
                let raw = self.take(n * 4)?;
                Tag::IntArray(
                    raw.chunks_exact(4)
                        .map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]))
                        .collect(),
                )
            }
            12 => {
                let n = self.checked_len(8)?;
                let raw = self.take(n * 8)?;
                Tag::LongArray(
                    raw.chunks_exact(8)
                        .map(|c| {
                            i64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])
                        })
                        .collect(),
                )
            }
            other => return Err(ImportError::BadNbt(format!("unknown tag id {other}"))),
        })
    }
}

/// Parses one uncompressed, big-endian (Java) named root tag. Every length is checked against the bytes
/// remaining before anything is allocated.
pub fn parse_nbt(bytes: &[u8]) -> Result<(String, Tag), ImportError> {
    let mut r = Reader {
        bytes,
        pos: 0,
        tags: 0,
    };
    let id = r.u8()?;
    if id == 0 {
        return Err(ImportError::BadNbt("root tag is End".into()));
    }
    let name = r.string()?;
    let tag = r.payload(id, 1)?;
    Ok((name, tag))
}

/// Gunzips (refusing more than `max_bytes` of output) then parses.
pub fn parse_gzip_nbt(bytes: &[u8], max_bytes: usize) -> Result<(String, Tag), ImportError> {
    if bytes.len() < 2 || bytes[0] != 0x1f || bytes[1] != 0x8b {
        return Err(ImportError::NotGzip);
    }
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut out = Vec::new();
    decoder
        .take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                ImportError::Truncated
            } else {
                ImportError::Io(e.to_string())
            }
        })?;
    if out.len() > max_bytes {
        return Err(ImportError::TooLarge);
    }
    parse_nbt(&out)
}
