//! The stud UI's Slint side: the build palette (`ui/stud-build.slint`) shown
//! while the stud build mode is on, and the fortress Build tool's material
//! list in the context box (`ui/menu-column.slint`) on stud terrain.
//!
//! Like the rest of the game UI (`slint_overlay.rs`): models are pushed from
//! client state only when they change, and Slint callbacks queue
//! [`StudUiAction`]s that a system applies in `Update`. Everything decided
//! here is in `palette.rs` (pure, tested); this file only moves it across.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::prelude::*;
use game::TerrainKind;
use slint_ui::slint::{Color as SlintColor, Model, ModelRc, SharedString, VecModel};
use slint_ui::{GameUi, StudSwatch};

use super::build::StudBuild;
use super::mesh::stud_assets;
use super::palette::{build_blocks, natural_materials, palette_view, part_rows, plastic_colors, PaletteAction, PaletteView};
use crate::renderer::commander::designate::{DesignateState, Tool};
use crate::renderer::commander::tiles::TileMirror;
use crate::renderer::slint_overlay::SlintLayer;

/// What a stud UI callback asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudUiAction {
    /// A pick in the build palette.
    Palette(PaletteAction),
    /// A row of the Build tool's material list (index into [`build_blocks`]).
    BuildMaterial(i32),
}

/// The stud UI's queued callbacks (main thread, like the UI itself).
pub struct StudUiActions(Rc<RefCell<Vec<StudUiAction>>>);

fn strings(lines: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    let lines: Vec<SharedString> = lines.into_iter().map(SharedString::from).collect();
    ModelRc::new(VecModel::from(lines))
}

/// Fill the palette's fixed lists (the catalog, the natural materials, the
/// colours — none changes while the game runs) and connect its callbacks.
/// Called on every (re)built `GameUi` (`slint_overlay::connect_game_ui`).
pub fn connect(ui: &GameUi, world: &mut World) {
    let assets = stud_assets();
    ui.set_stud_parts(strings(part_rows()));
    ui.set_stud_naturals(strings(natural_materials().into_iter().map(|(_, name)| name)));
    let swatches: Vec<StudSwatch> = plastic_colors(assets)
        .into_iter()
        .map(|c| StudSwatch { color: SlintColor::from_rgb_u8(c.rgba[0], c.rgba[1], c.rgba[2]), name: c.name.into() })
        .collect();
    ui.set_stud_colors(ModelRc::new(VecModel::from(swatches)));

    let actions = Rc::new(RefCell::new(Vec::new()));
    let push = |make: fn(i32) -> StudUiAction| {
        let queue = actions.clone();
        move |i: i32| queue.borrow_mut().push(make(i))
    };
    ui.on_stud_pick_part(push(|i| StudUiAction::Palette(PaletteAction::PickPart(i))));
    ui.on_stud_pick_natural(push(|i| StudUiAction::Palette(PaletteAction::PickNatural(i))));
    ui.on_stud_pick_color(push(|i| StudUiAction::Palette(PaletteAction::PickColor(i))));
    ui.on_pick_build_material(push(StudUiAction::BuildMaterial));
    let queue = actions.clone();
    ui.on_stud_rotate(move || queue.borrow_mut().push(StudUiAction::Palette(PaletteAction::Rotate)));
    let queue = actions.clone();
    ui.on_stud_close(move || queue.borrow_mut().push(StudUiAction::Palette(PaletteAction::Close)));
    world.insert_non_send(StudUiActions(actions));
}

/// Apply one queued action to the build mode / the Build tool.
pub fn apply_action(action: StudUiAction, build: &mut StudBuild, designate: Option<&mut DesignateState>) {
    match action {
        StudUiAction::Palette(pick) => build.apply(pick, stud_assets()),
        StudUiAction::BuildMaterial(i) => {
            let block = usize::try_from(i).ok().and_then(|i| build_blocks().get(i).map(|(block, _)| *block));
            if let (Some(block), Some(state)) = (block, designate) {
                state.build = block;
            }
        }
    }
}

/// Queued stud UI callbacks → the build mode and the Build tool's material.
pub fn apply_stud_ui_actions(
    actions: Option<NonSend<StudUiActions>>,
    mut build: ResMut<StudBuild>,
    mut designate: Option<ResMut<DesignateState>>,
) {
    let Some(actions) = actions else { return };
    let queued: Vec<StudUiAction> = actions.0.borrow_mut().drain(..).collect();
    for action in queued {
        apply_action(action, &mut build, designate.as_deref_mut());
    }
}

