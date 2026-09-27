//! Zero-copy canonical state hashing (rapier 0.36 upgrade, plan decision D2).
//!
//! [`StateHash`] has std `Hash`'s shape — a field-by-field walk into a
//! [`core::hash::Hasher`] — but is ours, so it can be implemented for foreign
//! float types (glam) and it pins every width so native and wasm32 feed the
//! hasher identical bytes:
//!
//! - `f32`/`f64`: canonicalised bits (`-0.0` → `0.0`, every NaN → one NaN),
//!   matching `OrderedFloat`'s `Hash`;
//! - `usize`/`isize`: written as `u64`/`i64`;
//! - slices, `Vec`, arrays, maps: length as `u64`, then the elements in order;
//! - `Option`: `0u64`, or `1u64` then the value;
//! - tuples, `Box`, `Arc`, `Rc`, references: their contents, no prefix;
//! - `#[derive(StateHash)]`: structs hash their fields in declaration order,
//!   enums the variant index as `u32` then the variant's fields;
//!   `#[state_hash(skip)]` leaves out caches and workspaces.
//!
//! Integers are written with `Hasher::write_*`, whose default impls write
//! native-endian bytes; every target this project ships (x86_64, aarch64,
//! wasm32) is little-endian.
//!
//! Hashing serialized bytes instead was rejected: the user measured it far
//! slower than `Hash` impls (plan D2).
#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::hash::Hasher;

#[cfg(feature = "derive")]
pub use state_hash_derive::StateHash;

/// Zero-copy canonical state hash: std `Hash`'s shape, but ours, so it can be
/// implemented for foreign `f32` types (glam) and pins widths across targets.
pub trait StateHash {
    /// Feeds this value into `h`.
    fn state_hash<H: Hasher>(&self, h: &mut H);
}

/// Object-safe twin of [`StateHash`], for trait objects (`dyn Shape`, the
/// contact-manifold workspaces): a trait that needs hashing takes this as a
/// supertrait, and `impl StateHash for dyn Trait` forwards to it.
pub trait DynStateHash {
    /// Feeds this value into `h`, exactly as [`StateHash::state_hash`] would.
    fn dyn_state_hash(&self, h: &mut dyn Hasher);
}

impl<T: StateHash + ?Sized> DynStateHash for T {
    fn dyn_state_hash(&self, mut h: &mut dyn Hasher) {
        self.state_hash(&mut h);
    }
}

/// `impl StateHash` for a `bitflags!` type (or anything with a `bits()`
/// accessor): the flags hash as their bits. A derive can't see through
/// bitflags 2's private inner type.
#[macro_export]
macro_rules! impl_via_bits {
    ($($t:ty),+ $(,)?) => {$(
        impl $crate::StateHash for $t {
            #[inline]
            fn state_hash<H: ::core::hash::Hasher>(&self, h: &mut H) {
                $crate::StateHash::state_hash(&self.bits(), h);
            }
        }
    )+};
}

/// The length prefix every sequence and map writes.
#[inline]
pub fn hash_len<H: Hasher>(len: usize, h: &mut H) {
    h.write_u64(len as u64);
}

/// The canonical bits of an `f32`: `-0.0` hashes as `0.0`, every NaN as `f32::NAN`.
#[inline]
pub fn canonical_f32_bits(v: f32) -> u32 {
    if v.is_nan() {
        f32::NAN.to_bits()
    } else if v == 0.0 {
        0
    } else {
        v.to_bits()
    }
}

/// The canonical bits of an `f64`, as [`canonical_f32_bits`].
#[inline]
pub fn canonical_f64_bits(v: f64) -> u64 {
    if v.is_nan() {
        f64::NAN.to_bits()
    } else if v == 0.0 {
        0
    } else {
        v.to_bits()
    }
}

macro_rules! impl_int {
    ($($t:ty => $w:ident),* $(,)?) => {$(
        impl StateHash for $t {
            #[inline]
            fn state_hash<H: Hasher>(&self, h: &mut H) {
                h.$w(*self);
            }
        }
    )*};
}

