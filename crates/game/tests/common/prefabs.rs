//! Prefab fixtures shared by the `prefab_*` test modules.

use game::{
    BuildBoxFile, BuildFile, BuildPartFile, CellBox, Marker, MaterialName, PrefabFile, PrefabKindFile, PrefabMeta,
    ResolvedBuild, ResolvedKind, ResolvedPrefab, Trust, PREFAB_FORMAT,
};

pub fn part(number: &str, min: [i32; 3], rot: u8) -> BuildPartFile {
    BuildPartFile { min, part: number.into(), rot, material: MaterialName::Plastic(4), data: 0 }
}

pub fn build_file(name: &str, build: BuildFile) -> PrefabFile {
    PrefabFile { format: PREFAB_FORMAT, name: name.into(), meta: PrefabMeta::default(), kind: PrefabKindFile::Build(build) }
}

pub fn empty_build() -> BuildFile {
    BuildFile { parts: Vec::new(), boxes: Vec::new(), carve: Vec::new(), markers: Vec::new() }
}

/// The sample asset `assets/prefabs/dev-arch.prefab.ron`: two 2 x 4 posts turned to
/// stand 2 wide in x (x 0..2 and 6..8, z 0..4) and a 2 x 8 lintel across them, red plastic.
pub fn arch() -> PrefabFile {
    build_file(
        "dev-arch",
        BuildFile {
            parts: vec![part("3001", [0, 0, 0], 1), part("3001", [6, 0, 0], 1), part("3007", [0, 3, 1], 0)],
            ..empty_build()
        },
    )
}

/// A prefab using every Build feature: parts, a natural box, a carve box, a marker.
pub fn rich() -> PrefabFile {
    build_file(
        "rich",
        BuildFile {
            parts: vec![part("3001", [0, 0, 0], 0), part("3001", [0, 3, 0], 1)],
            boxes: vec![BuildBoxFile { min: [-4, 0, 0], size: [4, 3, 2], material: MaterialName::Natural("stone".into()), data: 0 }],
            carve: vec![CellBox { min: [0, 0, 0], size: [1, 1, 1] }],
            markers: vec![Marker { at: [1, 6, 1], kind: "door".into(), turn: 2 }],
        },
    )
}

pub fn resolved(file: &PrefabFile) -> ResolvedPrefab {
    game::resolve(file, Trust::Builtin).expect("fixture resolves")
}

pub fn build_of(p: &ResolvedPrefab) -> &ResolvedBuild {
    let ResolvedKind::Build(b) = &p.kind;
    b
}

pub fn build_of_mut(p: &mut ResolvedPrefab) -> &mut ResolvedBuild {
    let ResolvedKind::Build(b) = &mut p.kind;
    b
}
