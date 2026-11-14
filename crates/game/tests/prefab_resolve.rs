use game::prefab::*;
use game::BlockId;

const STONE: BlockId = BlockId(2);
const OAK: BlockId = BlockId(50);
const GLASS: BlockId = BlockId(60);

fn names(name: &str) -> Option<BlockId> {
    match name {
        "stone" => Some(STONE),
        "oak_planks" => Some(OAK),
        "glass" => Some(GLASS),
        _ => None,
    }
}

fn empty(name: &str, size: [u32; 3]) -> PrefabFile {
    PrefabFile {
        format: PREFAB_FORMAT,
        name: name.into(),
        meta: PrefabMeta::default(),
        size,
        anchor: [0, 0, 0],
        palette: vec!["stone".into(), "oak_planks".into(), "glass".into()],
        boxes: vec![],
        shaped: vec![],
        carve: vec![],
        markers: vec![],
        ground: GroundFit::Flat,
        tags: vec![],
    }
}

fn precedence_file() -> PrefabFile {
    let mut f = empty("prec", [4, 4, 4]);
    f.palette = vec!["stone".into(), "oak_planks".into()];
    f.carve.push(FileCarve { min: [0, 0, 0], size: [4, 4, 4] });
    f.boxes.push(FileBox { min: [0, 0, 0], size: [4, 2, 4], mat: 0 });
    f.boxes.push(FileBox { min: [1, 1, 1], size: [2, 2, 2], mat: 1 });
    f.shaped.push(FileShaped { at: [1, 1, 1], shape: FileShape::Named { shape: Shape::Slab { half: Half::Bottom }, mat: 0 } });
    f
}

fn precedence() -> ResolvedPrefab {
    resolve(&precedence_file(), &names, Trust::Builtin).expect("resolves")
}

#[test]
fn precedence_order() {
    let p = precedence();
    assert_eq!(p.cell([0, 3, 0]), Cell::Carve);
    assert_eq!(p.cell([0, 0, 0]), Cell::Block(STONE));
    assert_eq!(p.cell([2, 1, 2]), Cell::Block(OAK));
    assert!(matches!(p.cell([1, 1, 1]), Cell::Shaped(_)));
    assert_eq!(p.cell([2, 2, 2]), Cell::Block(OAK));
    assert_eq!(p.cell([9, 0, 0]), Cell::Keep);
    assert_eq!(p.cell([-1, 0, 0]), Cell::Keep);
}

fn brute(p: &ResolvedPrefab, min: [i32; 3], max: [i32; 3]) -> Vec<([i32; 3], String)> {
    let mut out = vec![];
    for z in min[2]..max[2] {
        for y in min[1]..max[1] {
            for x in min[0]..max[0] {
                let c = p.cell([x, y, z]);
                if c != Cell::Keep {
                    out.push(([x, y, z], format!("{c:?}")));
                }
            }
        }
    }
    out
}

fn visited(p: &ResolvedPrefab, min: [i32; 3], max: [i32; 3]) -> Vec<([i32; 3], String)> {
    let mut out = vec![];
    p.for_each_cell(min, max, &mut |at, c| out.push((at, format!("{c:?}"))));
    out
}

fn lcg_file() -> PrefabFile {
    let mut f = empty("lcg", [40, 20, 40]);
    let mut s: u64 = 12345;
    let mut next = move |m: u64| {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (s >> 33) % m
    };
    for _ in 0..30 {
        let min = [next(36) as u32, next(16) as u32, next(36) as u32];
        let size = [1 + next(20) as u32, 1 + next(8) as u32, 1 + next(20) as u32];
        let size = [size[0].min(40 - min[0]), size[1].min(20 - min[1]), size[2].min(40 - min[2])];
        f.boxes.push(FileBox { min: min.map(|v| v as i32), size, mat: next(3) as u16 });
    }
    for _ in 0..4 {
        let min = [next(30) as u32, next(15) as u32, next(30) as u32];
        f.carve.push(FileCarve { min: min.map(|v| v as i32), size: [8, 4, 8] });
    }
    for i in 0..10 {
        f.shaped.push(FileShaped {
            at: [3 * i + 1, (i % 7) + 2, 37 - 3 * i],
            shape: FileShape::Named { shape: Shape::Carpet, mat: 2 },
        });
    }
    f
}