impl_int!(
    u8 => write_u8, u16 => write_u16, u32 => write_u32, u64 => write_u64, u128 => write_u128,
    i8 => write_i8, i16 => write_i16, i32 => write_i32, i64 => write_i64, i128 => write_i128,
);

impl StateHash for usize {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u64(*self as u64);
    }
}

impl StateHash for isize {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_i64(*self as i64);
    }
}

impl StateHash for bool {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u8(*self as u8);
    }
}

impl StateHash for char {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u32(*self as u32);
    }
}

impl StateHash for f32 {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u32(canonical_f32_bits(*self));
    }
}

impl StateHash for f64 {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        h.write_u64(canonical_f64_bits(*self));
    }
}

impl StateHash for () {
    #[inline]
    fn state_hash<H: Hasher>(&self, _h: &mut H) {}
}

impl<T: ?Sized> StateHash for core::marker::PhantomData<T> {
    #[inline]
    fn state_hash<H: Hasher>(&self, _h: &mut H) {}
}

impl<T: StateHash> StateHash for [T] {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        hash_len(self.len(), h);
        for x in self {
            x.state_hash(h);
        }
    }
}

impl<T: StateHash, const N: usize> StateHash for [T; N] {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self[..].state_hash(h);
    }
}

impl<T: StateHash> StateHash for Vec<T> {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self[..].state_hash(h);
    }
}

impl<T: StateHash> StateHash for VecDeque<T> {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        hash_len(self.len(), h);
        for x in self {
            x.state_hash(h);
        }
    }
}

impl StateHash for str {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.as_bytes().state_hash(h);
    }
}

impl StateHash for String {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.as_str().state_hash(h);
    }
}

impl<T: StateHash> StateHash for Option<T> {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        match self {
            None => h.write_u64(0),
            Some(v) => {
                h.write_u64(1);
                v.state_hash(h);
            }
        }
    }
}

impl<T: StateHash + ?Sized> StateHash for &T {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        (**self).state_hash(h);
    }
}

impl<T: StateHash + ?Sized> StateHash for Box<T> {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        (**self).state_hash(h);
    }
}

impl<T: StateHash + ?Sized> StateHash for Arc<T> {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        (**self).state_hash(h);
    }
}

impl<T: StateHash + ?Sized> StateHash for Rc<T> {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        (**self).state_hash(h);
    }
}

impl<K: StateHash, V: StateHash> StateHash for BTreeMap<K, V> {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        hash_len(self.len(), h);
        for (k, v) in self {
            k.state_hash(h);
            v.state_hash(h);
        }
    }
}

impl<T: StateHash> StateHash for BTreeSet<T> {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        hash_len(self.len(), h);
        for x in self {
            x.state_hash(h);
        }
    }
}

impl<T: StateHash> StateHash for core::ops::Range<T> {
    #[inline]
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.start.state_hash(h);
        self.end.state_hash(h);
    }
}

macro_rules! impl_tuple {
    ($($n:tt $T:ident),+) => {
        impl<$($T: StateHash),+> StateHash for ($($T,)+) {
            #[inline]
            fn state_hash<H: Hasher>(&self, h: &mut H) {
                $(self.$n.state_hash(h);)+
            }
        }
    };
}

impl_tuple!(0 A);
impl_tuple!(0 A, 1 B);
impl_tuple!(0 A, 1 B, 2 C);
impl_tuple!(0 A, 1 B, 2 C, 3 D);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G);
impl_tuple!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 I);

/// `IndexMap`/`IndexSet` (parry's `HashMap` under `enhanced-determinism`):
/// insertion order is iteration order, which the engine's behaviour follows
/// and serde writes, so it is state — hashed in order, not sorted.
#[cfg(feature = "indexmap")]
mod indexmap_impls {
    use super::*;

    impl<K: StateHash, V: StateHash, S> StateHash for indexmap::IndexMap<K, V, S> {
        fn state_hash<H: Hasher>(&self, h: &mut H) {
            hash_len(self.len(), h);
            for (k, v) in self {
                k.state_hash(h);
                v.state_hash(h);
            }
        }
    }

