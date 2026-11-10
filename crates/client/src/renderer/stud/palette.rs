//! The pure side of the stud UI (no Slint, no ECS): what the build palette
//! lists, what a pick does to the build mode ([`StudBuild`]), the readouts'
//! wording on stud terrain, and the Build tool's material choices.
//! `palette_ui.rs` feeds these into `GameUi` and its callbacks back.
//!
//! - **Parts**: the whole baked catalog (`game::stud_catalog::STUD_PARTS`), in
//!   catalog order — a row's index IS the part index an order carries.
//! - **Materials**: a brick's material is one `u16` (`game::stud_material`): a
//!   natural terrain material by name, or the plastic of an LDraw colour. The
//!   palette lists the buildable naturals ([`natural_materials`]) and every
//!   baked colour ([`plastic_colors`]) as a swatch.
//! - **The fortress Build tool** designates tiles with a voxel `BlockId`
//!   (`DesignationKind::Build`), which the sim turns into a stud material BY
//!   NAME (`game::stud_material_of_block`). So its picker lists the naturals
//!   that have a same-named block ([`build_blocks`]).

use game::stud_catalog::{STUD_COLORS, STUD_PARTS};
use game::{BlockId, TerrainKind, TileBox, MAT_BEDROCK, NATURAL_BASE, STUD_NATURALS};

use super::build::StudBuild;
use super::mesh::StudAssets;

/// A stud is half a metre; a plate a fifth.
const STUD_M: f32 = game::STUD_UNITS as f32 / game::movement::UNITS_PER_METER;

/// "Stone" from "stone", "Glacier ice" from "glacier_ice".
pub fn title_case(name: &str) -> String {
    let spaced = name.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A part's footprint as the palette shows it: studs across × studs deep, and
/// its height in plates ("2 × 4 studs, 3 plates").
pub fn footprint_text(size: [u8; 3]) -> String {
    let plates = if size[1] == 1 { "plate" } else { "plates" };
    format!("{} × {} studs, {} {plates}", size[0], size[2], size[1])
}

/// One palette row per catalog part, in catalog order: its LDraw name (the
/// catalog pads numbers with spaces — collapsed here) and its footprint.
pub fn part_rows() -> Vec<String> {
    STUD_PARTS
        .iter()
        .map(|p| {
            let name = p.name.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("{name} — {}", footprint_text(p.size))
        })
        .collect()
}

/// The natural materials a player can build with, as `(material id, label)`,
/// in table order: every terrain material but bedrock (which cannot be dug
/// out again).
pub fn natural_materials() -> Vec<(u16, String)> {
    STUD_NATURALS
        .iter()
        .enumerate()
        .map(|(i, m)| (NATURAL_BASE + i as u16, title_case(m.name)))
        .filter(|(id, _)| *id != MAT_BEDROCK)
        .collect()
}

/// One plastic: the LDraw colour code (which is its material id), its sRGB
/// colour and opacity, and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlasticColor {
    pub code: u16,
    pub rgba: [u8; 4],
    pub name: String,
}

/// Every baked colour as a swatch, in code order (`STUD_COLORS`' order).
pub fn plastic_colors(assets: &StudAssets) -> Vec<PlasticColor> {
    STUD_COLORS
        .iter()
        .filter_map(|code| assets.colors.get(code))
        .map(|c| PlasticColor { code: c.code, rgba: [c.rgb[0], c.rgb[1], c.rgb[2], c.alpha], name: c.name.replace('_', " ") })
        .collect()
}

/// What a material is called in the palette: the natural's name, else the
/// plastic's colour name.
pub fn material_label(material: u16, assets: &StudAssets) -> String {
    match game::stud_material(material) {
        Some(m) if material >= NATURAL_BASE => title_case(m.name),
        Some(m) => assets.colors.get(&m.color).map_or_else(|| format!("Colour {material}"), |c| c.name.replace('_', " ")),
        None => format!("Material {material}"),
    }
}

