//! Designation tools: DF-style dig / deconstruct / wall / floor / erase
//! orders painted onto the home region's tiles at the working level.
//!
//! A tool key ([`TOOL_KEYS`]) picks a tool; the tile under the cursor at the
//! working level is the hover ([`pick_level_cell`]). A left drag spans a box
//! from the tile it started on to the hovered column at the CURRENT working
//! level — Alt+wheel steps the level mid-drag, so one drag covers several
//! levels — and its release sends the box as a sim [`Order`] (Ctrl held: an
//! erase of the tool's kind). While it is held, [`DragPreview`] carries the
//! box and each tile's validity for the overlay, and [`DragReadout`] the
//! size-and-reasons line. Esc or a right click (not a right drag, which still
//! orbits) drops the drag, or with none the tool; Ctrl+Z cancels the last
//! order sent; 1–7 set the priority while a tool is active.
//!
//! Gizmos draw the drag box (bright where visible, dimmed through terrain),
//! the hover cell, and the build designations (walls and floors sit in air,
//! where the overlay has no face to tint) as merged ghost cuboids.
//!
//! Legacy regions only: in cell mode the tile mirror has no home region, so
//! there is nothing to pick and no tool can be chosen.

use std::collections::BTreeMap;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::rapier3d::math::Vector;
use game::{
    pick_level_cell, resolve_drag, validate, BlockId, Designation, DesignationKind, DragShape, Invalid,
    OpId, Order, OrderSummary, RegionId, TileBox, TilePos, TileSource, BLOCK_VOXELS, DEFAULT_PRIORITY,
    DWARF_HEIGHT_LEVELS, MAX_ORDER_TILES, MAX_PRIORITY, REGION_BLOCKS,
};

use super::level::WorkingLevel;
use super::minimap::{self, MinimapPointer};
use super::selection::DRAG_THRESHOLD_PX;
use super::tiles::TileMirror;
use super::CommanderView;
use crate::renderer::cursor::CursorPolicy;
use crate::renderer::fortress_view::{overlay_base_level, DragPreview};
use crate::renderer::{GameEvents, LocalPlayer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Dig,
    Deconstruct,
    Wall,
    Floor,
    Erase,
}

/// Tool hotkeys (bare, no Ctrl — Ctrl+Z is undo). Kept off the camera's
/// WASD / arrows / Space / Shift.
pub const TOOL_KEYS: [(KeyCode, Tool); 5] = [
    (KeyCode::KeyG, Tool::Dig),
    (KeyCode::KeyX, Tool::Deconstruct),
    (KeyCode::KeyB, Tool::Wall),
    (KeyCode::KeyV, Tool::Floor),
    (KeyCode::KeyZ, Tool::Erase),
];

/// Priority keys, `PRIORITY_KEYS[i]` = priority `i + 1`; read only while a
/// tool is active (nothing else binds digits in commander mode).
pub const PRIORITY_KEYS: [KeyCode; MAX_PRIORITY as usize] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
];

/// At most this many merged build-ghost cuboids are drawn per frame.
pub const MAX_GHOST_BOXES: usize = 4096;

/// How much the hover outline is grown past its cell, in sim units, so it
/// does not z-fight the block faces.
const HOVER_INFLATE: f32 = 0.25;

const DIG_COLOR: Color = Color::srgb(1.0, 0.72, 0.2);
const DECONSTRUCT_COLOR: Color = Color::srgb(1.0, 0.3, 0.25);
const BUILD_COLOR: Color = Color::srgb(0.6, 0.8, 1.0);
const ERASE_COLOR: Color = Color::srgb(0.9, 0.9, 0.9);
const TOO_LARGE_COLOR: Color = Color::srgb(1.0, 0.1, 0.1);
const HOVER_COLOR: Color = Color::srgba(1.0, 1.0, 1.0, 0.8);
/// Alpha of the drag box's edges drawn through terrain.
const HIDDEN_EDGE_ALPHA: f32 = 0.3;

