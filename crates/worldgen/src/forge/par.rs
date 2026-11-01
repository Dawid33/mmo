//! Data parallelism for the forge's erosion step: rayon natively, plain loops
//! on wasm32 (which has no threads here). Every helper splits work into
//! pieces whose results do not depend on how the pieces are scheduled — rows
//! of a grid, or contiguous ranges of independent drainage trees — and
//! collects ordered results in piece order, so the output is bit-identical
//! for any thread count.

use std::ops::Range;

/// Grid rows per parallel work item: enough cells per item to pay for the
/// scheduling (~4k cells on a 500-cell row grid), small enough to balance.
pub const ROWS_PER_TASK: usize = 8;

/// Cells per parallel work item when work is split by drainage trees: trees
/// are grouped, in order, until a group holds at least this many cells.
pub const TREE_GROUP_CELLS: usize = 4096;

/// Calls `f(first_row, rows, sink)` on `data` split into runs of
/// `ROWS_PER_TASK` rows of `row_len`, in parallel natively. Whatever the
/// calls push to their `sink` replaces `out`, in row order.
pub fn for_rows<T: Send>(data: &mut [T], row_len: usize, out: &mut Vec<u32>, f: impl Fn(usize, &mut [T], &mut Vec<u32>) + Sync + Send) {
    let chunk = row_len * ROWS_PER_TASK;
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        let parts: Vec<Vec<u32>> = data
            .par_chunks_mut(chunk)
            .enumerate()
            .map(|(k, rows)| {
                let mut sink = Vec::new();
                f(k * ROWS_PER_TASK, rows, &mut sink);
                sink
            })
            .collect();
        concat(parts, out);
    }
    #[cfg(target_arch = "wasm32")]
    {
        out.clear();
        for (k, rows) in data.chunks_mut(chunk).enumerate() {
            f(k * ROWS_PER_TASK, rows, out);
        }
    }
}

/// `for_rows` over three grids of the same shape at once.
pub fn for_rows3<A: Send, B: Send, C: Send>(
    a: &mut [A],
    b: &mut [B],
    c: &mut [C],
    row_len: usize,
    out: &mut Vec<u32>,
    f: impl Fn(usize, &mut [A], &mut [B], &mut [C], &mut Vec<u32>) + Sync + Send,
) {
    let chunk = row_len * ROWS_PER_TASK;
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        let parts: Vec<Vec<u32>> = a
            .par_chunks_mut(chunk)
            .zip(b.par_chunks_mut(chunk))
            .zip(c.par_chunks_mut(chunk))
            .enumerate()
            .map(|(k, ((ra, rb), rc))| {
                let mut sink = Vec::new();
                f(k * ROWS_PER_TASK, ra, rb, rc, &mut sink);
                sink
            })
            .collect();
        concat(parts, out);
    }
    #[cfg(target_arch = "wasm32")]
    {
        out.clear();
        for (k, ((ra, rb), rc)) in a.chunks_mut(chunk).zip(b.chunks_mut(chunk)).zip(c.chunks_mut(chunk)).enumerate() {
            f(k * ROWS_PER_TASK, ra, rb, rc, out);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn concat(parts: Vec<Vec<u32>>, out: &mut Vec<u32>) {
    out.clear();
    for part in parts {
        out.extend_from_slice(&part);
    }
}

/// `f(item)` for every item, results in item order.
pub fn map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync + Send) -> Vec<R> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        items.par_iter().map(f).collect()
    }
    #[cfg(target_arch = "wasm32")]
    items.iter().map(f).collect()
}

/// `f(range)` for each range, in parallel natively.
pub fn for_ranges(ranges: &[Range<usize>], f: impl Fn(Range<usize>) + Sync + Send) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        ranges.par_iter().for_each(|r| f(r.clone()));
    }
    #[cfg(target_arch = "wasm32")]
    for r in ranges {
        f(r.clone());
    }
}

/// `f(i)` for `i` in `0..len`, results in order.
pub fn map_range<R: Send>(len: usize, f: impl Fn(usize) -> R + Sync + Send) -> Vec<R> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        (0..len).into_par_iter().map(f).collect()
    }
    #[cfg(target_arch = "wasm32")]
    (0..len).map(f).collect()
}

/// A slice that several workers write at DISJOINT indices: each drainage tree
/// owns its cells, scattered over the grid, so the slice can't be split into
/// `&mut` pieces. Every `unsafe` accessor's contract is the disjointness: an
/// index written by one worker is neither read nor written by another during
/// the same parallel pass.
pub struct Disjoint<'a, T> {
    ptr: *mut T,
    len: usize,
    _slice: std::marker::PhantomData<&'a mut [T]>,
}

// SAFETY: access is only through the unsafe accessors, whose callers promise
// disjoint indices per worker. `set` moves a T in from another thread (Send);
// `get` may read indices concurrently from several workers (Sync), so a shared
// `Disjoint` needs both — the bound `&mut [T]: Sync` itself has.
unsafe impl<T: Send + Sync> Sync for Disjoint<'_, T> {}
unsafe impl<T: Send> Send for Disjoint<'_, T> {}

impl<'a, T: Copy> Disjoint<'a, T> {
    pub fn new(slice: &'a mut [T]) -> Self {
        Disjoint { ptr: slice.as_mut_ptr(), len: slice.len(), _slice: std::marker::PhantomData }
    }

    /// # Safety
    /// No other worker writes index `i` during this pass.
    #[inline]
    pub unsafe fn get(&self, i: usize) -> T {
        assert!(i < self.len);
        unsafe { *self.ptr.add(i) }
    }

    /// # Safety
    /// No other worker reads or writes index `i` during this pass.
    #[inline]
    pub unsafe fn set(&self, i: usize, v: T) {
        assert!(i < self.len);
        unsafe { *self.ptr.add(i) = v }
    }
}

/// Splits `order` (a concatenation of whole drainage trees, `tree_starts`
/// giving each tree's first position) into ranges of whole trees holding at
/// least `TREE_GROUP_CELLS` cells each (the last may hold fewer).
pub fn group_trees(tree_starts: &[u32], len: usize, out: &mut Vec<Range<usize>>) {
    out.clear();
    let mut start = 0;
    for &t in tree_starts.iter().skip(1) {
        let t = t as usize;
        if t - start >= TREE_GROUP_CELLS {
            out.push(start..t);
            start = t;
        }
    }
    if start < len {
        out.push(start..len);
    }
}