    impl<T: StateHash, S> StateHash for indexmap::IndexSet<T, S> {
        fn state_hash<H: Hasher>(&self, h: &mut H) {
            hash_len(self.len(), h);
            for x in self {
                x.state_hash(h);
            }
        }
    }
}

/// `hashbrown` maps and sets (parry's `HashMap`/`HashSet` WITHOUT
/// `enhanced-determinism`): iteration order is an accident of the hasher and
/// the insertion history, not state, so entries are hashed sorted by key.
/// Costs an allocation and a sort; the game never builds this configuration.
#[cfg(feature = "hashbrown")]
mod hashbrown_impls {
    use super::*;

    impl<K: StateHash + Ord, V: StateHash, S> StateHash for hashbrown::HashMap<K, V, S> {
        fn state_hash<H: Hasher>(&self, h: &mut H) {
            let mut entries: Vec<(&K, &V)> = self.iter().collect();
            entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
            hash_len(entries.len(), h);
            for (k, v) in entries {
                k.state_hash(h);
                v.state_hash(h);
            }
        }
    }

    impl<T: StateHash + Ord, S> StateHash for hashbrown::HashSet<T, S> {
        fn state_hash<H: Hasher>(&self, h: &mut H) {
            let mut entries: Vec<&T> = self.iter().collect();
            entries.sort_unstable();
            hash_len(entries.len(), h);
            for x in entries {
                x.state_hash(h);
            }
        }
    }
}

/// nalgebra matrices (rapier 0.36 keeps a few internally: multibody state, a
/// contact's tangent impulse): rows and columns as `u64` (a dynamic matrix's
/// shape is state), then the elements column-major.
#[cfg(feature = "nalgebra")]
mod nalgebra_impls {
    use super::*;
    use nalgebra::{Dim, Matrix, Scalar, storage::RawStorage};

    impl<T, R, C, S> StateHash for Matrix<T, R, C, S>
    where
        T: StateHash + Scalar,
        R: Dim,
        C: Dim,
        S: RawStorage<T, R, C>,
    {
        fn state_hash<H: Hasher>(&self, h: &mut H) {
            let (rows, cols) = self.shape();
            hash_len(rows, h);
            hash_len(cols, h);
            for x in self.iter() {
                x.state_hash(h);
            }
        }
    }
}

/// glam (through glamx, whose version the rapier/parry forks pin) and glamx's
/// pose types: component-wise, matrices column-major, no length prefix.
#[cfg(feature = "glamx")]
mod glam_impls {
    use super::*;
    use glamx::glam;

    macro_rules! impl_components {
        ($($t:ty => [$($f:ident),+]),* $(,)?) => {$(
            impl StateHash for $t {
                #[inline]
                fn state_hash<H: Hasher>(&self, h: &mut H) {
                    $(self.$f.state_hash(h);)+
                }
            }
        )*};
    }

    impl_components!(
        glam::Vec2 => [x, y],
        glam::Vec3 => [x, y, z],
        glam::Vec3A => [x, y, z],
        glam::Vec4 => [x, y, z, w],
        glam::Quat => [x, y, z, w],
        glam::IVec2 => [x, y],
        glam::IVec3 => [x, y, z],
        glam::IVec4 => [x, y, z, w],
        glam::BVec2 => [x, y],
        glam::BVec3 => [x, y, z],
        glam::BVec4 => [x, y, z, w],
        glam::Mat2 => [x_axis, y_axis],
        glam::Mat3 => [x_axis, y_axis, z_axis],
        glam::Mat3A => [x_axis, y_axis, z_axis],
        glam::Mat4 => [x_axis, y_axis, z_axis, w_axis],
        glam::Affine3A => [matrix3, translation],
        // `Pose3::padding` "can have any value as it is never read": not state.
        glamx::Pose3 => [rotation, translation],
        glamx::Pose3A => [rotation, translation],
    );
}