#[test]
fn for_each_cell_matches_cell() {
    let small = precedence();
    let big = resolve(&lcg_file(), &names, Trust::Builtin).expect("resolves");
    let ranges: [([i32; 3], [i32; 3]); 6] = [
        ([0, 0, 0], [40, 20, 40]),
        ([16, 0, 16], [32, 16, 32]),
        ([10, 3, 12], [21, 9, 19]),
        ([5, 5, 5], [5, 9, 9]),
        ([-5, -3, -4], [48, 25, 50]),
        ([30, 10, 0], [33, 11, 40]),
    ];
    for (min, max) in ranges {
        assert_eq!(visited(&big, min, max), brute(&big, min.map(|v| v.max(0)), [max[0].min(40), max[1].min(20), max[2].min(40)]));
        assert_eq!(visited(&small, min, max), brute(&small, min.map(|v| v.max(0)), [max[0].min(4), max[1].min(4), max[2].min(4)]));
    }
    assert!(!visited(&big, [0, 0, 0], [40, 20, 40]).is_empty());
}

#[test]
fn column_top_answers() {
    let p = precedence();
    assert_eq!(p.column_top(0, 0), Some((1, STONE)));
    assert_eq!(p.column_top(1, 1), Some((2, OAK)));
    assert_eq!(p.column_top(9, 0), None);
    assert_eq!(p.column_top(-1, 0), None);
    let q = resolve(&empty("none", [3, 3, 3]), &names, Trust::Builtin).unwrap();
    assert_eq!(q.column_top(1, 1), None);
    assert_eq!(q.solid_count(), 0);
}

#[test]
fn shaped_wins_the_column_top_cell() {
    let mut f = empty("s", [2, 4, 2]);
    f.boxes.push(FileBox { min: [0, 0, 0], size: [2, 3, 2], mat: 0 });
    f.shaped.push(FileShaped { at: [0, 2, 0], shape: FileShape::Named { shape: Shape::Carpet, mat: 2 } });
    let p = resolve(&f, &names, Trust::Builtin).unwrap();
    assert_eq!(p.column_top(0, 0), Some((2, GLASS)));
    assert_eq!(p.column_top(1, 0), Some((2, STONE)));
    assert_eq!(p.solid_count(), 12);
}

#[test]
fn reports_every_problem() {
    let mut f = empty("", [4, 4, 4]);
    f.format = 1;
    f.palette = vec!["stone".into(), "nope".into()];
    f.boxes.push(FileBox { min: [0, 0, 0], size: [1, 1, 1], mat: 9 });
    f.boxes.push(FileBox { min: [3, 0, 0], size: [2, 1, 1], mat: 0 });
    f.shaped.push(FileShaped { at: [0, 0, 0], shape: FileShape::Named { shape: Shape::Layer { layers: 0 }, mat: 0 } });
    f.shaped.push(FileShaped { at: [1, 0, 0], shape: FileShape::Named { shape: Shape::Carpet, mat: 0 } });
    f.shaped.push(FileShaped { at: [1, 0, 0], shape: FileShape::Named { shape: Shape::Carpet, mat: 0 } });
    f.markers.push(Marker { at: [0, 0, 0], kind: "door".into(), turn: 7, props: Default::default() });
    let problems = resolve(&f, &names, Trust::Builtin).expect_err("has problems");
    for want in [
        Problem::BadFormat(1),
        Problem::EmptyName,
        Problem::UnknownBlock("nope".into()),
        Problem::BadPaletteIndex { list: "boxes", index: 0 },
        Problem::OutsideSize { list: "boxes", index: 1 },
        Problem::BadShape { index: 0 },
        Problem::DuplicateShaped { at: [1, 0, 0] },
        Problem::BadTurn { index: 0 },
    ] {
        assert!(problems.contains(&want), "missing {want:?} in {problems:?}");
    }
    assert!(problems.iter().all(|p| !p.to_string().is_empty()));
}

