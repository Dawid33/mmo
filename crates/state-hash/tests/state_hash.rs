//! The `StateHash` contract (plan Task 4.4 Step 1, decision D2).
//!
//! Every test compares the exact byte stream a value feeds the hasher
//! (`bytes_of`), so "hashes like" means "feeds identical bytes", not "happens
//! to collide under crc32".

use core::hash::Hasher;
use indexmap::IndexMap;
use state_hash::{DynStateHash, StateHash};

/// Records every byte written, so two values can be compared stream to stream.
#[derive(Default)]
struct Recorder(Vec<u8>);

impl Hasher for Recorder {
    fn finish(&self) -> u64 {
        0
    }
    fn write(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

fn bytes_of<T: StateHash + ?Sized>(v: &T) -> Vec<u8> {
    let mut r = Recorder::default();
    v.state_hash(&mut r);
    r.0
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[test]
fn zero_and_negative_zero_hash_equal() {
    assert_eq!(bytes_of(&0.0f32), bytes_of(&-0.0f32));
    assert_eq!(
        bytes_of(&glamx::Vec3::new(-0.0, 1.0, -0.0)),
        bytes_of(&glamx::Vec3::new(0.0, 1.0, 0.0))
    );
}

#[test]
fn all_nans_hash_equal() {
    let canon = bytes_of(&f32::NAN);
    for bits in [0x7fc0_0000u32, 0xffc0_0000, 0x7f80_0001, 0xffff_ffff, 0x7fc0_1234] {
        let nan = f32::from_bits(bits);
        assert!(nan.is_nan());
        assert_eq!(bytes_of(&nan), canon, "NaN bits {bits:#x}");
    }
    assert_ne!(bytes_of(&f32::INFINITY), canon);
}

#[test]
fn floats_hash_their_bits() {
    assert_eq!(bytes_of(&1.5f32), 1.5f32.to_bits().to_le_bytes().to_vec());
    assert_ne!(bytes_of(&1.0f32), bytes_of(&f32::from_bits(1.0f32.to_bits() + 1)));
    assert_eq!(bytes_of(&-0.0f64), bytes_of(&0.0f64));
    assert_eq!(bytes_of(&f64::from_bits(0xfff8_0000_0000_0001)), bytes_of(&f64::NAN));
}

#[test]
fn usize_hashes_like_the_same_u64() {
    assert_eq!(bytes_of(&7usize), bytes_of(&7u64));
    assert_eq!(bytes_of(&usize::MAX), bytes_of(&(usize::MAX as u64)));
    assert_eq!(bytes_of(&-3isize), bytes_of(&-3i64));
    assert_eq!(bytes_of(&7usize).len(), 8);
}

#[test]
fn sequences_hash_their_length_as_u64_then_elements() {
    let v: Vec<u16> = vec![1, 2];
    let expect = cat(&[&2u64.to_le_bytes(), &1u16.to_le_bytes(), &2u16.to_le_bytes()]);
    assert_eq!(bytes_of(&v), expect);
    assert_eq!(bytes_of(&v[..]), expect);
    assert_eq!(bytes_of(&[1u16, 2u16]), expect);
    // Length prefixes keep ([1], [2]) and ([1, 2], []) apart.
    assert_ne!(
        bytes_of(&(vec![1u8], vec![2u8])),
        bytes_of(&(vec![1u8, 2u8], Vec::<u8>::new()))
    );
}

#[test]
fn option_hashes_a_u64_tag_then_the_value() {
    assert_eq!(bytes_of(&None::<u8>), 0u64.to_le_bytes().to_vec());
    assert_eq!(bytes_of(&Some(5u8)), cat(&[&1u64.to_le_bytes(), &[5]]));
}

#[test]
fn tuples_and_wrappers_hash_their_contents() {
    assert_eq!(bytes_of(&(1u8, 2u32)), cat(&[&[1], &2u32.to_le_bytes()]));
    assert_eq!(bytes_of(&Box::new(3u32)), bytes_of(&3u32));
    assert_eq!(bytes_of(&std::sync::Arc::new(3u32)), bytes_of(&3u32));
    assert_eq!(bytes_of(&true), vec![1]);
    assert_eq!(bytes_of(&core::marker::PhantomData::<f32>), Vec::<u8>::new());
}

#[test]
fn index_map_hashes_length_then_entries_in_order() {
    let mut a = IndexMap::new();
    a.insert(1u32, 10u8);
    a.insert(2u32, 20u8);
    let expect = cat(&[
        &2u64.to_le_bytes(),
        &1u32.to_le_bytes(),
        &[10],
        &2u32.to_le_bytes(),
        &[20],
    ]);
    assert_eq!(bytes_of(&a), expect);
    // Insertion order is iteration order, which drives the engine: it is state.
    let mut b = IndexMap::new();
    b.insert(2u32, 20u8);
    b.insert(1u32, 10u8);
    assert_ne!(bytes_of(&a), bytes_of(&b));
}

#[test]
fn glam_types_hash_component_wise() {
    use glamx::{IVec3, Mat3, Pose3, Pose3A, Quat, Vec3, Vec3A};
    let v = Vec3::new(1.0, 2.0, 3.0);
    assert_eq!(bytes_of(&v), bytes_of(&(1.0f32, 2.0f32, 3.0f32)));
    assert_eq!(bytes_of(&Vec3A::from(v)), bytes_of(&v));
    assert_eq!(bytes_of(&IVec3::new(-1, 0, 7)), bytes_of(&(-1i32, 0i32, 7i32)));
    let q = Quat::from_xyzw(0.0, 0.0, 0.0, 1.0);
    assert_eq!(bytes_of(&q), bytes_of(&(0.0f32, 0.0f32, 0.0f32, 1.0f32)));
    let m = Mat3::from_cols_array(&[1., 2., 3., 4., 5., 6., 7., 8., 9.]);
    assert_eq!(bytes_of(&m), bytes_of(&[1f32, 2., 3., 4., 5., 6., 7., 8., 9.])[8..]);
    let p = Pose3::from_parts(v, q);
    assert_eq!(bytes_of(&p), cat(&[&bytes_of(&q), &bytes_of(&v)]));
    assert_eq!(bytes_of(&Pose3A::from_parts(v.into(), q)), bytes_of(&p));
    // Pose3's padding "can have any value": it is not state.
    let mut padded = p;
    padded.padding = 12345;
    assert_eq!(bytes_of(&padded), bytes_of(&p));
}

#[derive(StateHash)]
struct Plain {
    a: u8,
    b: f32,
    c: Vec<u32>,
}

#[derive(StateHash)]
struct WithSkip {
    a: u32,
    #[state_hash(skip)]
    _cache: u64,
}

#[derive(StateHash)]
struct Tuple(u8, u16);

#[derive(StateHash)]
struct Generic<T> {
    t: T,
}

#[derive(StateHash)]
struct Unit;

#[derive(StateHash)]
enum Shape {
    Empty,
    Ball(f32),
    Box { hx: f32, #[state_hash(skip)] _scratch: u8, hy: f32 },
}

#[test]
fn derived_struct_hashes_like_its_fields_in_order() {
    let p = Plain { a: 1, b: 2.0, c: vec![3] };
    assert_eq!(bytes_of(&p), bytes_of(&(1u8, 2.0f32, vec![3u32])));
    assert_eq!(bytes_of(&Tuple(1, 2)), bytes_of(&(1u8, 2u16)));
    assert_eq!(bytes_of(&Generic { t: 9u64 }), bytes_of(&9u64));
    assert_eq!(bytes_of(&Unit), Vec::<u8>::new());
}

#[test]
fn any_field_change_changes_the_hash() {
    let base = bytes_of(&Plain { a: 1, b: 2.0, c: vec![3] });
    assert_ne!(base, bytes_of(&Plain { a: 9, b: 2.0, c: vec![3] }));
    assert_ne!(base, bytes_of(&Plain { a: 1, b: 2.5, c: vec![3] }));
    assert_ne!(base, bytes_of(&Plain { a: 1, b: 2.0, c: vec![4] }));
    assert_ne!(base, bytes_of(&Plain { a: 1, b: 2.0, c: vec![3, 0] }));
}

#[test]
fn skipped_fields_are_not_hashed() {
    assert_eq!(bytes_of(&WithSkip { a: 5, _cache: 1 }), bytes_of(&5u32));
    assert_eq!(
        bytes_of(&WithSkip { a: 5, _cache: 1 }),
        bytes_of(&WithSkip { a: 5, _cache: 2 })
    );
}

#[test]
fn derived_enum_hashes_variant_index_as_u32_then_fields() {
    assert_eq!(bytes_of(&Shape::Empty), 0u32.to_le_bytes().to_vec());
    assert_eq!(bytes_of(&Shape::Ball(1.0)), bytes_of(&(1u32, 1.0f32)));
    assert_eq!(
        bytes_of(&Shape::Box { hx: 1.0, _scratch: 7, hy: 2.0 }),
        bytes_of(&(2u32, 1.0f32, 2.0f32))
    );
    // Same payload, different variant.
    assert_ne!(bytes_of(&Shape::Ball(0.0)), bytes_of(&Shape::Empty));
}

#[test]
fn dyn_state_hash_hashes_like_the_concrete_value() {
    let boxed: Box<dyn DynStateHash> = Box::new(Plain { a: 1, b: 2.0, c: vec![3] });
    let mut r = Recorder::default();
    boxed.dyn_state_hash(&mut r);
    assert_eq!(r.0, bytes_of(&Plain { a: 1, b: 2.0, c: vec![3] }));
}

#[test]
fn hashbrown_map_hashes_sorted_by_key_whatever_the_insertion_order() {
    let mut a = hashbrown::HashMap::new();
    let mut b = hashbrown::HashMap::new();
    for k in 0u32..64 {
        a.insert(k, k as u8);
        b.insert(63 - k, (63 - k) as u8);
    }
    assert_eq!(bytes_of(&a), bytes_of(&b));
    let mut sorted = IndexMap::new();
    for k in 0u32..64 {
        sorted.insert(k, k as u8);
    }
    assert_eq!(bytes_of(&a), bytes_of(&sorted));
    b.insert(7, 0);
    assert_ne!(bytes_of(&a), bytes_of(&b));
}

#[test]
fn nalgebra_matrices_hash_shape_then_column_major_elements() {
    use nalgebra::{DMatrix, DVector, Vector2};
    let v = Vector2::new(1.0f32, 2.0);
    assert_eq!(bytes_of(&v), bytes_of(&(2u64, 1u64, 1.0f32, 2.0f32)));
    assert_eq!(bytes_of(&DVector::from_vec(vec![1.0f32, 2.0])), bytes_of(&v));
    let m = DMatrix::from_column_slice(2, 3, &[1.0f32, 2., 3., 4., 5., 6.]);
    assert_eq!(bytes_of(&m), bytes_of(&(2u64, 3u64, 1.0f32, 2.0f32, 3.0f32, 4.0f32, 5.0f32, 6.0f32)));
    // Same elements, other shape.
    assert_ne!(bytes_of(&m), bytes_of(&DMatrix::from_column_slice(3, 2, &[1.0f32, 2., 3., 4., 5., 6.])));
    assert_eq!(bytes_of(&DVector::from_vec(vec![-0.0f32])), bytes_of(&DVector::from_vec(vec![0.0f32])));
}

#[test]
fn impl_via_bits_hashes_the_bits() {
    struct Flags(u16);
    impl Flags {
        fn bits(&self) -> u16 {
            self.0
        }
    }
    state_hash::impl_via_bits!(Flags);
    assert_eq!(bytes_of(&Flags(0x1234)), bytes_of(&0x1234u16));
}

fn hash_sorted<H: core::hash::Hasher>(v: &Vec<u32>, h: &mut H) {
    let mut sorted = v.clone();
    sorted.sort();
    sorted.state_hash(h);
}

#[derive(StateHash)]
struct WithCustomField {
    a: u8,
    #[state_hash(with = "hash_sorted")]
    set: Vec<u32>,
}

#[test]
fn with_attribute_routes_a_field_through_a_function() {
    let x = WithCustomField { a: 1, set: vec![3, 1, 2] };
    let y = WithCustomField { a: 1, set: vec![1, 2, 3] };
    assert_eq!(bytes_of(&x), bytes_of(&y));
    assert_eq!(bytes_of(&x), bytes_of(&(1u8, vec![1u32, 2, 3])));
}
