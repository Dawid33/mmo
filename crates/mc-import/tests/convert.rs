use game::prefab::PrefabFile;
use mc_import::convert::{convert as convert_raw, AirMode, ConvertOptions};
use mc_import::Report;
use mc_import::{BlockEntity, BlockVolume, Tag, Table};

use crate::common::vol;

/// `convert` for a volume that is meant to be convertible.
fn convert(v: &BlockVolume, t: &Table, o: &ConvertOptions) -> Option<(PrefabFile, Report)> {
    convert_raw(v, t, o).unwrap()
}

fn opts(air: AirMode) -> ConvertOptions {
    ConvertOptions {
        name: "test".into(),
        air,
        region: None,
        author: "me".into(),
        source: "src".into(),
        licence: "lic".into(),
        source_hash: "abc".into(),
        tags: vec![],
        max_records: None,
    }
}

const BRICK: &str = "minecraft:stone_bricks";

fn s(x: &str) -> Tag {
    Tag::String(x.to_string())
}

fn comp(fields: Vec<(&str, Tag)>) -> Tag {
    Tag::Compound(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn sign_state_volume(data_version: i32, lines: Vec<Tag>) -> BlockVolume {
    let mut v = vol([2, 1, 1], |x, _, _| if x == 0 { BRICK.into() } else { "minecraft:oak_sign[rotation=0]".into() });
    v.data_version = data_version;
    let front = comp(vec![("messages", Tag::List(lines))]);
    v.block_entities =
        vec![BlockEntity { pos: [1, 0, 0], id: "minecraft:sign".into(), nbt: comp(vec![("front_text", front)]) }];
    v
}

/// Regression: 1.21.5+ (data version >= 4325) plain-string sign lines were lost; seen in the IMPORT RUN over the real library: file tgates (data version 4671) reported 5 unparsed sign lines (Task 6 final review of the library run, session 67a33e5e 2026-11-14 19:27), fixed in 9e7a9939
#[test]
fn plain_string_sign_lines_from_1_21_5() {
    let t = Table::builtin();
    let lines = || vec![s("Zeus"), s("  Temple "), s(""), s(r#"{"text":"x"}"#)];
    // From data version 4325 a message may be a plain string.
    let (f, r) = convert(&sign_state_volume(4325, lines()), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.markers[0].props.get("text").map(String::as_str), Some("Zeus Temple x"));
    assert_eq!(r.signs_unparsed, 0);
    // Before it, such a line is unparsed (and counted); the JSON one still reads.
    let (f, r) = convert(&sign_state_volume(4324, lines()), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.markers[0].props.get("text").map(String::as_str), Some("x"));
    assert_eq!(r.signs_unparsed, 2);
    // Compound messages stay unparsed and counted, at any version.
    let (f, r) = convert(&sign_state_volume(4325, vec![comp(vec![("text", s("Hi"))]), s("Ok")]), t, &opts(AirMode::Auto)).unwrap();
    assert_eq!(f.markers[0].props.get("text").map(String::as_str), Some("Ok"));
    assert_eq!(r.signs_unparsed, 1);
}