impl Tool {
    /// What the tool designates; `None` for the erase tool.
    pub fn kind(self) -> Option<DesignationKind> {
        match self {
            Tool::Dig => Some(DesignationKind::Dig),
            Tool::Deconstruct => Some(DesignationKind::Deconstruct),
            Tool::Wall | Tool::Floor => Some(DesignationKind::Build(game::edit::PLACE_BLOCK)),
            Tool::Erase => None,
        }
    }

    pub fn shape(self) -> DragShape {
        match self {
            Tool::Wall => DragShape::Wall,
            Tool::Floor => DragShape::Floor,
            Tool::Dig | Tool::Deconstruct | Tool::Erase => DragShape::Box,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Dig => "Dig",
            Tool::Deconstruct => "Deconstruct",
            Tool::Wall => "Wall",
            Tool::Floor => "Floor",
            Tool::Erase => "Erase",
        }
    }

    /// The hotkey as shown on the tool bar.
    pub fn key_label(self) -> &'static str {
        match self {
            Tool::Dig => "G",
            Tool::Deconstruct => "X",
            Tool::Wall => "B",
            Tool::Floor => "V",
            Tool::Erase => "Z",
        }
    }
}

/// A left drag in progress: the tile it started on, the tile it reaches now
/// (its `y` the working level), and whether it erases (Ctrl held).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drag {
    pub anchor: TilePos,
    pub corner: TilePos,
    pub erase: bool,
}

/// The designation tools' client-local state.
#[derive(Resource, Debug)]
pub struct DesignateState {
    pub tool: Option<Tool>,
    pub priority: u8,
    pub drag: Option<Drag>,
    /// The tile under the cursor at the working level (region-local).
    pub hover: Option<TilePos>,
    /// Sequence number of this client's next `Designate` order.
    pub next_seq: u32,
    /// Designate orders sent, oldest first; Ctrl+Z cancels the last.
    pub sent: Vec<OpId>,
}

impl Default for DesignateState {
    fn default() -> Self {
        Self { tool: None, priority: DEFAULT_PRIORITY, drag: None, hover: None, next_seq: 0, sent: Vec::new() }
    }
}

impl DesignateState {
    /// Esc / right click: drop the drag if there is one, else the tool.
    pub fn cancel(&mut self) {
        if self.drag.take().is_none() {
            self.tool = None;
        }
    }

    /// The box the drag covers and whether it erases, with the tool.
    fn drag_area(&self) -> Option<(Tool, TileBox, bool)> {
        let (tool, drag) = (self.tool?, self.drag?);
        Some((tool, resolve_drag(tool.shape(), drag.anchor, drag.corner), drag.erase))
    }
}

/// The drag's size-and-reasons line for the readout ("" when not dragging).
#[derive(Resource, Default, Debug)]
pub struct DragReadout(pub String);

/// Gizmo group for the drag box's edges hidden behind terrain: drawn in
/// front of everything, dimmed.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct HiddenEdgeGizmos;

/// Run condition: entity selection is off while a tool is active.
pub fn no_tool_active(state: Res<DesignateState>) -> bool {
    state.tool.is_none()
}

/// The order a released drag sends: an erase for the erase tool or with
/// Ctrl (of the tool's kind only), otherwise the tool's designation.
pub fn order_for(tool: Tool, area: TileBox, erase: bool, op: OpId, priority: u8) -> Order {
    match tool.kind() {
        Some(kind) if !erase => Order::Designate { op, kind, priority, area },
        only => Order::Erase { area, only },
    }
}

/// The tool a key press selects: none while Ctrl is held (Ctrl+Z is undo).
pub fn tool_for_key(key: KeyCode, ctrl: bool) -> Option<Tool> {
    if ctrl {
        return None;
    }
    TOOL_KEYS.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
}

