//! Prefab RON files and export strings: round trips and hostile input.

use crate::common::prefabs::{arch, rich};
use base64::Engine;
use game::{
    prefab_from_export, prefab_from_ron, prefab_to_export, prefab_to_ron, resolve, PrefabError, Trust,
    EXPORT_MAX_BYTES, EXPORT_MAX_CHARS,
};

const ARCH_RON: &str = include_str!("../../../assets/prefabs/dev-arch.prefab.ron");

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn ron_round_trip_is_byte_stable() {
    for f in [arch(), rich()] {
        let s = prefab_to_ron(&f);
        assert!(s.ends_with('\n'));
        assert_eq!(prefab_from_ron(&s).unwrap(), f);
        assert_eq!(prefab_to_ron(&prefab_from_ron(&s).unwrap()), s);
    }
}

#[test]
fn sample_asset_parses_and_resolves() {
    let f = prefab_from_ron(ARCH_RON).unwrap();
    assert_eq!(f.name, "dev-arch");
    resolve(&f, Trust::Builtin).unwrap();
}

#[test]
fn sample_asset_is_the_fixture_in_canonical_form() {
    assert_eq!(prefab_to_ron(&arch()), ARCH_RON);
}

#[test]
fn export_round_trip() {
    for f in [arch(), rich()] {
        let s = prefab_to_export(&f);
        assert!(s.starts_with("MMOPF1:"));
        assert_eq!(prefab_from_export(&format!("  {s}\n")).unwrap(), f);
    }
}

#[test]
fn hostile_strings_are_refused() {
    assert!(matches!(prefab_from_export("hello"), Err(PrefabError::NotAPrefabString)));
    assert!(matches!(prefab_from_export(""), Err(PrefabError::NotAPrefabString)));
    assert!(matches!(prefab_from_export("MMOPF1:!!!"), Err(PrefabError::Corrupt)));
    assert!(matches!(prefab_from_export("MMOPF1:"), Err(PrefabError::Corrupt)));
    let s = prefab_to_export(&arch());
    assert!(matches!(prefab_from_export(&s[..s.len() / 2]), Err(PrefabError::Corrupt)));
    // An lz4 size header declaring far more than the limit, in front of a tiny block.
    let mut huge = u32::MAX.to_le_bytes().to_vec();
    huge.extend_from_slice(&[0x10, 0x00]);
    assert!(matches!(prefab_from_export(&format!("MMOPF1:{}", b64(&huge))), Err(PrefabError::TooLarge)));
    let mut over = (EXPORT_MAX_BYTES as u32 + 1).to_le_bytes().to_vec();
    over.extend_from_slice(&[0x10, 0x00]);
    assert!(matches!(prefab_from_export(&format!("MMOPF1:{}", b64(&over))), Err(PrefabError::TooLarge)));
    assert!(matches!(
        prefab_from_export(&format!("MMOPF1:{}", "A".repeat(EXPORT_MAX_CHARS + 1))),
        Err(PrefabError::TooLarge)
    ));
}

#[test]
fn nonsense_payloads_are_corrupt_not_a_panic() {
    // Valid base64 + valid lz4 of bytes that are not a prefab.
    let junk = lz4_flex::block::compress_prepend_size(&[0xFFu8; 64]);
    assert!(matches!(prefab_from_export(&format!("MMOPF1:{}", b64(&junk))), Err(PrefabError::Corrupt)));
    // A size header that lies low: the block does not fit the declared size.
    let mut lies = lz4_flex::block::compress_prepend_size(&[7u8; 4096]);
    lies[..4].copy_from_slice(&8u32.to_le_bytes());
    assert!(matches!(prefab_from_export(&format!("MMOPF1:{}", b64(&lies))), Err(PrefabError::Corrupt)));
    // Every prefix and every single-byte corruption of a good string decodes or is refused.
    let s = prefab_to_export(&arch());
    for cut in 0..s.len() {
        let _ = prefab_from_export(&s[..cut]);
    }
    let mut bytes = s.clone().into_bytes();
    for i in 7..bytes.len() {
        let keep = bytes[i];
        bytes[i] = if keep == b'A' { b'B' } else { b'A' };
        let _ = prefab_from_export(std::str::from_utf8(&bytes).unwrap());
        bytes[i] = keep;
    }
}

#[test]
fn ron_syntax_error_is_reported() {
    assert!(matches!(prefab_from_ron("(format: 1, nam"), Err(PrefabError::Syntax(_))));
}