/// One thing the palette's widgets (or the build keys) ask of the build mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteAction {
    /// A part row (index into [`part_rows`] = the catalog index).
    PickPart(i32),
    /// The Rotate button: a quarter turn.
    Rotate,
    /// A natural material row (index into [`natural_materials`]).
    PickNatural(i32),
    /// A colour swatch (index into [`plastic_colors`]).
    PickColor(i32),
    /// The palette's Close button: leave build mode.
    Close,
}

impl StudBuild {
    /// Apply a palette pick. An index outside its list changes nothing.
    pub fn apply(&mut self, action: PaletteAction, assets: &StudAssets) {
        let at = |i: i32, len: usize| usize::try_from(i).ok().filter(|&i| i < len);
        match action {
            PaletteAction::PickPart(i) => {
                if let Some(i) = at(i, STUD_PARTS.len()) {
                    self.part = i as u16;
                }
            }
            PaletteAction::Rotate => self.rot = (self.rot + 1) & 3,
            PaletteAction::PickNatural(i) => {
                let naturals = natural_materials();
                if let Some(i) = at(i, naturals.len()) {
                    self.material = naturals[i].0;
                }
            }
            PaletteAction::PickColor(i) => {
                let colors = plastic_colors(assets);
                if let Some(i) = at(i, colors.len()) {
                    self.material = colors[i].code;
                }
            }
            PaletteAction::Close => self.on = false,
        }
    }
}

/// What the palette shows for a build mode: which rows are picked and the
/// selection's one line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaletteView {
    pub open: bool,
    pub part: i32,
    /// "Rotation 90°".
    pub rotation: String,
    /// Index into [`natural_materials`], −1 when a plastic is picked.
    pub natural: i32,
    /// Index into [`plastic_colors`], −1 when a natural is picked.
    pub color: i32,
    /// "Brick 2 x 4 · 90° · Stone".
    pub selection: String,
}

/// The palette's view of `build`.
pub fn palette_view(build: &StudBuild, assets: &StudAssets) -> PaletteView {
    let index = |found: Option<usize>| found.map_or(-1, |i| i as i32);
    let degrees = u32::from(build.rot & 3) * 90;
    let part = STUD_PARTS.get(build.part as usize).map_or(String::new(), |p| p.name.split_whitespace().collect::<Vec<_>>().join(" "));
    PaletteView {
        open: build.on,
        part: build.part as i32,
        rotation: format!("Rotation {degrees}°"),
        natural: index(natural_materials().iter().position(|(id, _)| *id == build.material)),
        color: if build.material < NATURAL_BASE {
            index(plastic_colors(assets).iter().position(|c| c.code == build.material))
        } else {
            -1
        },
        selection: format!("{part} · {degrees}° · {}", material_label(build.material, assets)),
    }
}

/// The Build tool's material choices on stud terrain, as `(block, label)`: the
/// natural materials a designation can name — those with a block of the same
/// registry name that the sim maps back to that very material
/// (`game::stud_material_of_block`; stud wood goes by its drop block). Empty
/// until the block props are installed. Bedrock is left out.
pub fn build_blocks() -> Vec<(BlockId, String)> {
    natural_materials()
        .into_iter()
        .filter_map(|(material, label)| {
            let block = game::block_of_stud_material(material)?;
            (game::stud_material_of_block(block) == material).then_some((block, label))
        })
        .collect()
}

/// The level readout (`slint_overlay::level_text`) for terrain `kind`: levels
/// are TILES — 1 m on voxels, 1.2 m on studs — so on studs the distance to the
/// surface is also given in metres ("Level 53 · 6 below surface (7.2 m)").
pub fn level_text(kind: TerrainKind, level: i32, surface_level: Option<i32>) -> String {
    let metres = |levels: i32| match kind {
        TerrainKind::Voxel => String::new(),
        TerrainKind::Stud => format!(" ({} m)", metres_text(levels as f32 * kind.tile_height_units() / game::movement::UNITS_PER_METER)),
    };
    match surface_level.map(|surface| level - surface) {
        None => format!("Level {level}"),
        Some(0) => format!("Level {level} · surface"),
        Some(d) if d < 0 => format!("Level {level} · {} below surface{}", -d, metres(-d)),
        Some(d) => format!("Level {level} · {d} above surface{}", metres(d)),
    }
}