/// Merge tiles into boxes of x-runs: consecutive tiles on one `(y, z)` row
/// with consecutive `x` become one box. Expects [`TileBox::tiles`] order.
pub fn merge_runs(tiles: impl Iterator<Item = TilePos>) -> Vec<TileBox> {
    let mut out: Vec<TileBox> = Vec::new();
    for p in tiles {
        match out.last_mut() {
            Some(b) if b.max[1] == p[1] && b.max[2] == p[2] && b.max[0] + 1 == p[0] => b.max[0] = p[0],
            _ => out.push(TileBox { min: p, max: p }),
        }
    }
    out
}

/// "x × z × levels" of a box.
fn size_text(area: TileBox) -> String {
    let len = |i: usize| i64::from(area.max[i]) - i64::from(area.min[i]) + 1;
    format!("{} × {} × {}", len(0), len(2), len(1))
}

fn verb(kind: Option<DesignationKind>) -> &'static str {
    match kind {
        Some(DesignationKind::Dig) => "to dig",
        Some(DesignationKind::Deconstruct) => "to deconstruct",
        Some(DesignationKind::Build(_)) => "to build",
        None => "to erase",
    }
}

fn reason(why: Invalid) -> &'static str {
    match why {
        Invalid::Bedrock => "bedrock",
        Invalid::NothingToDig => "nothing to dig",
        Invalid::Built => "built",
        Invalid::RegionEdge => "region edge",
        Invalid::Occupied => "occupied",
        Invalid::NotBuilt => "not built",
    }
}

/// The drag readout: size (x × z × levels), the tiles the order takes, then
/// each refusal reason with its count, in [`Invalid`] order.
pub fn drag_text(area: TileBox, kind: Option<DesignationKind>, summary: &OrderSummary) -> String {
    let mut text = format!("{} — {} {}", size_text(area), summary.valid, verb(kind));
    for (why, n) in &summary.invalid {
        text.push_str(&format!(" · {n} {}", reason(*why)));
    }
    text
}

/// `game::summarize` over already-validated tiles.
fn summary_of(cells: &[(TilePos, Result<(), Invalid>)]) -> OrderSummary {
    let mut out = OrderSummary::default();
    for (_, result) in cells {
        match result {
            Ok(()) => out.valid += 1,
            Err(why) => *out.invalid.entry(*why).or_default() += 1,
        }
    }
    out
}

fn too_large_text(area: TileBox) -> String {
    format!("{} — too large", size_text(area))
}

/// Erase previews: how many designations the erase would remove.
fn erase_summary(
    designations: &BTreeMap<TilePos, Designation>,
    area: TileBox,
    only: Option<DesignationKind>,
) -> OrderSummary {
    let valid = designations
        .iter()
        .filter(|(p, d)| area.contains(**p) && only.is_none_or(|k| k.same_tool(d.kind)))
        .count() as u64;
    OrderSummary { valid, invalid: BTreeMap::new() }
}

/// Keep a picked tile's column inside the home region.
/// The drag corner this frame: x/z from the hovered tile while there is
/// one, y the working level whenever it is known — a level step mid-drag
/// lands even while the pointer is off the terrain (e.g. over the readout).
pub fn follow_corner(corner: TilePos, hover: Option<TilePos>, level: Option<i32>) -> TilePos {
    let [mut x, mut y, mut z] = corner;
    if let Some(h) = hover {
        (x, z) = (h[0], h[2]);
    }
    if let Some(level) = level {
        y = level;
    }
    [x, y, z]
}

fn clamp_to_region(p: TilePos) -> TilePos {
    [p[0].clamp(0, REGION_BLOCKS - 1), p[1], p[2].clamp(0, REGION_BLOCKS - 1)]
}

fn ctrl_held(keys: &ButtonInput<KeyCode>) -> bool {
    keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])
}

