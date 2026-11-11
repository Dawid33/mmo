//! A prefab folder on disk: list, load, save, delete, duplicate, import, export.

use crate::common::prefabs::{arch, rich, TempDir};
use game::{prefab_to_export, prefab_to_ron, PrefabDir, PrefabError, PrefabKindFile, PrefabKindTag, Problem};

fn dir(tag: &str) -> (TempDir, PrefabDir) {
    let tmp = TempDir::new(tag);
    let d = PrefabDir::new(tmp.path().join("prefabs"));
    (tmp, d)
}

#[test]
fn save_list_load_round_trip() {
    let (_tmp, d) = dir("round");
    d.save(&arch()).unwrap();
    d.save(&rich()).unwrap();
    let l = d.list();
    assert!(l.bad.is_empty());
    assert_eq!(l.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["dev-arch", "rich"]);
    let e = &l.entries[0];
    assert_eq!((e.kind, e.elements, e.size), (PrefabKindTag::Build, 3, [8, 6, 4]));
    assert_eq!(d.load("dev-arch").unwrap(), arch());
    // The file on disk is the canonical RON, byte for byte.
    assert_eq!(std::fs::read_to_string(d.path_of("dev-arch")).unwrap(), prefab_to_ron(&arch()));
    // Saving again replaces the file and leaves no temp file behind.
    d.save(&arch()).unwrap();
    assert_eq!(std::fs::read_dir(d.root()).unwrap().count(), 2);
}

#[test]
fn missing_folder_lists_empty() {
    let (tmp, _) = dir("missing");
    let l = PrefabDir::new(tmp.path().join("nope")).list();
    assert!(l.entries.is_empty() && l.bad.is_empty());
    assert!(matches!(PrefabDir::new(tmp.path().join("nope")).load("dev-arch"), Err(PrefabError::Io(_))));
}

#[test]
fn junk_and_mismatched_files_are_reported_not_fatal() {
    let (_tmp, d) = dir("junk");
    d.save(&arch()).unwrap();
    std::fs::write(d.root().join("x.prefab.ron"), "junk").unwrap();
    std::fs::write(d.root().join("other.prefab.ron"), prefab_to_ron(&arch())).unwrap();
    std::fs::write(d.root().join("notes.txt"), "not a prefab file at all").unwrap();
    std::fs::create_dir(d.root().join("folder.prefab.ron")).unwrap();
    let l = d.list();
    assert_eq!(l.entries.len(), 1);
    assert_eq!(l.entries[0].name, "dev-arch");
    let bad: Vec<&str> = l.bad.iter().map(|(file, _)| file.as_str()).collect();
    assert_eq!(bad, ["folder.prefab.ron", "other.prefab.ron", "x.prefab.ron"]);
    assert!(l.bad[1].1.contains("dev-arch"), "the mismatch names the prefab inside: {}", l.bad[1].1);
}

#[test]
fn duplicate_picks_free_names() {
    let (_tmp, d) = dir("dup");
    d.save(&arch()).unwrap();
    assert_eq!(d.duplicate("dev-arch").unwrap(), "dev-arch-copy");
    assert_eq!(d.duplicate("dev-arch").unwrap(), "dev-arch-copy-2");
    assert_eq!(d.load("dev-arch-copy-2").unwrap().kind, arch().kind);
    assert_eq!(d.load("dev-arch-copy-2").unwrap().name, "dev-arch-copy-2");
    // A name at the length limit still gets a valid copy name.
    let mut long = arch();
    long.name = "a".repeat(48);
    d.save(&long).unwrap();
    let copy = d.duplicate(&long.name).unwrap();
    assert!(game::prefab_name_ok(&copy) && copy != long.name, "{copy}");
}

