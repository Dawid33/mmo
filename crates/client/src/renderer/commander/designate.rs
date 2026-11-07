//! Designation tools: DF-style dig / deconstruct / build / erase orders
//! painted onto the home region's tiles at the working level.
//!
//! A tool key ([`TOOL_KEYS`]) picks a tool; the tile under the cursor at the
//! working level is the hover ([`pick_level_cell`]) — or, with the cut off
//! ("Show above"), the block the cursor is on at any level
//! ([`pick_visible_cell`]), and then corners take their level from it. A box is picked by its
//! two corners ([`step_corners`]), either way: click a tile (press and
//! release without moving) to anchor the first corner, then click again for
//! the second; or press, drag and release. The moving corner is the hovered
//! column at the CURRENT working level — Space / Shift / PgUp / PgDn step the
//! level between the clicks or mid-drag, and the camera may move, so one box
//! covers several levels — and the commit sends exactly that box as a sim
//! [`Order`] (Ctrl held at the commit: an erase of the tool's kind). While a
//! box is open, [`DragPreview`] carries it and each tile's validity for the
//! overlay, and [`DragReadout`] the size-and-reasons line. Esc or a right
//! click (not a right drag, which still orbits) drops the box, or with none
//! the tool; Ctrl+Z cancels the last order sent; 1–7 set the priority while a
//! tool is active.
//!
//! Gizmos draw the open box (bright where visible, dimmed through terrain),
//! the hover cell, and the build designations (they sit in air, where the
//! overlay has no face to tint) as merged ghost cuboids.
//!
//! Legacy regions only: in cell mode the tile mirror has no home region, so
//! there is nothing to pick and no tool can be chosen.

use std::collections::BTreeMap;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use game::rapier3d::math::Vector;
use game::{
    pick_level_cell, pick_visible_cell, validate, Designation, DesignationKind, Invalid, OpId, Order, OrderSummary,
    RegionId, TileBox, TilePos, TileSource, BLOCK_VOXELS, DEFAULT_PRIORITY, MAX_ORDER_TILES, MAX_PRIORITY,
    REGION_BLOCKS,
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
    /// Fills exactly the picked box with [`game::edit::PLACE_BLOCK`]; a wall
    /// or a tower is a box whose corners sit on different levels.
    Build,
    /// Fells the part of a tree beyond each picked log or branch block.
    Chop,
    Erase,
}

/// Tool hotkeys (bare, no Ctrl — Ctrl+Z is undo). Kept off the camera's
/// WASD / arrows and the level keys Space / Shift.
pub const TOOL_KEYS: [(KeyCode, Tool); 5] = [
    (KeyCode::KeyG, Tool::Dig),
    (KeyCode::KeyX, Tool::Deconstruct),
    (KeyCode::KeyB, Tool::Build),
    (KeyCode::KeyT, Tool::Chop),
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
/// Build ghosts are drawn from the overlay's base level up to this many
/// levels above the working level, so the top of a two-level wall built
/// from the level below still shows above the cut.
pub const GHOST_LEVELS_ABOVE: i32 = 1;

/// How much the hover outline is grown past its cell, in sim units, so it
/// does not z-fight the block faces.
const HOVER_INFLATE: f32 = 0.25;

const DIG_COLOR: Color = Color::srgb(1.0, 0.72, 0.2);
const DECONSTRUCT_COLOR: Color = Color::srgb(1.0, 0.3, 0.25);
const BUILD_COLOR: Color = Color::srgb(0.6, 0.8, 1.0);
const CHOP_COLOR: Color = Color::srgb(0.55, 0.9, 0.35);
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
            Tool::Build => Some(DesignationKind::Build(game::edit::PLACE_BLOCK)),
            Tool::Chop => Some(DesignationKind::Chop),
            Tool::Erase => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Dig => "Dig",
            Tool::Deconstruct => "Deconstruct",
            Tool::Build => "Build",
            Tool::Chop => "Chop",
            Tool::Erase => "Erase",
        }
    }

    /// The hotkey as shown on the tool bar.
    pub fn key_label(self) -> &'static str {
        match self {
            Tool::Dig => "G",
            Tool::Deconstruct => "X",
            Tool::Build => "B",
            Tool::Chop => "T",
            Tool::Erase => "Z",
        }
    }
}