#[test]
fn bad_cuboids_and_overflow_are_problems_not_panics() {
    let mut f = empty("c", [4, 4, 4]);
    f.boxes.push(FileBox { min: [i32::MAX, 0, 0], size: [u32::MAX, 1, 1], mat: 0 });
    f.boxes.push(FileBox { min: [-1, 0, 0], size: [1, 1, 1], mat: 0 });
    f.carve.push(FileCarve { min: [i32::MIN, 0, 0], size: [u32::MAX, 1, 1] });
    f.shaped.push(FileShaped { at: [0, 0, 0], shape: FileShape::Cuboids { cuboids: vec![([0, 0, 0], [17, 1, 1], 0)], mats: vec![0] } });
    f.shaped.push(FileShaped { at: [1, 0, 0], shape: FileShape::Cuboids { cuboids: vec![([2, 0, 0], [2, 1, 1], 0)], mats: vec![0] } });
    f.shaped.push(FileShaped { at: [2, 0, 0], shape: FileShape::Cuboids { cuboids: vec![([0, 0, 0], [1, 1, 1], 1)], mats: vec![0] } });
    f.shaped.push(FileShaped { at: [i32::MAX, 0, 0], shape: FileShape::Cuboids { cuboids: vec![], mats: vec![] } });
    let problems = resolve(&f, &names, Trust::Builtin).expect_err("has problems");
    assert!(problems.contains(&Problem::OutsideSize { list: "boxes", index: 0 }));
    assert!(problems.contains(&Problem::OutsideSize { list: "boxes", index: 1 }));
    assert!(problems.contains(&Problem::OutsideSize { list: "carve", index: 0 }));
    assert!(problems.contains(&Problem::OutsideSize { list: "shaped", index: 3 }));
    for index in 0..3 {
        assert!(problems.contains(&Problem::BadShape { index }), "{index}: {problems:?}");
    }
    let mut g = empty("m", [4, 4, 4]);
    g.shaped.push(FileShaped { at: [0, 0, 0], shape: FileShape::Cuboids { cuboids: vec![([0, 0, 0], [1, 1, 1], 0)], mats: vec![0; 257] } });
    assert!(resolve(&g, &names, Trust::Builtin).unwrap_err().contains(&Problem::BadShape { index: 0 }));
}

#[test]
fn untrusted_caps() {
    let wide = empty("w", [257, 1, 1]);
    let err = resolve(&wide, &names, Trust::Untrusted).unwrap_err();
    assert!(err.iter().any(|p| matches!(p, Problem::TooLarge { .. })), "{err:?}");
    assert!(resolve(&wide, &names, Trust::Builtin).is_ok());

    let mut many = empty("m", [256, 1, 256]);
    for i in 0..(UNTRUSTED_MAX_ITEMS as u32 + 1) {
        many.boxes.push(FileBox { min: [(i % 256) as i32, 0, (i / 256) as i32], size: [1, 1, 1], mat: 0 });
    }
    let err = resolve(&many, &names, Trust::Untrusted).unwrap_err();
    assert!(err.iter().any(|p| matches!(p, Problem::TooLarge { what: "items", .. })), "{err:?}");
}

#[test]
fn name_key_is_crc32_of_the_name() {
    assert_eq!(prefab_name_key("hut"), crc32fast::hash(b"hut"));
    assert_eq!(precedence().name_key, prefab_name_key("prec"));
}

#[test]
fn a_monument_resolves_and_answers_locally() {
    let mut f = empty("monument", [600, 200, 600]);
    f.boxes.push(FileBox { min: [0, 0, 0], size: [600, 200, 600], mat: 0 });
    let p = resolve(&f, &names, Trust::Builtin).expect("resolves");
    let mut n = 0;
    p.for_each_cell([100, 50, 100], [116, 66, 116], &mut |_, c| {
        assert_eq!(c, Cell::Block(STONE));
        n += 1;
    });
    assert_eq!(n, 4096);
    assert_eq!(p.solid_count(), 72_000_000);
    assert_eq!(p.column_top(599, 0), Some((199, STONE)));
}

#[test]
fn untrusted_work_budget_refuses_hostile_overlap() {
    let mut f = empty("hostile", [256, 256, 256]);
    for _ in 0..UNTRUSTED_MAX_ITEMS {
        f.boxes.push(FileBox { min: [0, 0, 0], size: [256, 256, 256], mat: 0 });
    }
    let err = resolve(&f, &names, Trust::Untrusted).unwrap_err();
    assert!(err.iter().any(|p| matches!(p, Problem::TooLarge { what: "box overlap", .. })), "{err:?}");
    f.boxes.truncate(5);
    assert!(resolve(&f, &names, Trust::Untrusted).is_ok());
}

