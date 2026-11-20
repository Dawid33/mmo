//! The monthly climate's wire form: every month plane of the six fields
//! row-delta encoded in rows of the CLIMATE grid's `nx` (each value but a
//! row's first becomes its wrapping difference to its left neighbour). Small
//! steps between neighbouring cells become small numbers, which the packer
//! that follows (bincode, then zstd in `worldgen`'s world file and tiled
//! meta, lz4 in `encode_packet` on the wire) shrinks well.
//!
//! Pure byte shuffling, no floats: the same bytes on every target (wasm32
//! included). The world file's and the tiled meta's bytes are this form
//! (`worldgen::forged`, `forged_tiles`); changing it changes what they store.

use crate::MonthlyClimate;

/// Row-delta encodes a plane in rows of `n` (wrapping `sub`). A month plane
/// is `n · nz` values, so rows never straddle two months: every month is
/// encoded on its own. Walks `chunks_mut`, so a plane of any length is safe
/// (a wrong one is refused by `MonthlyClimate::validate`).
fn delta_plane<T: Copy>(plane: &mut [T], n: usize, sub: fn(T, T) -> T) {
    for row in plane.chunks_mut(n.max(1)) {
        for i in (1..row.len()).rev() {
            row[i] = sub(row[i], row[i - 1]);
        }
    }
}

/// Inverse of [`delta_plane`] (`add` the wrapping inverse of its `sub`).
fn undelta_plane<T: Copy>(plane: &mut [T], n: usize, add: fn(T, T) -> T) {
    for row in plane.chunks_mut(n.max(1)) {
        for i in 1..row.len() {
            row[i] = add(row[i], row[i - 1]);
        }
    }
}

impl MonthlyClimate {
    /// Row-delta encodes every month plane in place (rows of `nx`; each value
    /// but a row's first becomes its wrapping difference to its left
    /// neighbour). Never indexes past a plane (`chunks_mut(nx.max(1))`).
    pub fn delta_encode(&mut self) {
        let n = self.nx as usize;
        delta_plane(&mut self.temp_sea_centi_c, n, i16::wrapping_sub);
        delta_plane(&mut self.diurnal_deci_c, n, u8::wrapping_sub);
        delta_plane(&mut self.precip_deci_mm, n, u16::wrapping_sub);
        delta_plane(&mut self.wet_day_255, n, u8::wrapping_sub);
        delta_plane(&mut self.wind_east_q, n, i8::wrapping_sub);
        delta_plane(&mut self.wind_south_q, n, i8::wrapping_sub);
    }

    /// The inverse of [`MonthlyClimate::delta_encode`]. Call `validate` on
    /// untrusted input FIRST (a plane of the wrong size decodes without a
    /// panic, into values that mean nothing).
    pub fn delta_decode(&mut self) {
        let n = self.nx as usize;
        undelta_plane(&mut self.temp_sea_centi_c, n, i16::wrapping_add);
        undelta_plane(&mut self.diurnal_deci_c, n, u8::wrapping_add);
        undelta_plane(&mut self.precip_deci_mm, n, u16::wrapping_add);
        undelta_plane(&mut self.wet_day_255, n, u8::wrapping_add);
        undelta_plane(&mut self.wind_east_q, n, i8::wrapping_add);
        undelta_plane(&mut self.wind_south_q, n, i8::wrapping_add);
    }

    /// crc32 of the grid (`nx`, `nz`, the bits of `cell_m`), `wrap_x` and the
    /// six planes, every value little-endian and each plane preceded by its
    /// length (`u64`), so the same family gives the same value on every
    /// target.
    pub fn content_crc(&self) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(&self.nx.to_le_bytes());
        h.update(&self.nz.to_le_bytes());
        h.update(&self.cell_m.to_bits().to_le_bytes());
        h.update(&[u8::from(self.wrap_x)]);
        h.update(&(self.temp_sea_centi_c.len() as u64).to_le_bytes());
        for v in &self.temp_sea_centi_c {
            h.update(&v.to_le_bytes());
        }
        h.update(&(self.diurnal_deci_c.len() as u64).to_le_bytes());
        h.update(&self.diurnal_deci_c);
        h.update(&(self.precip_deci_mm.len() as u64).to_le_bytes());
        for v in &self.precip_deci_mm {
            h.update(&v.to_le_bytes());
        }
        h.update(&(self.wet_day_255.len() as u64).to_le_bytes());
        h.update(&self.wet_day_255);
        for plane in [&self.wind_east_q, &self.wind_south_q] {
            h.update(&(plane.len() as u64).to_le_bytes());
            for v in plane {
                h.update(&v.to_le_bytes());
            }
        }
        h.finalize()
    }
}