/// An open box: its first corner, the corner that follows the cursor now
/// (its `y` the working level), and whether it erases (Ctrl held).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drag {
    pub anchor: TilePos,
    pub corner: TilePos,
    pub erase: bool,
}

/// Where the two-corner pick stands. Pure: [`step_corners`] moves it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum CornerPhase {
    #[default]
    Idle,
    /// The left button is down. `start` is the box's first corner: the
    /// pressed tile, or the anchor when the press came while `anchored`.
    Pressed { start: TilePos, press_px: Vec2, anchored: bool },
    /// The first corner is set by a click; the next click sets the second.
    Anchored { anchor: TilePos },
}

impl CornerPhase {
    /// The box's first corner, while one is open.
    pub fn start(self) -> Option<TilePos> {
        match self {
            CornerPhase::Idle => None,
            CornerPhase::Pressed { start, .. } => Some(start),
            CornerPhase::Anchored { anchor } => Some(anchor),
        }
    }
}

/// Left press / release at a tile (`px`: the cursor, window pixels), or Esc /
/// right click.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CornerEvent {
    Press { cell: TilePos, px: Vec2 },
    Release { cell: TilePos, px: Vec2 },
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CornerOutcome {
    None,
    /// The box from `a` to `b` is picked: send it.
    Commit { a: TilePos, b: TilePos },
    /// An open box was dropped.
    Cancelled,
}

/// One step of the two-corner pick. A release within `drag_threshold_px` of
/// its press is a click: the first click anchors, the second commits from
/// the anchor to the released tile. A release further away is a drag and
/// commits from the press to it — or, if the press came while anchored,
/// from the anchor (a drag finishes an anchored box rather than starting a
/// new one). Cancel drops an open box; with none it is a no-op here (the
/// caller then puts the tool down).
pub fn step_corners(phase: &mut CornerPhase, ev: CornerEvent, drag_threshold_px: f32) -> CornerOutcome {
    match (*phase, ev) {
        (CornerPhase::Idle, CornerEvent::Press { cell, px }) => {
            *phase = CornerPhase::Pressed { start: cell, press_px: px, anchored: false };
            CornerOutcome::None
        }
        (CornerPhase::Anchored { anchor }, CornerEvent::Press { px, .. }) => {
            *phase = CornerPhase::Pressed { start: anchor, press_px: px, anchored: true };
            CornerOutcome::None
        }
        (CornerPhase::Pressed { start, press_px, anchored }, CornerEvent::Release { cell, px }) => {
            if !anchored && px.distance(press_px) <= drag_threshold_px {
                *phase = CornerPhase::Anchored { anchor: start };
                CornerOutcome::None
            } else {
                *phase = CornerPhase::Idle;
                CornerOutcome::Commit { a: start, b: cell }
            }
        }
        (CornerPhase::Idle, CornerEvent::Cancel) => CornerOutcome::None,
        (_, CornerEvent::Cancel) => {
            *phase = CornerPhase::Idle;
            CornerOutcome::Cancelled
        }
        // A second press while held, or a release whose press was not ours.
        _ => CornerOutcome::None,
    }
}

/// The designation tools' client-local state.
#[derive(Resource, Debug)]
pub struct DesignateState {
    pub tool: Option<Tool>,
    pub priority: u8,
    /// The two-corner pick; `drag` is `Some` exactly while it is not idle.
    pub corners: CornerPhase,
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
        Self {
            tool: None,
            priority: DEFAULT_PRIORITY,
            corners: CornerPhase::Idle,
            drag: None,
            hover: None,
            next_seq: 0,
            sent: Vec::new(),
        }
    }
}