/// Tool keys, priority keys, Esc and Ctrl+Z.
pub fn designate_keys(
    keys: Res<ButtonInput<KeyCode>>,
    policy: Res<CursorPolicy>,
    mirror: Res<TileMirror>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
    mut state: ResMut<DesignateState>,
) {
    if policy.overlay_open {
        return;
    }
    let ctrl = ctrl_held(&keys);
    if ctrl && keys.just_pressed(KeyCode::KeyZ) {
        if let (Some(op), Some(client)) = (state.sent.last().copied(), local.0) {
            state.sent.pop();
            let _ = events.0.send(game::GameEventKind::Order(client, Order::CancelOp(op)));
        }
    }
    if keys.just_pressed(KeyCode::Escape) {
        state.cancel();
    }
    // No home region (cell mode, or not joined yet): no tools.
    if mirror.region().is_none() {
        if state.tool.is_some() {
            state.tool = None;
            state.drag = None;
        }
        return;
    }
    for key in keys.get_just_pressed() {
        if let Some(tool) = tool_for_key(*key, ctrl) {
            if state.tool != Some(tool) {
                state.tool = Some(tool);
                state.drag = None;
            }
        }
    }
    if state.tool.is_some() {
        if let Some(i) = PRIORITY_KEYS.iter().position(|k| keys.just_pressed(*k)) {
            state.priority = i as u8 + 1;
        }
    }
}

/// Cursor ray → the tile at the working level (region-local).
pub fn update_hover(
    window: Query<&Window, With<PrimaryWindow>>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
    policy: Res<CursorPolicy>,
    mirror: Res<TileMirror>,
    working: Res<WorkingLevel>,
    mut state: ResMut<DesignateState>,
) {
    let hover = (|| {
        state.tool?;
        if policy.overlay_open || policy.ui_pointer {
            return None;
        }
        let (region, level) = (mirror.region()?, working.level?);
        let cursor = window.single().ok()?.cursor_position()?;
        let (camera, tf) = view.single().ok()?;
        let ray = camera.viewport_to_world(&GlobalTransform::from(*tf), cursor).ok()?;
        let origin = ray.origin - Vec3::from_array(region.world_offset());
        let dir = *ray.direction;
        let p = pick_level_cell(
            Vector::new(origin.x, origin.y, origin.z),
            Vector::new(dir.x, dir.y, dir.z),
            level,
            |p| mirror.block(p) != BlockId::AIR,
        )?;
        Some(clamp_to_region(p))
    })();
    if state.hover != hover {
        state.hover = hover;
    }
}

/// Left press / hold / release → drags and orders; a right click cancels.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn drag_input(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    motion: Res<AccumulatedMouseMotion>,
    window: Query<&Window, With<PrimaryWindow>>,
    policy: Res<CursorPolicy>,
    minimap_pointer: Res<MinimapPointer>,
    working: Res<WorkingLevel>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
    mut state: ResMut<DesignateState>,
    // Mouse travel since the right press; `None` while the button is up or
    // the press was not ours (UI / overlay).
    mut right_travel: Local<Option<f32>>,
) {
    let Ok(window) = window.single() else { return };
    // A right press is a click unless the mouse travels: orbiting holds the
    // pointer still, so the travel is the raw motion, not the cursor.
    if buttons.just_pressed(MouseButton::Right) {
        *right_travel = (!policy.overlay_open && !policy.ui_pointer).then_some(0.0);
    }
    if let Some(travel) = right_travel.as_mut() {
        *travel += motion.delta.length();
    }
    if buttons.just_released(MouseButton::Right) {
        if right_travel.take().is_some_and(|t| t <= DRAG_THRESHOLD_PX) {
            state.cancel();
        }
    }

    let Some(tool) = state.tool else { return };
    let ctrl = ctrl_held(&keys);
    if buttons.just_pressed(MouseButton::Left) {
        let over_minimap =
            window.cursor_position().is_some_and(|c| minimap::screen_rect(window.size()).contains(c));
        let blocked = policy.overlay_open || policy.ui_pointer || minimap_pointer.dragging || over_minimap;
        if let (false, Some(at)) = (blocked, state.hover) {
            state.drag = Some(Drag { anchor: at, corner: at, erase: ctrl });
        }
    }
    let Some(mut drag) = state.drag else { return };
    drag.corner = follow_corner(drag.corner, state.hover, working.level);
    drag.erase = ctrl;
    if state.drag != Some(drag) {
        state.drag = Some(drag);
    }
    if !buttons.just_released(MouseButton::Left) {
        if !buttons.pressed(MouseButton::Left) {
            // The release went elsewhere (focus loss): drop the drag.
            state.drag = None;
        }
        return;
    }
    state.drag = None;
    let area = resolve_drag(tool.shape(), drag.anchor, drag.corner);
    let fits = area.clip_to_region().is_some_and(|a| a.tile_count() <= MAX_ORDER_TILES);
    let Some(client) = local.0 else { return };
    if !fits {
        return;
    }
    let op = OpId { client, seq: state.next_seq };
    let order = order_for(tool, area, drag.erase, op, state.priority);
    if matches!(order, Order::Designate { .. }) {
        state.next_seq += 1;
        state.sent.push(op);
    }
    let _ = events.0.send(game::GameEventKind::Order(client, order));
}

