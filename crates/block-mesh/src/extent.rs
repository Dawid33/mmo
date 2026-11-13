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
        Self { minimum, shape: lub.saturating_sub(minimum) }
    }

    /// `min..=max` shrunk by one voxel on every side (room for a 3×3×3
    /// kernel), computed in signed space and cast back, as ilattice's
    /// `padded(-1)` + `as_uvec3` did.
    #[inline]
    pub fn interior(min: [u32; 3], max: [u32; 3]) -> Self {
        let min = UVec3::from(min).as_ivec3();
        let max = UVec3::from(max).as_ivec3();
        let shape = (max + IVec3::ONE - min).max(IVec3::ZERO);
        Self::from_min_and_shape((min + IVec3::ONE).as_uvec3(), (shape - IVec3::splat(2)).as_uvec3())
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
        (min.z..lub.z).flat_map(move |z| (min.y..lub.y).flat_map(move |y| (min.x..lub.x).map(move |x| UVec3::new(x, y, z))))
    }
}

impl Add<UVec3> for Extent {
    type Output = Self;

    #[inline]
    fn add(self, rhs: UVec3) -> Self {
        Self { minimum: self.minimum + rhs, shape: self.shape }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iter3_walks_x_fastest() {
        let e = Extent::from_min_and_shape(UVec3::new(1, 2, 3), UVec3::new(2, 2, 2));
        let pts: Vec<[u32; 3]> = e.iter3().map(|p| p.to_array()).collect();
        assert_eq!(pts[..3], [[1, 2, 3], [2, 2, 3], [1, 3, 3]]);
        assert_eq!(pts.len(), 8);
    }

    #[test]
    fn max_is_inclusive_and_subset_checks_bounds() {
        let outer = Extent::from_min_and_shape(UVec3::ZERO, UVec3::splat(34));
        assert_eq!(Extent::from_min_and_max(UVec3::ZERO, UVec3::splat(33)), outer);
        assert!(Extent::from_min_and_max(UVec3::splat(1), UVec3::splat(33)).is_subset_of(&outer));
        assert!(!Extent::from_min_and_max(UVec3::ZERO, UVec3::new(34, 33, 33)).is_subset_of(&outer));
    }

    #[test]
    fn interior_drops_one_voxel_each_side() {
        let i = Extent::interior([0; 3], [33; 3]);
        assert_eq!((i.minimum, i.shape), (UVec3::ONE, UVec3::splat(32)));
    }
}