impl DesignateState {
    /// A tool is up or a box is being picked: Esc cancels that before it
    /// opens the menu.
    pub fn in_progress(&self) -> bool {
        self.tool.is_some() || self.corners != CornerPhase::Idle
    }

    /// Esc / right click: drop the open box if there is one, else the tool.
    pub fn cancel(&mut self) {
        if step_corners(&mut self.corners, CornerEvent::Cancel, DRAG_THRESHOLD_PX) == CornerOutcome::None {
            self.tool = None;
        }
        self.drag = None;
    }

    /// Drop any open box (the tool changed or went away).
    pub fn clear_box(&mut self) {
        self.corners = CornerPhase::Idle;
        self.drag = None;
    }

    /// Pick `tool` (or none), dropping the open box when it changes.
    pub fn set_tool(&mut self, tool: Option<Tool>) {
        if self.tool != tool {
            self.tool = tool;
            self.clear_box();
        }
    }

    /// The box the drag covers and whether it erases, with the tool.
    fn drag_area(&self) -> Option<(Tool, TileBox, bool)> {
        let (tool, drag) = (self.tool?, self.drag?);
        Some((tool, TileBox::new(drag.anchor, drag.corner), drag.erase))
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

/// The order a committed box sends: an erase for the erase tool or with
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
        Some(DesignationKind::Chop) => "to chop",
        None => "to erase",
    }
}

