//! Prefab file form → resolved form: name/id mapping, validation, caps, keys.

use crate::common::prefabs::{arch, build_file, build_of, build_of_mut, empty_build, part, resolved, rich};
use game::stud_catalog::STUD_PARTS;
use game::{
    resolve, stud_material_id, unresolve, BuildFile, MaterialName, PrefabKey, PrefabKindFile, PrefabMeta, Problem,
    PropFile, Trust, PREFAB_FORMAT, PREFAB_MAX_PARTS,
};

#[test]
fn resolve_maps_names_to_ids() {
    let r = resolved(&rich());
    let b = build_of(&r);
    assert_eq!(STUD_PARTS[b.parts[0].part as usize].number, "3001");
    assert_eq!(b.boxes[0].material, stud_material_id("stone").unwrap());
    assert_eq!(b.parts[0].material, 4);
}

#[test]
fn unresolve_round_trips() {
    for f in [rich(), arch()] {
        let r = resolve(&f, Trust::Builtin).unwrap();
        assert_eq!(unresolve(&r, f.meta.clone()), f);
    }
}

#[test]
fn problems_are_listed_not_panicked() {
    let mut unknown_material = part("3001", [0, 0, 0], 0);
    unknown_material.material = MaterialName::Natural("nope".into());
    let mut bad = build_file(
        "Bad Name",
        BuildFile {
            parts: vec![part("99999", [0, 0, 0], 0), part("3001", [0, 0, 0], 7), part("3001", [48, 0, 0], 0), unknown_material],
            ..empty_build()
        },
    );
    bad.format = 9;
    let e = resolve(&bad, Trust::Builtin).unwrap_err();
    assert!(e.contains(&Problem::UnknownPart("99999".into())));
    assert!(e.contains(&Problem::UnknownMaterial("nope".into())));
    assert!(e.contains(&Problem::BadRotation(1)));
    assert!(e.contains(&Problem::OutOfBounds(2)));
    assert!(e.contains(&Problem::BadName));
    assert!(e.contains(&Problem::BadFormat(9)));
}

#[test]
fn bounds_follow_the_turned_footprint() {
    // 3001 is 4 x 3 x 2: unturned it reaches x 44..48 (in), turned its z reach is 4.
    let ok = build_file("b", BuildFile { parts: vec![part("3001", [44, 0, 46], 0)], ..empty_build() });
    assert!(resolve(&ok, Trust::Builtin).is_ok());
    let out = build_file("b", BuildFile { parts: vec![part("3001", [44, 0, 46], 1)], ..empty_build() });
    assert_eq!(resolve(&out, Trust::Builtin), Err(vec![Problem::OutOfBounds(0)]));
    let low = build_file("b", BuildFile { parts: vec![part("3001", [0, -91, 0], 0)], ..empty_build() });
    assert_eq!(resolve(&low, Trust::Builtin), Err(vec![Problem::OutOfBounds(0)]));
    let high = build_file("b", BuildFile { parts: vec![part("3001", [0, 178, 0], 0)], ..empty_build() });
    assert_eq!(resolve(&high, Trust::Builtin), Err(vec![Problem::OutOfBounds(0)]));
}

#[test]
fn boxes_are_checked() {
    let mut f = rich();
    let PrefabKindFile::Build(b) = &mut f.kind else { unreachable!() };
    b.boxes[0].size = [0, 3, 2];
    assert_eq!(resolve(&f, Trust::Builtin), Err(vec![Problem::EmptyBox(0)]));
    let PrefabKindFile::Build(b) = &mut f.kind else { unreachable!() };
    b.boxes[0].size = [u32::MAX, 3, 2];
    assert_eq!(resolve(&f, Trust::Builtin), Err(vec![Problem::OutOfBounds(0)]));
}

#[test]
fn caps() {
    let f = build_file("many", BuildFile { parts: vec![part("3005", [0, 0, 0], 0); PREFAB_MAX_PARTS + 1], ..empty_build() });
    let err = resolve(&f, Trust::Builtin).unwrap_err();
    assert!(matches!(err[..], [Problem::TooManyParts(4097)]));
}

