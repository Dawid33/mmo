//! Splitting a schematic of several spaced-out builds (a showroom) into one box per build.

use crate::classify::{Class, classify};
use crate::convert::sign_text;
use crate::{BlockVolume, Table};
use std::collections::VecDeque;

/// A sign this close (Chebyshev, x/z) to a build's box labels it.
const SIGN_REACH: i64 = 6;
/// A ground layer is at least this many percent solid.
const GROUND_PERCENT: u64 = 60;
const NONE: u32 = u32::MAX;

#[derive(Debug, Clone)]
pub struct SplitOptions {
    /// Builds closer than this many empty cells (in x/z) are one build; default 3.
    /// The footprint is dilated by `gap / 2` and 8-connected, so odd values are exact; an even
    /// value joins builds one cell further apart than the number says.
    pub gap: u32,
    /// Smaller components are dropped; default 30.
    pub min_blocks: u64,
    pub ground: GroundMode,
}

impl Default for SplitOptions {
    fn default() -> Self {
        SplitOptions {
            gap: 3,
            min_blocks: 30,
            ground: GroundMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundMode {
    Auto,
    None,
    /// The lowest n layers of the volume are ground (clamped to the height).
    Layers(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub min: [u32; 3],
    /// Exclusive.
    pub max: [u32; 3],
    pub blocks: u64,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub ground_layers: u32,
    pub builds: Vec<Build>,
    pub dropped_small: usize,
}

/// Per palette entry: (counts as solid for a build, counts as solid for the ground rule).
fn solidity(volume: &BlockVolume, table: &Table) -> Vec<(bool, bool)> {
    volume
        .palette
        .iter()
        .map(|s| match classify(s, table).class {
            Class::Block { .. } | Class::Shaped { .. } => (true, true),
            Class::Marker { solid: Some(_), .. } => (true, false),
            _ => (false, false),
        })
        .collect()
}

fn ground_layers(volume: &BlockVolume, sol: &[(bool, bool)], mode: GroundMode) -> u32 {
    let [sx, sy, sz] = volume.size;
    match mode {
        GroundMode::None => 0,
        GroundMode::Layers(n) => n.min(sy),
        GroundMode::Auto => {
            let total = sx as u64 * sz as u64;
            let layer = sx as usize * sz as usize;
            let mut g = 0;
            while g < sy {
                let cells = &volume.blocks[g as usize * layer..(g as usize + 1) * layer];
                let n = cells
                    .iter()
                    .filter(|&&p| sol.get(p as usize).is_some_and(|s| s.1))
                    .count() as u64;
                if total == 0 || n * 100 < GROUND_PERCENT * total {
                    break;
                }
                g += 1;
            }
            g
        }
    }
}

/// `out[i]` = any occupied cell within `r` of `i` along one line of `n` cells, `stride` apart.
fn dilate_lines(
    grid: &mut [bool],
    lines: usize,
    n: usize,
    line_stride: usize,
    stride: usize,
    r: usize,
) {
    let mut prefix = vec![0u32; n + 1];
    for l in 0..lines {
        let base = l * line_stride;
        for i in 0..n {
            prefix[i + 1] = prefix[i] + grid[base + i * stride] as u32;
        }
        for i in 0..n {
            let (lo, hi) = (i.saturating_sub(r), (i + r + 1).min(n));
            grid[base + i * stride] = prefix[hi] > prefix[lo];
        }
    }
}

pub fn split(volume: &BlockVolume, table: &Table, opts: &SplitOptions) -> Split {
    let [sx, sy, sz] = volume.size;
    let (sxu, szu) = (sx as usize, sz as usize);
    let sol = solidity(volume, table);
    let g = ground_layers(volume, &sol, opts.ground);
    let mut out = Split {
        ground_layers: g,
        builds: Vec::new(),
        dropped_small: 0,
    };
    if sxu == 0 || szu == 0 || g >= sy {
        return out;
    }

    // One pass over the cells above the ground: per column lowest/highest solid y and the count.
    let cols = sxu * szu;
    let mut lo = vec![u32::MAX; cols];
    let mut hi = vec![0u32; cols];
    let mut cnt = vec![0u32; cols];
    for y in g..sy {
        let layer = &volume.blocks[y as usize * cols..(y as usize + 1) * cols];
        for (c, &p) in layer.iter().enumerate() {
            if sol.get(p as usize).is_some_and(|s| s.0) {
                lo[c] = lo[c].min(y);
                hi[c] = y;
                cnt[c] += 1;
            }
        }
    }

    // Dilate the footprint (two separable passes), label 8-connected components with a queue.
    let r = (opts.gap / 2) as usize;
    let mut dil: Vec<bool> = cnt.iter().map(|&n| n > 0).collect();
    dilate_lines(&mut dil, szu, sxu, sxu, 1, r);
    dilate_lines(&mut dil, sxu, szu, 1, sxu, r);
    let mut label = vec![NONE; cols];
    let mut next = 0u32;
    let mut queue = VecDeque::new();
    for start in 0..cols {
        if !dil[start] || label[start] != NONE {
            continue;
        }
        label[start] = next;
        queue.push_back(start);
        while let Some(c) = queue.pop_front() {
            let (cx, cz) = (c % sxu, c / sxu);
            for nz in cz.saturating_sub(1)..=(cz + 1).min(szu - 1) {
                for nx in cx.saturating_sub(1)..=(cx + 1).min(sxu - 1) {
                    let n = nz * sxu + nx;
                    if dil[n] && label[n] == NONE {
                        label[n] = next;
                        queue.push_back(n);
                    }
                }
            }
        }
        next += 1;
    }

    // Tight boxes from the undilated columns. Scanning z then x keeps the discovery order
    // irrelevant: builds are sorted by (min.z, min.x) afterwards.
    let mut builds: Vec<Build> = (0..next)
        .map(|_| Build {
            min: [u32::MAX; 3],
            max: [0; 3],
            blocks: 0,
            label: None,
        })
        .collect();
    for c in 0..cols {
        if cnt[c] == 0 {
            continue;
        }
        let b = &mut builds[label[c] as usize];
        let (x, z) = ((c % sxu) as u32, (c / sxu) as u32);
        b.min = [b.min[0].min(x), b.min[1].min(lo[c]), b.min[2].min(z)];
        b.max = [
            b.max[0].max(x + 1),
            b.max[1].max(hi[c] + 1),
            b.max[2].max(z + 1),
        ];
        b.blocks += cnt[c] as u64;
    }
    let before = builds.len();
    builds.retain(|b| b.blocks >= opts.min_blocks);
    out.dropped_small = before - builds.len();
    builds.sort_by_key(|b| (b.min[2], b.min[0]));

    label_builds(volume, &mut builds);
    out.builds = builds;
    out
}

/// A label's slug must hold at least this many letters or digits.
const MIN_LABEL_CHARS: usize = 3;
/// A slug is cut to this many characters.
const MAX_SLUG: usize = 60;

/// Gives each build the text of the nearest sign within `SIGN_REACH`. A sign whose column lies inside any
/// build's box is decoration and is ignored, as is text whose slug has fewer than `MIN_LABEL_CHARS`
/// letters or digits.
fn label_builds(volume: &BlockVolume, builds: &mut [Build]) {
    // Per build: (distance, (z, x, y), text) of the best sign so far.
    let mut best: Vec<Option<(i64, (i32, i32, i32), String)>> = vec![None; builds.len()];
    for e in &volume.block_entities {
        if e.id != "minecraft:sign" && e.id != "minecraft:hanging_sign" {
            continue;
        }
        let Some(text) = sign_text(&e.nbt, volume.data_version) else {
            continue;
        };
        let text = text.trim().to_string();
        if slug(&text)
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .count()
            < MIN_LABEL_CHARS
        {
            continue;
        }
        let (x, y, z) = (e.pos[0] as i64, e.pos[1], e.pos[2] as i64);
        let inside = builds.iter().any(|b| {
            (b.min[0] as i64..b.max[0] as i64).contains(&x)
                && (b.min[2] as i64..b.max[2] as i64).contains(&z)
        });
        if inside {
            continue;
        }
        let mut nearest: Option<(i64, usize)> = None;
        for (i, b) in builds.iter().enumerate() {
            let dx = (b.min[0] as i64 - x).max(x - (b.max[0] as i64 - 1)).max(0);
            let dz = (b.min[2] as i64 - z).max(z - (b.max[2] as i64 - 1)).max(0);
            let d = dx.max(dz);
            if d <= SIGN_REACH && nearest.map_or(true, |(nd, _)| d < nd) {
                nearest = Some((d, i));
            }
        }
        let Some((d, i)) = nearest else { continue };
        let key = (e.pos[2], e.pos[0], y);
        if best[i]
            .as_ref()
            .map_or(true, |(bd, bk, _)| (d, key) < (*bd, *bk))
        {
            best[i] = Some((d, key, text));
        }
    }
    for (b, s) in builds.iter_mut().zip(best) {
        b.label = s.map(|(_, _, t)| t);
    }
}

/// A file-name-safe name: lowercase ASCII letters, digits and `-`, at most 60 characters; "" when nothing
/// is left.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(MAX_SLUG);
    out.trim_matches('-').to_string()
}