/// A length in metres without trailing zeros: "7.2", "12", "0.5".
fn metres_text(m: f32) -> String {
    let rounded = (m * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 { format!("{rounded:.0}") } else { format!("{rounded:.1}") }
}

/// The drag readout's size: "x × z × levels" in tiles; on stud terrain, where
/// a level is 1.2 m, followed by the box in metres ("4 × 3 × 2 (4 × 3 × 2.4 m)").
/// A voxel tile is a metre cube, so there the counts are the metres.
pub fn size_text(kind: TerrainKind, area: TileBox) -> String {
    let len = |i: usize| i64::from(area.max[i]) - i64::from(area.min[i]) + 1;
    let tiles = format!("{} × {} × {}", len(0), len(2), len(1));
    match kind {
        TerrainKind::Voxel => tiles,
        TerrainKind::Stud => {
            let (w, h) = (kind.tile_width_units(), kind.tile_height_units());
            let m = |n: i64, unit: f32| metres_text(n as f32 * unit / game::movement::UNITS_PER_METER);
            format!("{tiles} ({} × {} × {} m)", m(len(0), w), m(len(2), w), m(len(1), h))
        }
    }
}

/// A part's footprint in metres, for tooltips and tests: studs are half a metre.
pub fn footprint_metres(size: [u8; 3]) -> [f32; 2] {
    [f32::from(size[0]) * STUD_M, f32::from(size[2]) * STUD_M]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::stud::build::{build_target, BUILD_COLORS};
    use crate::renderer::stud::mesh::stud_assets;
    use crate::renderer::stud::world::StudWorld;
    use bevy::prelude::Entity;
    use game::rapier3d::math::Vector;
    use game::{Order, RegionCoords, StudBrick, StudChunk, StudShape, WorldChunkPos, MAT_DIRT, MAT_STONE, MAT_WOOD, STUD_CHUNK_STUDS};

    #[test]
    fn the_part_list_is_the_catalog_in_order_with_footprints() {
        let rows = part_rows();
        assert_eq!(rows.len(), STUD_PARTS.len());
        let i = STUD_PARTS.iter().position(|p| p.number == "3001").unwrap();
        assert_eq!(rows[i], "Brick 2 x 4 — 4 × 2 studs, 3 plates");
        assert_eq!(footprint_metres(STUD_PARTS[i].size), [2.0, 1.0]);
        let plate = STUD_PARTS.iter().position(|p| p.size[1] == 1).expect("a plate in the catalog");
        assert!(rows[plate].ends_with("1 plate"), "{}", rows[plate]);
        assert!(rows.iter().all(|r| !r.contains("  ")), "the catalog's padding is collapsed");
    }

    #[test]
    fn naturals_are_named_and_bedrock_is_not_offered() {
        let naturals = natural_materials();
        assert_eq!(naturals.len(), STUD_NATURALS.len() - 1);
        assert!(naturals.iter().all(|(id, _)| *id != MAT_BEDROCK));
        assert!(naturals.contains(&(MAT_STONE, "Stone".into())));
        assert!(naturals.iter().any(|(_, name)| name == "Glacier ice"));
        for (id, name) in &naturals {
            assert_eq!(game::stud_material_id(&name.to_lowercase().replace(' ', "_")), Some(*id), "{name} names its material");
        }
    }

    #[test]
    fn every_baked_colour_is_a_swatch_keyed_by_its_material_id() {
        let colors = plastic_colors(stud_assets());
        assert_eq!(colors.len(), STUD_COLORS.len());
        assert!(colors.windows(2).all(|w| w[0].code < w[1].code), "code order");
        let red = colors.iter().find(|c| c.code == 4).unwrap();
        assert_eq!((red.name.as_str(), red.rgba[3]), ("Red", 255));
        let clear = colors.iter().find(|c| c.code == 47).unwrap();
        assert_eq!(clear.name, "Trans Clear");
        assert!(clear.rgba[3] < 255, "a transparent plastic keeps its alpha in the swatch");
        // A swatch's code is a valid material drawn in that very colour.
        for c in &colors {
            assert_eq!(game::stud_material_color(c.code), c.code);
        }
        // The colour key's cycle is a subset of the swatches.
        assert!(BUILD_COLORS.iter().all(|k| colors.iter().any(|c| c.code == *k)));
    }

    #[test]
    fn picks_change_the_build_mode_and_the_view_follows() {
        let assets = stud_assets();
        let mut build = StudBuild { on: true, ..Default::default() };
        let v = palette_view(&build, assets);
        assert!(v.open);
        assert_eq!((v.part, v.natural), (0, -1));
        assert!(v.color >= 0, "the default material is a plastic");
        assert_eq!(v.rotation, "Rotation 0°");

        let part = STUD_PARTS.iter().position(|p| p.number == "3001").unwrap() as i32;
        build.apply(PaletteAction::PickPart(part), assets);
        build.apply(PaletteAction::Rotate, assets);
        let stone = natural_materials().iter().position(|(id, _)| *id == MAT_STONE).unwrap() as i32;
        build.apply(PaletteAction::PickNatural(stone), assets);
        assert_eq!((build.part as i32, build.rot, build.material), (part, 1, MAT_STONE));
        let v = palette_view(&build, assets);
        assert_eq!((v.part, v.natural, v.color), (part, stone, -1));
        assert_eq!(v.selection, "Brick 2 x 4 · 90° · Stone");

        let red = plastic_colors(assets).iter().position(|c| c.code == 4).unwrap() as i32;
        build.apply(PaletteAction::PickColor(red), assets);
        assert_eq!(build.material, 4);
        let v = palette_view(&build, assets);
        assert_eq!((v.natural, v.color), (-1, red));
        assert_eq!(v.selection, "Brick 2 x 4 · 90° · Red");

        // Four quarter turns come back; a bad index is ignored; Close leaves the mode.
        for _ in 0..3 {
            build.apply(PaletteAction::Rotate, assets);
        }
        assert_eq!(build.rot, 0);
        let before = build;
        for bad in [PaletteAction::PickPart(-1), PaletteAction::PickPart(9999), PaletteAction::PickNatural(999), PaletteAction::PickColor(-3)] {
            build.apply(bad, assets);
        }
        assert_eq!((build.part, build.rot, build.material), (before.part, before.rot, before.material));
        build.apply(PaletteAction::Close, assets);
        assert!(!build.on && !palette_view(&build, assets).open);
    }

    /// The palette's model ↔ order round trip: what is picked in the lists is
    /// what a click places — part, rotation and material — and what was placed
    /// reads back as the same rows.
    #[test]
    fn a_palette_selection_is_the_brick_the_click_places() {
        let assets = stud_assets();
        let key = WorldChunkPos::new(0, 0, 0);
        let floor = StudChunk::new([StudBrick::filler([0, 0, 0], [STUD_CHUNK_STUDS as u8, 3, STUD_CHUNK_STUDS as u8], MAT_DIRT)]).unwrap();
        let mut world = StudWorld::default();
        let e = Entity::from_raw_u32(1).unwrap();
        world.set_chunk(e, key, &floor);
        let chunk_of = |en: Entity| (en == e).then_some(&floor);
        let down = |build: &StudBuild| {
            build_target(build, &world, chunk_of, RegionCoords::new(0, 0), Vector::new(100.0, 80.0, 100.0), Vector::new(0.0, -1.0, 0.0))
                .and_then(|t| t.place_order())
        };

        let mut build = StudBuild { on: true, ..Default::default() };
        let parts = part_rows();
        let naturals = natural_materials();
        let colors = plastic_colors(assets);
        // Every natural and a spread of colours, on two parts and every rotation.
        let picks = naturals.iter().enumerate().map(|(i, (id, _))| (PaletteAction::PickNatural(i as i32), *id)).chain(
            colors.iter().enumerate().step_by(37).map(|(i, c)| (PaletteAction::PickColor(i as i32), c.code)),
        );
        for (n, (pick, material)) in picks.enumerate() {
            let part = [0usize, parts.len() - 1, STUD_PARTS.iter().position(|p| p.number == "3001").unwrap()][n % 3];
            build.apply(PaletteAction::PickPart(part as i32), assets);
            build.apply(PaletteAction::Rotate, assets);
            build.apply(pick, assets);
            let Some(Order::PlaceStudAt { chunk, brick }) = down(&build) else { panic!("pick {n}: a placement") };
            assert_eq!(chunk, key);
            assert_eq!(brick.shape, StudShape::Part { part: part as u16, rot: build.rot }, "pick {n}");
            assert_eq!(brick.material, material, "pick {n}");
            // Back from the brick to the rows: the same selection.
            let placed = StudBuild { on: true, part: part as u16, rot: build.rot, material: brick.material };
            assert_eq!(palette_view(&placed, assets), palette_view(&build, assets));
            assert!(game::stud_material(brick.material).is_some(), "a material the sim knows");
        }
    }

    fn install_block_props() {
        let reg = game::BlockRegistry::from_ron(include_str!("../../../../../assets/blocks/blocks.ron")).unwrap();
        game::install_block_props(reg.props());
    }

    #[test]
    fn build_blocks_are_the_naturals_a_block_can_name() {
        install_block_props();
        let blocks = build_blocks();
        assert!(blocks.len() >= 8, "most terrain materials have a block: {blocks:?}");
        let props = game::block_props();
        for (block, label) in &blocks {
            let material = game::stud_material_of_block(*block);
            assert!(material >= NATURAL_BASE && material != MAT_BEDROCK, "{label}");
            assert_eq!(title_case(game::stud_material(material).unwrap().name), *label, "the label is the material's name");
            assert!(props.name_of(*block).is_some());
        }
        let label_of = |m: u16| blocks.iter().find(|(b, _)| game::stud_material_of_block(*b) == m).map(|(_, l)| l.as_str());
        assert_eq!(label_of(MAT_STONE), Some("Stone"));
        assert_eq!(label_of(MAT_DIRT), Some("Dirt"));
        assert_eq!(label_of(MAT_WOOD), Some("Wood"), "wood builds through its drop block");
        // The default build block is among them (dirt): the picker can show it picked.
        assert!(blocks.iter().any(|(b, _)| *b == game::edit::PLACE_BLOCK));
    }

    #[test]
    fn level_text_counts_tiles_and_gives_metres_on_studs() {
        let v = TerrainKind::Voxel;
        assert_eq!(level_text(v, 64, Some(70)), "Level 64 · 6 below surface");
        assert_eq!(level_text(v, 72, Some(70)), "Level 72 · 2 above surface");
        assert_eq!(level_text(v, 70, Some(70)), "Level 70 · surface");
        assert_eq!(level_text(v, 70, None), "Level 70");
        let s = TerrainKind::Stud;
        assert_eq!(level_text(s, 52, Some(58)), "Level 52 · 6 below surface (7.2 m)");
        assert_eq!(level_text(s, 63, Some(58)), "Level 63 · 5 above surface (6 m)");
        assert_eq!(level_text(s, 58, Some(58)), "Level 58 · surface");
        assert_eq!(level_text(s, 58, None), "Level 58");
        // The surface level itself comes through `TerrainKind`: the same ground is
        // level 70 on voxels and 58 on studs.
        use crate::renderer::commander::level::surface_level;
        let y = 70.0 * game::movement::UNITS_PER_METER;
        assert_eq!((surface_level(v, y), surface_level(s, y)), (70, 58));
        assert_eq!(level_text(s, surface_level(s, y) - 1, Some(surface_level(s, y))), "Level 57 · 1 below surface (1.2 m)");
    }

    #[test]
    fn the_drag_size_is_tiles_and_on_studs_also_metres() {
        let area = TileBox::new([2, 10, 5], [5, 11, 7]);
        assert_eq!(size_text(TerrainKind::Voxel, area), "4 × 3 × 2");
        assert_eq!(size_text(TerrainKind::Stud, area), "4 × 3 × 2 (4 × 3 × 2.4 m)");
        let one = TileBox::new([0, 0, 0], [0, 0, 0]);
        assert_eq!(size_text(TerrainKind::Stud, one), "1 × 1 × 1 (1 × 1 × 1.2 m)");
        let tall = TileBox::new([0, 0, 0], [0, 4, 0]);
        assert_eq!(size_text(TerrainKind::Stud, tall), "1 × 1 × 5 (1 × 1 × 6 m)");
    }
}
