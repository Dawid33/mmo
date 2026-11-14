use std::collections::BTreeMap;

use game::prefab::*;

fn sample() -> PrefabFile {
    let mut props = BTreeMap::new();
    props.insert("hinge".to_string(), "left".to_string());
    PrefabFile {
        format: PREFAB_FORMAT,
        name: "hut".into(),
        meta: PrefabMeta { licence: "own work".into(), ..Default::default() },
        size: [5, 4, 5],
        anchor: [2, 0, -1],
        palette: vec!["oak_planks".into(), "glass".into(), "stone_bricks".into()],
        boxes: vec![
            FileBox { min: [0, 0, 0], size: [5, 1, 5], mat: 2 },
            FileBox { min: [0, 1, 0], size: [5, 2, 5], mat: 0 },
            FileBox { min: [2, 2, 0], size: [1, 1, 1], mat: 1 },
        ],
        shaped: vec![
            FileShaped {
                at: [1, 3, 1],
                shape: FileShape::Named {
                    shape: Shape::Stairs { facing: Facing::North, half: Half::Bottom, corner: StairCorner::Straight },
                    mat: 0,
                },
            },
            FileShaped {
                at: [2, 3, 2],
                shape: FileShape::Cuboids { cuboids: vec![([0, 0, 0], [8, 4, 16], 0)], mats: vec![1] },
            },
        ],
        carve: vec![FileCarve { min: [1, 1, 1], size: [3, 2, 3] }],
        markers: vec![Marker { at: [2, 1, 0], kind: "door".into(), turn: 0, props }],
        ground: GroundFit::Follow,
        tags: vec!["house".into()],
    }
}

#[test]
fn ron_round_trip_is_byte_stable() {
    let f = sample();
    let text = prefab_to_ron(&f);
    assert!(text.ends_with('\n'));
    let back = prefab_from_ron(&text).unwrap();
    assert_eq!(back, f);
    assert_eq!(prefab_to_ron(&back), text);
}

#[test]
fn a_short_hand_written_file_parses() {
    let f = prefab_from_ron(
        r#"(format: 2, name: "x", size: (1, 1, 1), palette: ["stone"], boxes: [(min: (0, 0, 0), size: (1, 1, 1), mat: 0)])"#,
    )
    .unwrap();
    assert_eq!(f.ground, GroundFit::Flat);
    assert!(f.shaped.is_empty() && f.carve.is_empty() && f.markers.is_empty() && f.tags.is_empty());
    assert_eq!(f.boxes.len(), 1);
}

#[test]
fn bin_round_trip() {
    let f = sample();
    let bytes = prefab_to_bin(&f);
    assert!(bytes.starts_with(BIN_MAGIC));
    assert_eq!(prefab_from_bin(&bytes, None), Ok(f));
}

#[test]
fn bin_refuses_bad_input() {
    assert_eq!(prefab_from_bin(&[], None), Err(PrefabError::NotAPrefab));
    assert_eq!(prefab_from_bin(b"NOTAPREFAB_AT_ALL", None), Err(PrefabError::NotAPrefab));
    let bytes = prefab_to_bin(&sample());
    for len in 7..bytes.len() {
        let r = prefab_from_bin(&bytes[..len], None);
        assert!(matches!(r, Err(PrefabError::Corrupt | PrefabError::NotAPrefab)), "len {len}: {r:?}");
    }
    let mut huge = BIN_MAGIC.to_vec();
    huge.extend_from_slice(&u64::MAX.to_le_bytes());
    huge.extend_from_slice(&[1, 2, 3, 4]);
    assert_eq!(prefab_from_bin(&huge, Some(1 << 20)), Err(PrefabError::TooLarge));
    assert_eq!(prefab_from_bin(&huge, None), Err(PrefabError::Corrupt));
    assert_eq!(prefab_from_bin(&bytes, Some(8)), Err(PrefabError::TooLarge));
}

#[test]
fn named_bytes_picks_by_suffix() {
    let f = sample();
    assert_eq!(prefab_from_named_bytes("a.prefab.ron", prefab_to_ron(&f).as_bytes(), None), Some(Ok(f.clone())));
    assert_eq!(prefab_from_named_bytes("a.prefab.bin", &prefab_to_bin(&f), None), Some(Ok(f)));
    assert_eq!(prefab_from_named_bytes("a.txt", b"x", None), None);
}

#[test]
fn a_misspelt_field_is_a_syntax_error() {
    let src = r#"(format: 2, name: "x", size: (1, 1, 1), palette: ["stone"], carves: [])"#;
    assert!(matches!(prefab_from_ron(src), Err(PrefabError::Syntax(_))));
    // Nested structs refuse unknown fields too.
    let src = r#"(format: 2, name: "x", size: (1, 1, 1), boxes: [(min: (0, 0, 0), size: (1, 1, 1), mat: 0, matt: 1)])"#;
    assert!(matches!(prefab_from_ron(src), Err(PrefabError::Syntax(_))));
}

#[test]
fn the_binary_round_trip_survives_strict_fields() {
    let f = sample();
    assert_eq!(prefab_from_bin(&prefab_to_bin(&f), None).unwrap(), f);
}