#[test]
fn untrusted_column_work_budget() {
    // Few bucket refs (flat boxes) but a lot of column work.
    let mut f = empty("flat", [256, 1, 256]);
    for _ in 0..300 {
        f.boxes.push(FileBox { min: [0, 0, 0], size: [256, 1, 256], mat: 0 });
    }
    let err = resolve(&f, &names, Trust::Untrusted).unwrap_err();
    assert!(err.contains(&Problem::TooLarge { what: "box overlap", limit: UNTRUSTED_MAX_COLUMN_WORK as usize }), "{err:?}");
}

#[test]
fn markers_may_sit_a_margin_outside_the_box() {
    let m = |at: [i32; 3]| Marker { at, kind: "door".into(), turn: 0, props: Default::default() };
    let ok = |at: [i32; 3]| {
        let mut f = empty("m", [4, 4, 4]);
        f.markers.push(m(at));
        resolve(&f, &names, Trust::Builtin).is_ok()
    };
    assert!(ok([-MARKER_MARGIN, 0, 0]));
    assert!(!ok([-MARKER_MARGIN - 1, 0, 0]));
    assert!(ok([0, 4 + MARKER_MARGIN - 1, 0]));
    assert!(!ok([0, 4 + MARKER_MARGIN, 0]));
    assert!(ok([0, 0, 3 + MARKER_MARGIN]));
    assert!(!ok([0, 0, i32::MAX]));
}

#[test]
fn an_empty_cuboid_list_is_a_bad_shape() {
    let mut f = empty("e", [2, 2, 2]);
    f.shaped.push(FileShaped { at: [0, 0, 0], shape: FileShape::Cuboids { cuboids: vec![], mats: vec![0] } });
    assert!(resolve(&f, &names, Trust::Builtin).unwrap_err().contains(&Problem::BadShape { index: 0 }));
}

#[test]
fn solid_count_and_column_top_match_cell() {
    let p = resolve(&lcg_file(), &names, Trust::Builtin).expect("resolves");
    let mut solid = 0u64;
    for x in 0..40 {
        for z in 0..40 {
            let mut top = None;
            for y in 0..20 {
                let mat = match p.cell([x, y, z]) {
                    Cell::Block(m) => Some(m),
                    Cell::Shaped(ResolvedShape::Named { mat, .. }) => Some(*mat),
                    Cell::Shaped(_) => unreachable!(),
                    Cell::Keep | Cell::Carve => None,
                };
                if let Some(m) = mat {
                    solid += 1;
                    top = Some((y, m));
                }
            }
            assert_eq!(p.column_top(x, z), top, "column ({x}, {z})");
        }
    }
    assert_eq!(p.solid_count(), solid);
    assert!(solid > 0);
}

#[test]
fn covered_bucket_keeps_file_order_and_carve_stays_below_boxes() {
    // A later box over an earlier one, inside a fully covered bucket.
    let mut f = empty("cov", [16, 16, 16]);
    f.boxes.push(FileBox { min: [0, 0, 0], size: [16, 16, 16], mat: 0 });
    f.boxes.push(FileBox { min: [4, 4, 4], size: [4, 4, 4], mat: 1 });
    let p = resolve(&f, &names, Trust::Builtin).unwrap();
    assert_eq!(p.cell([5, 5, 5]), Cell::Block(OAK));
    assert_eq!(p.cell([3, 5, 5]), Cell::Block(STONE));
    assert_eq!(p.solid_count(), 4096);
    // The covering box comes last: it hides the earlier one.
    f.boxes.reverse();
    let p = resolve(&f, &names, Trust::Builtin).unwrap();
    assert_eq!(p.cell([5, 5, 5]), Cell::Block(STONE));
    assert_eq!(p.solid_count(), 4096);
    // A carve box over a box does not remove it.
    let mut g = empty("carve", [8, 2, 8]);
    g.boxes.push(FileBox { min: [0, 0, 0], size: [4, 2, 4], mat: 0 });
    g.carve.push(FileCarve { min: [0, 0, 0], size: [8, 2, 8] });
    let p = resolve(&g, &names, Trust::Builtin).unwrap();
    assert_eq!(p.cell([1, 1, 1]), Cell::Block(STONE));
    assert_eq!(p.cell([6, 1, 6]), Cell::Carve);
    assert_eq!(p.solid_count(), 32);
}