/// What [`update_drag_preview`] last built, so it rebuilds only on change.
#[derive(Default)]
pub struct PreviewBuilt {
    key: Option<(TileBox, Option<DesignationKind>, bool, u64)>,
}

/// The drag → [`DragPreview`] (box, kind, per-tile validity) and the readout.
/// Must run before `fortress_view::update_overlay_grid`.
pub fn update_drag_preview(
    state: Res<DesignateState>,
    mirror: Res<TileMirror>,
    mut preview: ResMut<DragPreview>,
    mut readout: ResMut<DragReadout>,
    mut built: Local<PreviewBuilt>,
) {
    let Some((tool, area, erase)) = state.drag_area() else {
        if built.key.is_some() {
            *built = PreviewBuilt::default();
            *preview = DragPreview::default();
            readout.0.clear();
        }
        return;
    };
    let erasing = erase || tool == Tool::Erase;
    // An erase previews the box only (the overlay keeps each tile's kind).
    let kind = if erasing { None } else { tool.kind() };
    let key = Some((area, kind, erasing, mirror.version()));
    if built.key == key {
        return;
    }
    built.key = key;
    let clipped = area.clip_to_region();
    let count = clipped.map_or(0, |a| a.tile_count());
    let (text, cells) = match clipped {
        Some(a) if count <= MAX_ORDER_TILES => match kind {
            Some(k) => {
                let cells: Vec<_> = a.tiles().map(|p| (p, validate(&*mirror, k, p))).collect();
                (drag_text(area, kind, &summary_of(&cells)), cells)
            }
            None => {
                let only = if tool == Tool::Erase { None } else { tool.kind() };
                let summary = erase_summary(mirror.designations(), a, only);
                (drag_text(area, None, &summary), a.tiles().map(|p| (p, Ok(()))).collect())
            }
        },
        Some(_) => (too_large_text(area), Vec::new()),
        None => (drag_text(area, kind, &OrderSummary::default()), Vec::new()),
    };
    *preview = DragPreview { area: Some(area), kind, cells };
    readout.0 = text;
}

/// A world-space gizmo cuboid over the tiles of `b` (region-local) in `region`.
fn tile_box_transform(b: TileBox, region: RegionId, inflate: f32) -> Transform {
    let size = BLOCK_VOXELS as f32;
    let min = Vec3::new(b.min[0] as f32, b.min[1] as f32, b.min[2] as f32) * size;
    let max = Vec3::new(b.max[0] as f32 + 1.0, b.max[1] as f32 + 1.0, b.max[2] as f32 + 1.0) * size;
    let offset = Vec3::from_array(region.world_offset());
    Transform::from_translation(offset + (min + max) / 2.0).with_scale(max - min + Vec3::splat(2.0 * inflate))
}