#[test]
fn player_trust_zeroes_data() {
    let mut p = part("3001", [0, 0, 0], 0);
    p.data = 7;
    let f = build_file("d", BuildFile { parts: vec![p], ..empty_build() });
    assert_eq!(build_of(&resolve(&f, Trust::Player).unwrap()).parts[0].data, 0);
    assert_eq!(build_of(&resolve(&f, Trust::Builtin).unwrap()).parts[0].data, 7);
}

#[test]
fn validate_refuses_hand_built() {
    let good = resolved(&arch());
    assert_eq!(good.validate(Trust::Player), Ok(()));
    let mut p = good.clone();
    build_of_mut(&mut p).parts[0].part = u16::MAX;
    assert!(p.validate(Trust::Player).is_err());
    let mut p = good.clone();
    build_of_mut(&mut p).parts[0].data = 3;
    assert!(p.validate(Trust::Player).is_err());
    assert!(p.validate(Trust::Builtin).is_ok());
    let mut p = good.clone();
    build_of_mut(&mut p).parts[0].material = 9999;
    assert!(p.validate(Trust::Player).is_err());
    let mut p = good.clone();
    build_of_mut(&mut p).parts[0].min = [48, 0, 0];
    assert!(p.validate(Trust::Player).is_err());
    let mut p = good.clone();
    p.name = "Bad Name".into();
    assert!(p.validate(Trust::Player).is_err());
}

#[test]
fn key_is_content_addressed() {
    let a = resolved(&arch());
    assert_eq!(a.key(), a.clone().key());
    let mut moved = a.clone();
    build_of_mut(&mut moved).parts[0].min[0] += 1;
    assert_ne!(a.key(), moved.key());
    let _: PrefabKey = a.key();
    assert_eq!(a.element_count(), 3);
}

#[test]
fn prop_files_are_refused_for_now() {
    let prop_file = game::PrefabFile {
        format: PREFAB_FORMAT,
        name: "tree".into(),
        meta: PrefabMeta::default(),
        kind: PrefabKindFile::Prop(PropFile { parts: Vec::new(), slots: Vec::new(), foot: [1, 1], markers: Vec::new() }),
    };
    assert_eq!(resolve(&prop_file, Trust::Builtin), Err(vec![Problem::PropsNotSupported]));
}

#[test]
fn name_rule() {
    for ok in ["a", "dev-arch", "0-9", &"a".repeat(48)] {
        assert!(game::prefab_name_ok(ok), "{ok}");
    }
    for bad in ["", "-a", "A", "a b", "a_b", "ä", &"a".repeat(49), "a/b", "a.b"] {
        assert!(!game::prefab_name_ok(bad), "{bad}");
    }
    assert_eq!(game::prefab_name_key("dev-arch"), crc32fast::hash(b"dev-arch"));
}

#[test]
fn total_box_volume_is_capped() {
    // Two boxes that each fill the whole extent: individually in bounds, together a
    // layout nobody should be made to tile.
    let full = game::BuildBoxFile { min: [-48, -90, -48], size: [96, 270, 96], material: MaterialName::Natural("stone".into()), data: 0 };
    let one = build_file("v", BuildFile { boxes: vec![full.clone()], ..empty_build() });
    assert!(resolve(&one, Trust::Player).is_ok());
    let two = build_file("v", BuildFile { boxes: vec![full.clone(), full], ..empty_build() });
    assert!(matches!(resolve(&two, Trust::Player).unwrap_err()[..], [Problem::BoxesTooLarge(_)]));
    let carve = game::CellBox { min: [-48, -90, -48], size: [96, 270, 96] };
    let mut r = resolved(&build_file("v", BuildFile { carve: vec![carve.clone()], ..empty_build() }));
    build_of_mut(&mut r).carve.push(carve);
    assert!(matches!(r.validate(Trust::Player).unwrap_err()[..], [Problem::BoxesTooLarge(_)]));
}