#[test]
fn import_export_round_trip_and_name_clash() {
    let (_tmp, d) = dir("import");
    d.save(&arch()).unwrap();
    let s = d.export("dev-arch").unwrap();
    assert_eq!(s, prefab_to_export(&arch()));
    assert_eq!(d.import(&s).unwrap(), "dev-arch-2");
    assert_eq!(d.import(&format!("  {s}\n")).unwrap(), "dev-arch-3");
    assert_eq!(d.load("dev-arch-2").unwrap().kind, arch().kind);
    // Into an empty folder the name is kept.
    let (_tmp2, fresh) = dir("import-fresh");
    assert_eq!(fresh.import(&s).unwrap(), "dev-arch");
    assert_eq!(fresh.load("dev-arch").unwrap(), arch());
}

#[test]
fn import_refuses_an_invalid_prefab() {
    let (_tmp, d) = dir("import-bad");
    d.save(&arch()).unwrap();
    let before = d.list().entries.len();
    let mut bad = arch();
    let PrefabKindFile::Build(b) = &mut bad.kind else { unreachable!() };
    b.parts[0].part = "99999".into();
    let s = prefab_to_export(&bad);
    assert_eq!(d.import(&s), Err(PrefabError::Problems(vec![Problem::UnknownPart("99999".into())])));
    assert!(matches!(d.import("hello"), Err(PrefabError::NotAPrefabString)));
    assert_eq!(d.list().entries.len(), before);
    assert_eq!(std::fs::read_dir(d.root()).unwrap().count(), before);
}

#[test]
fn import_strips_what_a_player_may_not_carry() {
    // A brick `data` tag (what makes a brick part of a tree) does not survive an import.
    let (_tmp, d) = dir("import-data");
    let mut tagged = arch();
    let PrefabKindFile::Build(b) = &mut tagged.kind else { unreachable!() };
    b.parts[0].data = 7;
    let name = d.import(&prefab_to_export(&tagged)).unwrap();
    assert_eq!(d.load(&name).unwrap(), arch());
}

#[test]
fn delete_removes_the_file() {
    let (_tmp, d) = dir("delete");
    d.save(&arch()).unwrap();
    d.delete("dev-arch").unwrap();
    assert!(d.list().entries.is_empty());
    assert!(matches!(d.delete("dev-arch"), Err(PrefabError::Io(_))));
}

#[test]
fn names_cannot_leave_the_folder() {
    let (tmp, d) = dir("escape");
    std::fs::write(tmp.path().join("secret.prefab.ron"), prefab_to_ron(&arch())).unwrap();
    for name in ["../secret", "..", "a/b", "", "Dev-Arch"] {
        assert_eq!(d.load(name), Err(PrefabError::Problems(vec![Problem::BadName])), "{name:?}");
        assert_eq!(d.delete(name), Err(PrefabError::Problems(vec![Problem::BadName])), "{name:?}");
        assert!(d.export(name).is_err() && d.duplicate(name).is_err());
    }
    let mut f = arch();
    f.name = "../secret".into();
    assert_eq!(d.save(&f), Err(PrefabError::Problems(vec![Problem::BadName])));
    assert!(tmp.path().join("secret.prefab.ron").exists());
}

#[test]
fn prop_files_are_listed_without_resolving() {
    let (_tmp, d) = dir("prop");
    let prop = game::PrefabFile {
        format: game::PREFAB_FORMAT,
        name: "tree".into(),
        meta: Default::default(),
        kind: PrefabKindFile::Prop(game::PropFile {
            parts: vec![game::PropPartFile {
                part: "3741".into(),
                pos: [0, 0, 0],
                rot: [0.0, 0.0, 0.0, 1.0],
                colour: game::PropColour::Slot(0),
                solid: false,
            }],
            slots: vec![game::SlotDef { name: "foliage".into(), default: 2 }],
            foot: [2, 3],
            markers: Vec::new(),
        }),
    };
    d.save(&prop).unwrap();
    let l = d.list();
    assert_eq!((l.entries[0].kind, l.entries[0].elements, l.entries[0].size), (PrefabKindTag::Prop, 1, [2, 0, 3]));
    assert_eq!(d.load("tree").unwrap(), prop);
}
