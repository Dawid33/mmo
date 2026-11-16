use quest_vm::{Fnv64, Mem, MemError, RollbackError};
use state_hash::StateHash;
use std::hash::Hasher;

fn hash_of(m: &Mem) -> u64 {
    let mut h = Fnv64::default();
    m.state_hash(&mut h);
    h.finish()
}

#[test]
fn write_then_read_le() {
    let mut m = Mem::new(64);
    m.w64(8, 0x0102030405060708).unwrap();
    assert_eq!(m.bytes()[8..16], [8, 7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(m.r64(8).unwrap(), 0x0102030405060708);
}

#[test]
fn fnv64_known_vectors() {
    let mut h = Fnv64::default();
    h.write(b"");
    assert_eq!(h.finish(), 0xcbf29ce484222325);
    let mut h = Fnv64::default();
    h.write(b"a");
    assert_eq!(h.finish(), 0xaf63dc4c8601ec8c);
}

#[test]
fn new_rounds_down_to_words() {
    assert_eq!(Mem::new(67).len(), 64);
}

#[test]
fn rollback_restores_bytes_and_hash() {
    let mut m = Mem::new(256);
    m.w64(0, 7).unwrap();
    let (bytes, hash, cp) = (m.bytes().to_vec(), hash_of(&m), m.checkpoint());
    for i in 0..100u64 {
        m.w64(((i * 5) % 32 * 8) as u32, i + 1000).unwrap();
    }
    assert_ne!(hash_of(&m), hash);
    m.rollback_to(cp).unwrap();
    assert_eq!(m.bytes(), &bytes[..]);
    assert_eq!(hash_of(&m), hash);
}

#[test]
fn nested_checkpoints() {
    let mut m = Mem::new(64);
    let cp1 = m.checkpoint();
    m.w64(0, 1).unwrap();
    m.w64(8, 2).unwrap();
    let s1 = m.bytes().to_vec();
    let cp2 = m.checkpoint();
    m.w64(0, 9).unwrap();
    m.w64(16, 3).unwrap();
    m.rollback_to(cp2).unwrap();
    assert_eq!(m.bytes(), &s1[..]);
    m.rollback_to(cp1).unwrap();
    assert_eq!(m.bytes(), &[0u8; 64][..]);
}

#[test]
fn same_value_write_logs_nothing() {
    let mut m = Mem::new(64);
    m.w64(0, 5).unwrap();
    let n = m.log_len();
    m.w64(0, 5).unwrap();
    m.w64(8, 0).unwrap();
    assert_eq!(m.log_len(), n);
}

#[test]
fn forget_before_drops_history() {
    let mut m = Mem::new(64);
    let cp0 = m.checkpoint();
    m.w64(0, 1).unwrap();
    m.w64(8, 2).unwrap();
    let cp1 = m.checkpoint();
    m.w64(16, 3).unwrap();
    assert_eq!(m.log_len(), 3);
    m.forget_before(cp1);
    assert_eq!(m.log_len(), 1);
    // a later checkpoint still works
    m.rollback_to(cp1).unwrap();
    assert_eq!(m.r64(16).unwrap(), 0);
    assert_eq!(m.r64(8).unwrap(), 2);
    // a forgotten one is an error and changes nothing
    let before = m.bytes().to_vec();
    assert_eq!(m.rollback_to(cp0), Err(RollbackError::Forgotten));
    assert_eq!(m.bytes(), &before[..]);
}

#[test]
fn clear_log_forgets_everything() {
    let mut m = Mem::new(64);
    let cp = m.checkpoint();
    m.w64(0, 1).unwrap();
    m.clear_log();
    assert_eq!(m.log_len(), 0);
    assert_eq!(m.rollback_to(cp), Err(RollbackError::Forgotten));
    let cp2 = m.checkpoint();
    m.w64(0, 2).unwrap();
    m.rollback_to(cp2).unwrap();
    assert_eq!(m.r64(0).unwrap(), 1);
}

#[test]
fn out_of_range_and_unaligned_are_errors() {
    let mut m = Mem::new(64);
    assert_eq!(m.r64(64), Err(MemError::OutOfRange { off: 64 }));
    assert_eq!(m.w64(64, 1), Err(MemError::OutOfRange { off: 64 }));
    assert_eq!(m.r64(4), Err(MemError::Unaligned { off: 4 }));
    assert_eq!(m.w64(4, 1), Err(MemError::Unaligned { off: 4 }));
    assert_eq!(m.r64(u32::MAX), Err(MemError::Unaligned { off: u32::MAX }));
    assert_eq!(m.r64(u32::MAX - 7), Err(MemError::OutOfRange { off: u32::MAX - 7 }));
}

#[test]
fn from_bytes_has_empty_log() {
    let m = Mem::from_bytes(vec![1; 16]);
    assert_eq!(m.log_len(), 0);
    assert_eq!(m.len(), 16);
}

#[cfg(not(target_arch = "wasm32"))]
mod prop {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn random_writes_rollback_bit_exact(
            ops in proptest::collection::vec((0u32..16, any::<u64>(), any::<bool>()), 1..200)
        ) {
            let mut m = Mem::new(128);
            let mut saved = Vec::new();
            for (word, v, take) in ops {
                if take {
                    saved.push((m.checkpoint(), m.bytes().to_vec(), hash_of(&m)));
                }
                m.w64(word * 8, v).unwrap();
            }
            for (cp, bytes, hash) in saved.into_iter().rev() {
                m.rollback_to(cp).unwrap();
                prop_assert_eq!(m.bytes(), &bytes[..]);
                prop_assert_eq!(hash_of(&m), hash);
            }
        }
    }
}
