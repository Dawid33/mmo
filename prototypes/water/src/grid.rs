//! Grid geometry and a bitset. Index order is y-major, `(y * z_len + z) * x_len + x`,
//! so ascending index visits the bottom layer first. Every "canonical order" in the
//! prototype is ascending index.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dims {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

impl Dims {
    pub const fn cube(n: u32) -> Self {
        Dims { x: n, y: n, z: n }
    }
    pub const fn len(self) -> usize {
        (self.x * self.y * self.z) as usize
    }
    pub const fn layer(self) -> u32 {
        self.x * self.z
    }
    pub fn idx(self, x: u32, y: u32, z: u32) -> u32 {
        debug_assert!(x < self.x && y < self.y && z < self.z);
        (y * self.z + z) * self.x + x
    }
    pub fn xyz(self, i: u32) -> (u32, u32, u32) {
        let x = i % self.x;
        let r = i / self.x;
        (x, r / self.z, r % self.z)
    }
    pub fn y_of(self, i: u32) -> u32 {
        i / self.layer()
    }
    /// Bricks along each axis (`BRICK` cells each; dims must be multiples).
    fn bricks(self) -> (u32, u32, u32) {
        let b = crate::BRICK;
        (self.x / b, self.y / b, self.z / b)
    }
    pub fn brick_count(self) -> u32 {
        let (bx, by, bz) = self.bricks();
        bx * by * bz
    }
    /// Index of the `BRICK`³ brick holding cell `i`: `(by * bz_n + bz) * bx_n + bx`.
    pub fn brick_of(self, i: u32) -> u32 {
        let b = crate::BRICK;
        let (x, y, z) = self.xyz(i);
        let (bxn, _, bzn) = self.bricks();
        ((y / b) * bzn + z / b) * bxn + x / b
    }
    /// Cells of brick `b` in brick-local order `(ly * B + lz) * B + lx`.
    pub fn brick_cells(self, b: u32) -> impl Iterator<Item = u32> {
        let n = crate::BRICK;
        let (bxn, _, bzn) = self.bricks();
        let (bx, r) = (b % bxn, b / bxn);
        let (bz, by) = (r % bzn, r / bzn);
        (0..n * n * n).map(move |k| {
            let (lx, r) = (k % n, k / n);
            self.idx(bx * n + lx, by * n + r / n, bz * n + r % n)
        })
    }
    /// Face neighbour, or `None` outside the box. The box is closed: callers
    /// treat `None` as solid.
    pub fn step(self, i: u32, d: Dir) -> Option<u32> {
        let (x, y, z) = self.xyz(i);
        let (x, y, z) = match d {
            Dir::Down => (x, y.checked_sub(1)?, z),
            Dir::Up => (x, (y + 1 < self.y).then_some(y + 1)?, z),
            Dir::PosX => ((x + 1 < self.x).then_some(x + 1)?, y, z),
            Dir::NegX => (x.checked_sub(1)?, y, z),
            Dir::PosZ => (x, y, (z + 1 < self.z).then_some(z + 1)?),
            Dir::NegZ => (x, y, z.checked_sub(1)?),
        };
        Some(self.idx(x, y, z))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Down,
    Up,
    PosX,
    NegX,
    PosZ,
    NegZ,
}

pub const HORIZONTAL: [Dir; 4] = [Dir::PosX, Dir::PosZ, Dir::NegX, Dir::NegZ];
pub const ALL: [Dir; 6] = [Dir::Down, Dir::PosX, Dir::PosZ, Dir::NegX, Dir::NegZ, Dir::Up];

/// Checkerboard colour of a cell: `(x + y + z) % 2`.
pub fn parity(dims: Dims, i: u32) -> u32 {
    let (x, y, z) = dims.xyz(i);
    (x + y + z) % 2
}

/// Horizontal visiting order, rotated by the step so no direction is favoured.
pub fn horizontal_order(step: u32) -> [Dir; 4] {
    let r = (step % 4) as usize;
    core::array::from_fn(|k| HORIZONTAL[(k + r) % 4])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bits {
    words: Vec<u64>,
}

impl Bits {
    pub fn new(len: usize) -> Self {
        Bits { words: vec![0; len.div_ceil(64)] }
    }
    pub fn get(&self, i: u32) -> bool {
        ((self.words[(i / 64) as usize] >> (i % 64)) & 1) == 1
    }
    pub fn set(&mut self, i: u32, v: bool) {
        let w = &mut self.words[(i / 64) as usize];
        let m = 1u64 << (i % 64);
        if v { *w |= m } else { *w &= !m }
    }
    pub fn words(&self) -> &[u64] {
        &self.words
    }
    /// Set bits, ascending.
    pub fn ones(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for (wi, &word) in self.words.iter().enumerate() {
            let mut word = word;
            while word != 0 {
                out.push(wi as u32 * 64 + word.trailing_zeros());
                word &= word - 1;
            }
        }
        out
    }
    pub fn count(&self) -> u64 {
        self.words.iter().map(|w| w.count_ones() as u64).sum()
    }
}