fn reason(why: Invalid) -> &'static str {
    match why {
        Invalid::Bedrock => "bedrock",
        Invalid::NothingToDig => "nothing to dig",
        Invalid::Built => "built",
        Invalid::Occupied => "occupied",
        Invalid::NotBuilt => "not built",
        Invalid::NotTree => "not a tree",
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
    [p[0].clamp(0, REGION_BLOCKS as i32 - 1), p[1], p[2].clamp(0, REGION_BLOCKS as i32 - 1)]
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
    mode: Res<super::adventure::ClientPlayMode>,
    mut state: ResMut<DesignateState>,
) {
    if policy.blocks_game() {
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
    // No home region (cell mode, or not joined yet), or playing the body in
    // Adventure: no tools.
    if mirror.region().is_none() || mode.adventure() {
        if state.tool.is_some() {
            state.set_tool(None);
        }
        return;
    }
    for key in keys.get_just_pressed() {
        if let Some(tool) = tool_for_key(*key, ctrl) {
            state.set_tool(Some(tool));
        }
    }
    if state.tool.is_some() {
        if let Some(i) = PRIORITY_KEYS.iter().position(|k| keys.just_pressed(*k)) {
            state.priority = i as u8 + 1;
        }
    }
}

/// How far (blocks) the cursor ray looks for a block with the cut off: far
/// enough to cross the home region from a camera outside it.
pub const VISIBLE_PICK_REACH_BLOCKS: f32 = 2.0 * REGION_BLOCKS as f32;

/// Cursor ray → the hovered tile (region-local). With the cut on it is the
/// tile at the working level ([`pick_level_cell`]); with it off, everything is
/// drawn, so it is the block the cursor is on, at whatever level
/// ([`pick_visible_cell`]; for a build, the air in front of the face), falling
/// back to the working level where the ray hits nothing.
pub fn update_hover(
    window: Query<&Window, With<PrimaryWindow>>,
    view: Query<(&Camera, &Transform), With<CommanderView>>,
    policy: Res<CursorPolicy>,
    mirror: Res<TileMirror>,
    working: Res<WorkingLevel>,
    xray: Option<Res<crate::renderer::voxel_pull::xray::XraySettings>>,
    mut state: ResMut<DesignateState>,
) {
    let xray = crate::renderer::voxel_pull::xray::xray_on(xray.as_deref(), working.cut);
    let hover = (|| {
        let tool = state.tool?;
        if policy.blocks_game() || policy.ui_pointer {
            return None;
        }
        let ray = cursor_ray(window.single().ok()?, view.single().ok()?, mirror.region()?)?;
        let visible = (!working.cut).then(|| pick_visible_xray(ray, tool == Tool::Build, &mirror, xray)).flatten();
        let p = visible.or_else(|| pick_level_cell(ray.0, ray.1, working.level?, |p| game::block_props().blocks_ray(mirror.block(p))))?;
        Some(clamp_to_region(p))
    })();
    if state.hover != hover {
        state.hover = hover;
    }
}

/// The cursor's ray as (origin, normalised direction) in `region`'s local
/// sim units; `None` off the window.
pub fn cursor_ray(window: &Window, (camera, tf): (&Camera, &Transform), region: RegionId) -> Option<(Vector, Vector)> {
    let ray = camera.viewport_to_world(&GlobalTransform::from(*tf), window.cursor_position()?).ok()?;
    let origin = ray.origin - Vec3::from_array(region.world_offset());
    let dir = *ray.direction;
    Some((Vector::new(origin.x, origin.y, origin.z), Vector::new(dir.x, dir.y, dir.z)))
}

/// The block the cursor ray meets first in the mirrored region, at whatever
/// level ([`pick_visible_cell`]); with `face` the air cell in front of the
/// face it entered through.
pub fn pick_visible(ray: (Vector, Vector), face: bool, mirror: &TileMirror) -> Option<TilePos> {
    pick_visible_cell(ray.0, ray.1, VISIBLE_PICK_REACH_BLOCKS, face, |p| game::block_props().blocks_ray(mirror.block(p)))
}

/// [`pick_visible`] under the dug-space x-ray (`xray`, cut off): through
/// the terrain in front of a dug space, onto the floor or wall seen there
/// ([`game::pick_xray_cell`]).
pub fn pick_visible_xray(ray: (Vector, Vector), face: bool, mirror: &TileMirror, xray: bool) -> Option<TilePos> {
    if !xray {
        return pick_visible(ray, face, mirror);
    }
    game::pick_xray_cell(
        ray.0,
        ray.1,
        VISIBLE_PICK_REACH_BLOCKS,
        face,
        |p| game::block_props().blocks_ray(mirror.block(p)),
        |p| mirror.is_dug(p),
    )
}

/// Left presses and releases → corners ([`step_corners`]) and orders; a
/// right click cancels.
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
    mut right_travel: crate::session::SessionLocal<Option<f32>>,
) {
    let right_travel = right_travel.get();
    let Ok(window) = window.single() else { return };
    // A right press is a click unless the mouse travels: orbiting holds the
    // pointer still, so the travel is the raw motion, not the cursor.
    if buttons.just_pressed(MouseButton::Right) {
        *right_travel = (!policy.blocks_game() && !policy.ui_pointer).then_some(0.0);
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
    let cursor = window.cursor_position();
    if buttons.just_pressed(MouseButton::Left) {
        let over_minimap = cursor.is_some_and(|c| minimap::screen_rect(window.size()).contains(c));
        let blocked = policy.blocks_game() || policy.ui_pointer || minimap_pointer.dragging || over_minimap;
        if let (false, Some(at), Some(px)) = (blocked, state.hover, cursor) {
            step_corners(&mut state.corners, CornerEvent::Press { cell: at, px }, DRAG_THRESHOLD_PX);
        }
    }
    let Some(start) = state.corners.start() else { return };
    // The moving corner follows the hover and the working level, whether
    // the button is held (a drag) or not (anchored, waiting for the click).
    // With the cut off the hover is on whatever level the cursor is on, so
    // the corner takes its level from the hover instead.
    let level = if working.cut { working.level } else { state.hover.map(|h| h[1]) };
    let corner = follow_corner(state.drag.map_or(start, |d| d.corner), state.hover, level);
    let drag = Drag { anchor: start, corner, erase: ctrl };
    if state.drag != Some(drag) {
        state.drag = Some(drag);
    }
    let pressed = matches!(state.corners, CornerPhase::Pressed { .. });
    if !buttons.just_released(MouseButton::Left) {
        if pressed && !buttons.pressed(MouseButton::Left) {
            // The release went elsewhere (focus loss): drop the box.
            state.clear_box();
        }
        return;
    }
    // Releasing with the cursor outside the window counts as a drag.
    let px = cursor.unwrap_or(Vec2::INFINITY);
    let CornerOutcome::Commit { a, b } =
        step_corners(&mut state.corners, CornerEvent::Release { cell: corner, px }, DRAG_THRESHOLD_PX)
    else {
        return;
    };
    state.drag = None;
    let area = TileBox::new(a, b);
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
    mut built: crate::session::SessionLocal<PreviewBuilt>,
) {
    let built = built.get();
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
        Some(DesignationKind::Chop) => CHOP_COLOR,
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
    mut ghosts: crate::session::SessionLocal<GhostCache>,
) {
    let ghosts = ghosts.get();
    let Some(region) = mirror.region() else { return };
    if let Some(level) = working.level {
        let key = Some((mirror.version(), level));
        if ghosts.key != key {
            ghosts.key = key;
            let (lo, hi) = (overlay_base_level(level), level + GHOST_LEVELS_ABOVE);
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
    fn chop_tool_on_t() {
        assert_eq!(Tool::Chop.kind(), Some(DesignationKind::Chop));
        assert_eq!(tool_for_key(KeyCode::KeyT, false), Some(Tool::Chop));
        assert_eq!(tool_for_key(KeyCode::KeyT, true), None, "Ctrl+T is not a tool");
        assert_eq!(format!("[{}] {}", Tool::Chop.key_label(), Tool::Chop.label()), "[T] Chop");
        assert_eq!(verb(Tool::Chop.kind()), "to chop");
        assert_eq!(reason(Invalid::NotTree), "not a tree");
    }

    #[test]
    fn build_tool_places_the_construction_block_on_b() {
        assert_eq!(Tool::Build.kind(), Some(DesignationKind::Build(game::edit::PLACE_BLOCK)));
        assert_eq!(tool_for_key(KeyCode::KeyB, false), Some(Tool::Build));
        assert_eq!(tool_for_key(KeyCode::KeyV, false), None, "V is free");
    }

    const T: f32 = DRAG_THRESHOLD_PX;

    fn press(phase: &mut CornerPhase, cell: TilePos, px: Vec2) -> CornerOutcome {
        step_corners(phase, CornerEvent::Press { cell, px }, T)
    }

    fn release(phase: &mut CornerPhase, cell: TilePos, px: Vec2) -> CornerOutcome {
        step_corners(phase, CornerEvent::Release { cell, px }, T)
    }

    #[test]
    fn click_click_commits_on_second_click() {
        let mut phase = CornerPhase::Idle;
        let (here, there) = (Vec2::new(100.0, 100.0), Vec2::new(300.0, 200.0));
        assert_eq!(press(&mut phase, [1, 10, 1], here), CornerOutcome::None);
        assert_eq!(release(&mut phase, [1, 10, 1], here), CornerOutcome::None);
        assert_eq!(phase, CornerPhase::Anchored { anchor: [1, 10, 1] });
        assert_eq!(phase.start(), Some([1, 10, 1]), "the anchored box stays open");
        assert_eq!(press(&mut phase, [6, 10, 4], there), CornerOutcome::None);
        let out = release(&mut phase, [6, 10, 4], there);
        assert_eq!(out, CornerOutcome::Commit { a: [1, 10, 1], b: [6, 10, 4] });
        assert_eq!(phase, CornerPhase::Idle);
    }

    #[test]
    fn click_drag_commits_on_release() {
        let mut phase = CornerPhase::Idle;
        press(&mut phase, [1, 10, 1], Vec2::new(100.0, 100.0));
        let out = release(&mut phase, [5, 10, 3], Vec2::new(100.0 + T + 1.0, 100.0));
        assert_eq!(out, CornerOutcome::Commit { a: [1, 10, 1], b: [5, 10, 3] });
        assert_eq!(phase, CornerPhase::Idle);
    }

    #[test]
    fn level_change_between_clicks_spans_levels() {
        let mut phase = CornerPhase::Idle;
        let px = Vec2::new(50.0, 50.0);
        press(&mut phase, [2, 10, 2], px);
        release(&mut phase, [2, 10, 2], px);
        // Space to level 12 (and the camera may move) before the second click.
        let px = Vec2::new(400.0, 80.0);
        press(&mut phase, [4, 12, 2], px);
        let CornerOutcome::Commit { a, b } = release(&mut phase, [4, 12, 2], px) else {
            panic!("second click commits")
        };
        let area = TileBox::new(a, b);
        assert_eq!((area.min[1], area.max[1]), (10, 12));
        assert_eq!(area.tile_count(), 3 * 3);
    }

    #[test]
    fn small_jitter_counts_as_click() {
        let mut phase = CornerPhase::Idle;
        press(&mut phase, [1, 10, 1], Vec2::new(100.0, 100.0));
        let out = release(&mut phase, [2, 10, 1], Vec2::new(100.0 + T * 0.6, 100.0 - T * 0.6));
        assert_eq!(out, CornerOutcome::None);
        assert_eq!(phase, CornerPhase::Anchored { anchor: [1, 10, 1] }, "anchored at the press");
    }

    #[test]
    fn cancel_clears_anchor() {
        let mut phase = CornerPhase::Idle;
        let px = Vec2::ZERO;
        press(&mut phase, [1, 10, 1], px);
        release(&mut phase, [1, 10, 1], px);
        assert_eq!(step_corners(&mut phase, CornerEvent::Cancel, T), CornerOutcome::Cancelled);
        assert_eq!(phase, CornerPhase::Idle);
        let again = step_corners(&mut phase, CornerEvent::Cancel, T);
        assert_eq!(again, CornerOutcome::None, "nothing left to drop");
        // Mid-press too.
        press(&mut phase, [1, 10, 1], px);
        assert_eq!(step_corners(&mut phase, CornerEvent::Cancel, T), CornerOutcome::Cancelled);
        assert_eq!(release(&mut phase, [3, 10, 3], px), CornerOutcome::None, "its release is not ours");
    }

    #[test]
    fn drag_while_anchored_finishes_the_box() {
        let mut phase = CornerPhase::Idle;
        let px = Vec2::new(10.0, 10.0);
        press(&mut phase, [1, 10, 1], px);
        release(&mut phase, [1, 10, 1], px);
        // Press somewhere else and drag: the anchor stays the first corner.
        press(&mut phase, [8, 11, 8], Vec2::new(200.0, 200.0));
        let out = release(&mut phase, [9, 11, 9], Vec2::new(260.0, 240.0));
        assert_eq!(out, CornerOutcome::Commit { a: [1, 10, 1], b: [9, 11, 9] });
    }

    #[test]
    fn switching_tools_clears_the_anchor() {
        let mut state = DesignateState { tool: Some(Tool::Dig), ..default() };
        state.corners = CornerPhase::Anchored { anchor: [1, 2, 3] };
        state.drag = Some(Drag { anchor: [1, 2, 3], corner: [1, 2, 3], erase: false });
        state.set_tool(Some(Tool::Dig));
        assert!(state.drag.is_some(), "re-picking the same tool keeps the box");
        state.set_tool(Some(Tool::Build));
        assert_eq!((state.corners, state.drag), (CornerPhase::Idle, None));
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
            invalid: [(Invalid::Built, 1), (Invalid::Bedrock, 3)].into_iter().collect(),
        };
        assert_eq!(
            drag_text(area, Some(DesignationKind::Dig), &summary),
            "4 × 3 × 2 — 20 to dig · 3 bedrock · 1 built"
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
        state.corners = CornerPhase::Anchored { anchor: [0, 0, 0] };
        state.drag = Some(Drag { anchor: [0, 0, 0], corner: [1, 0, 1], erase: false });
        state.cancel();
        assert_eq!((state.tool, state.drag, state.corners), (Some(Tool::Dig), None, CornerPhase::Idle));
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
