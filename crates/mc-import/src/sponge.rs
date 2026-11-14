//! Sponge schematic reader (v2 and v3).

use crate::nbt::parse_gzip_nbt;
use crate::volume::{BlockEntity, BlockState, BlockVolume};
use crate::{ImportError, Tag};

/// Decompressed size cap.
pub const SCHEM_MAX_BYTES: usize = 1 << 30;
/// Cell count cap (Width * Height * Length).
pub const SCHEM_MAX_CELLS: u64 = 1 << 28;

const MAX_PALETTE: usize = 1 << 16;

fn bad(m: impl Into<String>) -> ImportError {
    ImportError::NotASchematic(m.into())
}

fn dim(t: &Tag, name: &str) -> Result<u32, ImportError> {
    match t.get(name) {
        Some(Tag::Short(v)) => Ok(*v as u16 as u32),
        Some(Tag::Int(v)) if (0..=0xffff).contains(v) => Ok(*v as u32),
        _ => Err(bad(format!("missing or bad {name}"))),
    }
}

/// Sponge schematic v2 (root `Schematic` with `Palette`, `BlockData`) and v3 (root "" holding `Schematic`
/// with `Blocks { Palette, Data, BlockEntities }`). v1 and anything else → Unsupported.
pub fn read_schem(bytes: &[u8]) -> Result<BlockVolume, ImportError> {
    let (_, root) = parse_gzip_nbt(bytes, SCHEM_MAX_BYTES)?;
    // v3 wraps the schematic in a `Schematic` compound; v2 is the root itself.
    let s = match root.get("Schematic") {
        Some(inner @ Tag::Compound(_)) => inner,
        _ => &root,
    };
    let version = s.get("Version").and_then(Tag::as_i32).ok_or_else(|| bad("no Version"))?;
    let (palette_tag, data_tag, entities_tag) = match version {
        2 => (s.get("Palette"), s.get("BlockData"), s.get("BlockEntities")),
        3 => {
            let b = s.get("Blocks").ok_or_else(|| bad("no Blocks"))?;
            (b.get("Palette"), b.get("Data"), b.get("BlockEntities"))
        }
        v => return Err(ImportError::Unsupported(format!("schematic version {v}"))),
    };

    let size = [dim(s, "Width")?, dim(s, "Height")?, dim(s, "Length")?];
    let cells = size.iter().map(|&d| d as u64).product::<u64>();
    if cells == 0 {
        return Err(bad("empty volume"));
    }
    if cells > SCHEM_MAX_CELLS {
        return Err(ImportError::TooLarge);
    }

    let palette = read_palette(palette_tag.ok_or_else(|| bad("no Palette"))?)?;
    let data = data_tag.and_then(Tag::as_bytes).ok_or_else(|| bad("no block data"))?;
    let blocks = decode_cells(data, cells as usize, palette.len())?;
    let block_entities = read_entities(entities_tag, version)?;

    let meta = s.get("Metadata");
    let text = |k: &str| meta.and_then(|m| m.get(k)).and_then(Tag::as_str).unwrap_or("").to_string();
    Ok(BlockVolume {
        size,
        palette,
        blocks,
        block_entities,
        data_version: s.get("DataVersion").and_then(Tag::as_i32).unwrap_or(0),
        name: text("Name"),
        author: text("Author"),
    })
}

fn read_palette(t: &Tag) -> Result<Vec<BlockState>, ImportError> {
    let m = t.as_compound().ok_or_else(|| bad("Palette is not a compound"))?;
    if m.len() > MAX_PALETTE {
        return Err(ImportError::Unsupported(format!("palette of {} states", m.len())));
    }
    if m.is_empty() {
        return Err(bad("empty palette"));
    }
    let mut slots: Vec<Option<BlockState>> = vec![None; m.len()];
    for (state, idx) in m {
        let i = idx.as_i32().ok_or_else(|| bad("palette index is not an int"))?;
        let slot = usize::try_from(i).ok().and_then(|i| slots.get_mut(i)).ok_or_else(|| bad("palette indices are not 0..n"))?;
        if slot.is_some() {
            return Err(bad("palette indices are not 0..n (one is used twice)"));
        }
        *slot = Some(BlockState::parse(state)?);
    }
    // Deliberately strict: indices must be exactly 0..n, each used once (every real file is).
    // `m.len()` distinct in-range indices fill every slot.
    Ok(slots.into_iter().flatten().collect())
}

/// Decodes exactly `cells` varints straight into the result.
fn decode_cells(data: &[u8], cells: usize, palette_len: usize) -> Result<Vec<u16>, ImportError> {
    // Every varint is at least one byte: bound the reservation by real input.
    if data.len() < cells {
        return Err(bad("block data is short"));
    }
    let mut out = Vec::with_capacity(cells);
    let mut i = 0;
    for _ in 0..cells {
        let mut v: u32 = 0;
        let mut shift = 0;
        loop {
            let b = *data.get(i).ok_or_else(|| bad("block data is short"))?;
            i += 1;
            if shift == 28 && b > 0x0f {
                return Err(bad("varint too long"));
            }
            v |= ((b & 0x7f) as u32) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        if v as usize >= palette_len {
            return Err(bad("cell index outside the palette"));
        }
        out.push(v as u16);
    }
    if i != data.len() {
        return Err(bad("block data is long"));
    }
    Ok(out)
}

/// Sponge v3 wraps a block entity's own NBT in `Data` next to `Id` and `Pos`; v2 is flat.
fn read_entities(t: Option<&Tag>, version: i32) -> Result<Vec<BlockEntity>, ImportError> {
    let Some(t) = t else { return Ok(Vec::new()) };
    let list = t.as_list().ok_or_else(|| bad("BlockEntities is not a list"))?;
    list.iter()
        .map(|e| {
            let pos = e.get("Pos").and_then(Tag::as_ints).filter(|p| p.len() == 3).ok_or_else(|| bad("block entity without Pos"))?;
            let id = e.get("Id").and_then(Tag::as_str).ok_or_else(|| bad("block entity without Id"))?;
            let nbt = match e.get("Data") {
                Some(d @ Tag::Compound(_)) if version == 3 => d.clone(),
                _ => e.clone(),
            };
            Ok(BlockEntity { pos: [pos[0], pos[1], pos[2]], id: id.to_string(), nbt })
        })
        .collect()
}
