//! The slice of `ilattice::Extent` this crate used, over glam's `UVec3`
//! (mmo-fork: replaces the `ilattice` dependency, which pinned glam 0.19).
//! Semantics match ilattice 0.1: `max` is inclusive, a shape never goes
//! negative, and `iter3` walks z outermost, x innermost.
use core::ops::Add;

use glam::{IVec3, UVec3};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Extent {
    pub minimum: UVec3,
    pub shape: UVec3,
}

impl Extent {
    #[inline]
    pub const fn from_min_and_shape(minimum: UVec3, shape: UVec3) -> Self {
        Self { minimum, shape }
    }

    /// `max` is inclusive; an empty range gives a zero shape.
    #[inline]
    pub fn from_min_and_max(minimum: UVec3, max: UVec3) -> Self {
        Self::from_min_and_lub(minimum, max + UVec3::ONE)
    }

    #[inline]
    fn from_min_and_lub(minimum: UVec3, lub: UVec3) -> Self {
        Self {
            minimum,
            shape: lub.saturating_sub(minimum),
        }
    }

    /// `min..=max` shrunk by one voxel on every side (room for a 3×3×3
    /// kernel), computed in signed space and cast back, as ilattice's
    /// `padded(-1)` + `as_uvec3` did.
    #[inline]
    pub fn interior(min: [u32; 3], max: [u32; 3]) -> Self {
        let min = UVec3::from(min).as_ivec3();
        let max = UVec3::from(max).as_ivec3();
        let shape = (max + IVec3::ONE - min).max(IVec3::ZERO);
        Self::from_min_and_shape(
            (min + IVec3::ONE).as_uvec3(),
            (shape - IVec3::splat(2)).as_uvec3(),
        )
    }

    #[inline]
    pub fn least_upper_bound(&self) -> UVec3 {
        self.minimum + self.shape
    }

    #[inline]
    pub fn check_positive_shape(self) -> Option<Self> {
        self.shape.cmpgt(UVec3::ZERO).all().then_some(self)
    }

    #[inline]
    pub fn intersection(&self, other: &Self) -> Self {
        let minimum = self.minimum.max(other.minimum);
        let lub = self.least_upper_bound().min(other.least_upper_bound());
        Self::from_min_and_lub(minimum, lub)
    }

    #[inline]
    pub fn is_subset_of(&self, other: &Self) -> bool {
        self.intersection(other) == *self
    }

    /// Every point, z outermost and x innermost.
    pub fn iter3(&self) -> impl Iterator<Item = UVec3> {
        let (min, lub) = (self.minimum, self.least_upper_bound());
        (min.z..lub.z).flat_map(move |z| {
            (min.y..lub.y).flat_map(move |y| (min.x..lub.x).map(move |x| UVec3::new(x, y, z)))
        })
    }
}

impl Add<UVec3> for Extent {
    type Output = Self;

    #[inline]
    fn add(self, rhs: UVec3) -> Self {
        Self {
            minimum: self.minimum + rhs,
            shape: self.shape,
        }
    }
}