fn kind_color(kind: Option<DesignationKind>) -> Color {
    match kind {
        Some(DesignationKind::Dig) => DIG_COLOR,
        Some(DesignationKind::Deconstruct) => DECONSTRUCT_COLOR,
        Some(DesignationKind::Build(_)) => BUILD_COLOR,
        None => ERASE_COLOR,
    }
}

/// Build-ghost boxes last merged, and what for.
#[derive(Default)]
pub struct GhostCache {
    key: Option<(u64, i32)>,
    boxes: Vec<TileBox>,
}

/// The drag box, the hover outline and the build-designation ghosts.
pub fn draw_designate_gizmos(
    state: Res<DesignateState>,
    preview: Res<DragPreview>,
    mirror: Res<TileMirror>,
    working: Res<WorkingLevel>,
    mut gizmos: Gizmos,
    mut hidden: Gizmos<HiddenEdgeGizmos>,
    mut ghosts: Local<GhostCache>,
) {
    let Some(region) = mirror.region() else { return };
    if let Some(level) = working.level {
        let key = Some((mirror.version(), level));
        if ghosts.key != key {
            ghosts.key = key;
            let (lo, hi) = (overlay_base_level(level), level + DWARF_HEIGHT_LEVELS);
            let mut builds: Vec<TilePos> = mirror
                .designations()
                .iter()
                .filter(|(p, d)| matches!(d.kind, DesignationKind::Build(_)) && (lo..=hi).contains(&p[1]))
                .map(|(p, _)| *p)
                .collect();
            builds.sort_unstable_by_key(|p| (p[1], p[2], p[0]));
            ghosts.boxes = merge_runs(builds.into_iter());
            ghosts.boxes.truncate(MAX_GHOST_BOXES);
        }
        for b in &ghosts.boxes {
            gizmos.cube(tile_box_transform(*b, region, 0.0), BUILD_COLOR);
        }
    }
    if let Some((_, area, _)) = state.drag_area() {
        let too_large = area.clip_to_region().is_some_and(|a| a.tile_count() > MAX_ORDER_TILES);
        let color = if too_large { TOO_LARGE_COLOR } else { kind_color(preview.kind) };
        let tf = tile_box_transform(area, region, HOVER_INFLATE);
        gizmos.cube(tf, color);
        hidden.cube(tf, color.with_alpha(HIDDEN_EDGE_ALPHA));
    }
    if let (Some(_), Some(p)) = (state.tool, state.hover) {
        gizmos.cube(tile_box_transform(TileBox { min: p, max: p }, region, HOVER_INFLATE), HOVER_COLOR);
    }
}