/// The Build tool's material list for the context box: the choices' labels and
/// the picked row — only with the Build tool up on stud terrain (on voxel
/// terrain the tool builds its one construction block, as before).
pub fn build_material_view(tool: Option<Tool>, terrain: TerrainKind, build: game::BlockId) -> (Vec<String>, i32) {
    if tool != Some(Tool::Build) || terrain != TerrainKind::Stud {
        return (Vec::new(), -1);
    }
    let blocks = build_blocks();
    let picked = blocks.iter().position(|(b, _)| *b == build).map_or(-1, |i| i as i32);
    (blocks.into_iter().map(|(_, label)| label).collect(), picked)
}

/// What [`sync_stud_ui`] last pushed.
#[derive(Default)]
pub struct StudUiShown {
    palette: Option<PaletteView>,
    materials: Option<(Option<Tool>, TerrainKind, game::BlockId)>,
}

/// The build mode and the Build tool's material → the UI, each only when it
/// changed (or the UI was rebuilt and lost them).
pub fn sync_stud_ui(
    slint: Option<NonSend<SlintLayer<GameUi>>>,
    build: Res<StudBuild>,
    designate: Option<Res<DesignateState>>,
    mirror: Option<Res<TileMirror>>,
    mut shown: crate::session::SessionLocal<StudUiShown>,
) {
    let Some(slint) = slint else { return };
    let ui = slint.overlay.ui();
    let shown = shown.get();

    // A rebuilt UI starts with no selection text: push again.
    let fresh = ui.get_stud_selection().is_empty();
    if build.is_changed() || shown.palette.is_none() || fresh {
        let view = palette_view(&build, stud_assets());
        if shown.palette.as_ref() != Some(&view) || fresh {
            ui.set_stud_build_open(view.open);
            ui.set_stud_part(view.part);
            ui.set_stud_rotation(view.rotation.as_str().into());
            ui.set_stud_natural(view.natural);
            ui.set_stud_color(view.color);
            ui.set_stud_selection(view.selection.as_str().into());
            shown.palette = Some(view);
        }
    }

    let tool = designate.as_ref().and_then(|d| d.tool);
    let terrain = mirror.as_ref().map_or(TerrainKind::Stud, |m| m.terrain());
    let block = designate.as_ref().map_or(game::edit::PLACE_BLOCK, |d| d.build);
    let key = (tool, terrain, block);
    let wanted = tool == Some(Tool::Build) && terrain == TerrainKind::Stud;
    // Also when a rebuilt UI lost the list.
    if shown.materials != Some(key) || (wanted && ui.get_build_materials().row_count() == 0) {
        let (labels, picked) = build_material_view(tool, terrain, block);
        // Still empty before the block props are installed: try again then.
        let ready = !wanted || !labels.is_empty();
        ui.set_build_materials(strings(labels));
        ui.set_build_material(picked);
        shown.materials = ready.then_some(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::commander::designate::order_for_build;
    use game::{
        ColumnCoords, DesignationKind, GameEventKind, OpId, Order, Region, RegionCoords, Rollback, StudColumnGround, StudProfile,
        StudRegionSeed, TileBox, TileSource, MAT_DIRT, MAT_GRASS, MAT_STONE, STUD_COLUMN_FOOTPRINT, STUD_NO_WATER,
    };

    fn install_block_props() {
        let reg = game::BlockRegistry::from_ron(include_str!("../../../../../assets/blocks/blocks.ron")).unwrap();
        game::install_block_props(reg.props());
    }

    #[test]
    fn the_material_list_shows_only_for_the_build_tool_on_stud_terrain() {
        install_block_props();
        let dirt = game::edit::PLACE_BLOCK;
        let (labels, picked) = build_material_view(Some(Tool::Build), TerrainKind::Stud, dirt);
        assert!(labels.len() >= 8 && labels.contains(&"Stone".to_string()));
        assert_eq!(labels[picked as usize], "Dirt", "the default build block is dirt, shown picked");
        assert_eq!(build_material_view(Some(Tool::Build), TerrainKind::Voxel, dirt), (Vec::new(), -1));
        assert_eq!(build_material_view(Some(Tool::Dig), TerrainKind::Stud, dirt), (Vec::new(), -1));
        assert_eq!(build_material_view(None, TerrainKind::Stud, dirt), (Vec::new(), -1));
    }

    #[test]
    fn picks_reach_the_build_mode_and_the_build_tool() {
        install_block_props();
        let mut build = StudBuild { on: true, ..Default::default() };
        let mut state = DesignateState { tool: Some(Tool::Build), ..Default::default() };
        apply_action(StudUiAction::Palette(PaletteAction::Rotate), &mut build, Some(&mut state));
        assert_eq!(build.rot, 1);
        let (labels, _) = build_material_view(state.tool, TerrainKind::Stud, state.build);
        let stone = labels.iter().position(|l| l == "Stone").unwrap() as i32;
        apply_action(StudUiAction::BuildMaterial(stone), &mut build, Some(&mut state));
        assert_eq!(game::stud_material_of_block(state.build), MAT_STONE);
        assert_eq!(build_material_view(state.tool, TerrainKind::Stud, state.build).1, stone, "and the list shows it picked");
        // Out of range, or no commander state: nothing happens.
        let before = state.build;
        apply_action(StudUiAction::BuildMaterial(9999), &mut build, Some(&mut state));
        apply_action(StudUiAction::BuildMaterial(-1), &mut build, Some(&mut state));
        assert_eq!(state.build, before);
        apply_action(StudUiAction::BuildMaterial(stone), &mut build, None);
    }

    const TOP: i32 = 30;
    const CLIENT: game::ClientId = 7;

    /// A flat one-chunk-column stud region: grass over dirt over stone, top at plate 30.
    fn stud_region() -> Region {
        let recipe = StudColumnGround::new(
            vec![TOP; STUD_COLUMN_FOOTPRINT].into(),
            vec![STUD_NO_WATER; STUD_COLUMN_FOOTPRINT].into(),
            vec![0; STUD_COLUMN_FOOTPRINT].into(),
            vec![StudProfile { cover: MAT_GRASS, cover_plates: 1, soil: MAT_DIRT, soil_plates: 5 }],
            vec![0; STUD_COLUMN_FOOTPRINT].into(),
            vec![(i32::MAX, MAT_STONE)],
            -100,
        )
        .unwrap();
        let mut r = Region::new(Rollback::new(None), None, RegionCoords::new(0, 0), None);
        r.seed_stud_world(StudRegionSeed { ground: [(ColumnCoords::new(0, 0), recipe)].into_iter().collect(), features: Vec::new() });
        r
    }

    /// The Build tool's material, picked in the list, reaches the SIM as that
    /// stud material by name: the tool's order names the picked natural's
    /// block, and the region builds the tile out of the same-named material.
    #[test]
    fn the_build_tool_builds_the_picked_material_in_the_sim() {
        install_block_props();
        let mut r = stud_region();
        assert_eq!(r.data().terrain_kind(), TerrainKind::Stud);
        // The air tile on the ground (plate 30 = the bottom of tile level 5).
        let level = TOP / game::TILE_PLATES as i32;
        let (labels, _) = build_material_view(Some(Tool::Build), TerrainKind::Stud, game::edit::PLACE_BLOCK);
        for (n, name) in ["Sandstone", "Stone", "Dirt", "Wood"].into_iter().enumerate() {
            let Some(row) = labels.iter().position(|l| l == name) else { panic!("{name} is offered: {labels:?}") };
            let mut build = StudBuild::default();
            let mut state = DesignateState { tool: Some(Tool::Build), ..Default::default() };
            apply_action(StudUiAction::BuildMaterial(row as i32), &mut build, Some(&mut state));
            let tile = [2 + 2 * n as i32, level, 3];
            let op = OpId { client: CLIENT, seq: n as u32 };
            let order = order_for_build(Tool::Build, state.build, TileBox::new(tile, tile), false, op, state.priority);
            assert!(matches!(order, Order::Designate { kind: DesignationKind::Build(b), .. } if b == state.build));
            r.handle_event(GameEventKind::Order(CLIENT, order)).unwrap();
            r.handle_event(GameEventKind::Order(CLIENT, Order::DevCompleteAll)).unwrap();
            let want = game::stud_material_id(&name.to_lowercase()).unwrap();
            assert_eq!(r.data().tile_material(tile), Some(want), "{name} built as the stud material of that name");
            assert!(r.data().is_constructed(tile));
        }
        // The default (nothing picked) is dirt, as before.
        let state = DesignateState::default();
        assert_eq!(game::stud_material_of_block(state.build), MAT_DIRT);
    }
}