/// The hidden-edge group draws in front of all geometry.
pub fn setup_designate_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<HiddenEdgeGizmos>();
    config.depth_bias = -1.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_for_dig_and_ctrl_erase() {
        let area = TileBox::new([0, 5, 0], [3, 5, 2]);
        let op = OpId { client: 3, seq: 9 };
        assert_eq!(
            order_for(Tool::Dig, area, false, op, 6),
            Order::Designate { op, kind: DesignationKind::Dig, priority: 6, area }
        );
        assert_eq!(
            order_for(Tool::Dig, area, true, op, 6),
            Order::Erase { area, only: Some(DesignationKind::Dig) }
        );
        assert_eq!(order_for(Tool::Erase, area, false, op, 6), Order::Erase { area, only: None });
    }

    #[test]
    fn corner_follows_the_level_without_a_hover() {
        assert_eq!(follow_corner([1, 5, 2], Some([7, 9, 8]), Some(6)), [7, 6, 8]);
        assert_eq!(follow_corner([1, 5, 2], None, Some(6)), [1, 6, 2], "level lands with no hover");
        assert_eq!(follow_corner([1, 5, 2], Some([7, 9, 8]), None), [7, 5, 8]);
        assert_eq!(follow_corner([1, 5, 2], None, None), [1, 5, 2]);
    }

    #[test]
    fn wall_tool_resolves_two_levels() {
        let area = resolve_drag(Tool::Wall.shape(), [1, 10, 1], [4, 10, 1]);
        assert_eq!(area.min[1], 10);
        assert_eq!(area.max[1], 11, "a same-level wall drag is dwarf height");
        assert_eq!(Tool::Wall.kind(), Some(DesignationKind::Build(game::edit::PLACE_BLOCK)));
    }

    #[test]
    fn merge_runs_joins_x_runs() {
        let tiles = (0..4).map(|x| [x, 0, 0]).chain((0..2).map(|x| [x, 0, 1]));
        assert_eq!(
            merge_runs(tiles),
            vec![TileBox::new([0, 0, 0], [3, 0, 0]), TileBox::new([0, 0, 1], [1, 0, 1])]
        );
    }

    #[test]
    fn drag_text_lists_reasons_in_order() {
        let area = TileBox::new([0, 7, 0], [3, 8, 2]);
        let summary = OrderSummary {
            valid: 20,
            invalid: [(Invalid::RegionEdge, 1), (Invalid::Bedrock, 3)].into_iter().collect(),
        };
        assert_eq!(
            drag_text(area, Some(DesignationKind::Dig), &summary),
            "4 × 3 × 2 — 20 to dig · 3 bedrock · 1 region edge"
        );
    }

    #[test]
    fn tool_keys_avoid_camera_keys() {
        let camera = [
            KeyCode::KeyW,
            KeyCode::KeyA,
            KeyCode::KeyS,
            KeyCode::KeyD,
            KeyCode::KeyQ,
            KeyCode::KeyE,
            KeyCode::Space,
            KeyCode::ShiftLeft,
            KeyCode::ArrowUp,
            KeyCode::ArrowDown,
            KeyCode::ArrowLeft,
            KeyCode::ArrowRight,
        ];
        for (key, tool) in TOOL_KEYS {
            assert!(!camera.contains(&key), "{tool:?} is bound to camera key {key:?}");
        }
    }

    #[test]
    fn z_selects_erase_only_without_ctrl() {
        assert_eq!(tool_for_key(KeyCode::KeyZ, false), Some(Tool::Erase));
        assert_eq!(tool_for_key(KeyCode::KeyZ, true), None, "Ctrl+Z is undo only");
        assert_eq!(tool_for_key(KeyCode::KeyG, false), Some(Tool::Dig));
        assert_eq!(tool_for_key(KeyCode::KeyW, false), None);
    }

    #[test]
    fn cancel_drops_the_drag_then_the_tool() {
        let mut state = DesignateState { tool: Some(Tool::Dig), ..default() };
        state.drag = Some(Drag { anchor: [0, 0, 0], corner: [1, 0, 1], erase: false });
        state.cancel();
        assert_eq!((state.tool, state.drag), (Some(Tool::Dig), None));
        state.cancel();
        assert_eq!(state.tool, None);
    }

    #[test]
    fn erase_summary_counts_matching_designations() {
        let d = |kind| Designation { kind, op: OpId::default(), priority: DEFAULT_PRIORITY };
        let marks: BTreeMap<TilePos, Designation> = [
            ([0, 0, 0], d(DesignationKind::Dig)),
            ([1, 0, 0], d(DesignationKind::Deconstruct)),
            ([9, 0, 0], d(DesignationKind::Dig)),
        ]
        .into_iter()
        .collect();
        let area = TileBox::new([0, 0, 0], [2, 0, 0]);
        assert_eq!(erase_summary(&marks, area, None).valid, 2);
        assert_eq!(erase_summary(&marks, area, Some(DesignationKind::Dig)).valid, 1);
        assert_eq!(drag_text(area, None, &erase_summary(&marks, area, None)), "3 × 1 × 1 — 2 to erase");
    }
}
