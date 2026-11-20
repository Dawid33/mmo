//! Adventure mode on the client: mode switch requests (only the autopilot
//! writes them — the player has no way into the overview: Adventure is the
//! default, the body view, `first_person`, is always on in it, and a client
//! that comes back in Overview is asked back out, [`ask_for_adventure`]), the local
//! body's visibility and the fortress tools' off switch.
//!
//! The mode itself is sim state (`game::PlayMode` on the client's
//! `game::Client`); the client mirrors it in [`ClientPlayMode`] from the
//! sim's `UiUpdate::PlayMode` (`slint_overlay::drain_ui_updates`), never
//! guessing ahead of the sim. A request sends `Order::SetMode`; entering
//! Adventure puts the body on the tile under the camera focus
//! ([`focus_tile`]). In Adventure the body is drawn in third person (it is
//! hidden in first person, where the view is inside it, and in Overview — the
//! commander has no body) and can be selected
//! (`selection::is_selectable`); the designation tools and the tool wheel
//! are off.

use bevy::prelude::*;
use game::rapier3d::math::Vector;
use game::{Order, PlayMode, REGION_BLOCKS, TilePos};

use super::CommanderCamera;
use super::designate::DesignateState;
use super::first_person::BodyView;
use super::follow::{Follow, FollowTarget};
use super::selection::Selection;
use super::tiles::TileMirror;
use crate::renderer::bridge::{LocalBody, SimEntity};
use crate::renderer::{GameEvents, LocalPlayer};

/// The local client's play mode as the sim last reported it.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClientPlayMode(pub PlayMode);

impl ClientPlayMode {
    pub fn adventure(self) -> bool {
        self.0 == PlayMode::Adventure
    }
}

/// The local client's authoritative editor state and per-world capability.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct EditorState {
    pub active: bool,
    pub allowed: bool,
    /// `allowed` came from an authoritative projection, not the reset default.
    pub allowed_known: bool,
}

/// Switch Overview ↔ Adventure. Written only by the autopilot
/// (`renderer/autopilot.rs`), which flies in the overview; the player has no
/// key or button for it.
#[derive(Message, Debug, Clone, Copy)]
pub struct ToggleModeRequest;

/// How often [`ask_for_adventure`] asks again while the mode still reads
/// Overview (as the autopilot paces its ask for the overview).
pub const ADVENTURE_ASK_EVERY_MS: u64 = 2_000;

/// Present while the autopilot drives the session (`renderer/autopilot.rs`
/// inserts it): it flies in the overview, so [`ask_for_adventure`] stands
/// aside.
#[derive(Resource, Debug, Clone, Copy)]
pub struct AutopilotFlies;

/// The player has no way into the overview, and none out of it either: a
/// client saved in Overview (`PlayMode` is per-client sim state and survives
/// parking) would come back in the commander camera, stranded. While the mode
/// reads Overview in a world that has the screen — and no autopilot wants it
/// — this asks the sim for Adventure (`Order::SetMode`, the body staying where
/// it stands), again every [`ADVENTURE_ASK_EVERY_MS`] until the mode says so.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn ask_for_adventure(
    time: Res<Time<Real>>,
    mode: Res<ClientPlayMode>,
    state: Option<Res<State<crate::session::AppState>>>,
    in_play: Option<Res<crate::session::InPlay>>,
    autopilot: Option<Res<AutopilotFlies>>,
    local: Res<LocalPlayer>,
    events: Res<GameEvents>,
    mut asked: crate::session::SessionLocal<Option<std::time::Duration>>,
) {
    let asked = asked.get();
    if mode.adventure() || autopilot.is_some() {
        *asked = None;
        return;
    }
    if crate::session::frontend_has_screen(state.as_deref(), in_play.as_deref()) {
        return;
    }
    let Some(client) = local.0 else { return };
    let now = time.elapsed();
    if !ask_due(*asked, now) {
        return;
    }
    *asked = Some(now);
    let order = Order::SetMode {
        mode: PlayMode::Adventure,
        at: None,
    };
    let _ = events.0.send(game::GameEventKind::Order(client, order));
}

/// Whether to ask again: never asked, or [`ADVENTURE_ASK_EVERY_MS`] since.
pub fn ask_due(last: Option<std::time::Duration>, now: std::time::Duration) -> bool {
    last.is_none_or(|at| {
        now.saturating_sub(at) >= std::time::Duration::from_millis(ADVENTURE_ASK_EVERY_MS)
    })
}

/// The other mode.
pub fn toggled(mode: PlayMode) -> PlayMode {
    match mode {
        PlayMode::Overview => PlayMode::Adventure,
        PlayMode::Adventure => PlayMode::Overview,
    }
}

/// The tile under the camera focus (world x, z and the ground height
/// `ground_y`, sim units), local to the home region drawn at `origin`: the
/// air tile feet standing on that ground occupy (`game::feet_tile`).
pub fn focus_tile(focus: Vec2, ground_y: f32, origin: Vec3) -> TilePos {
    let o = origin;
    game::feet_tile(Vector::new(focus.x - o[0], ground_y - o[1], focus.y - o[2]))
}

/// The order the mode button sends from `mode`: the other mode, entering
/// Adventure at the focus tile (when there is a home region, drawn at
/// `home_origin`).
pub fn toggle_order(mode: PlayMode, cam: &CommanderCamera, home_origin: Option<Vec3>) -> Order {
    let next = toggled(mode);
    let at = match next {
        PlayMode::Adventure => home_origin.map(|origin| {
            // A focus outside the home region (panned away) puts the body at
            // the nearest home tile, not at an out-of-region one.
            let mut t = focus_tile(cam.focus, cam.ground_y, origin);
            let last = REGION_BLOCKS as i32 - 1;
            t[0] = t[0].clamp(0, last);
            t[2] = t[2].clamp(0, last);
            t
        }),
        PlayMode::Overview => None,
    };
    Order::SetMode { mode: next, at }
}

/// Mode request → `Order::SetMode`. The mode changes when the sim says so.
pub fn send_mode_toggle(
    mut requests: MessageReader<ToggleModeRequest>,
    mode: Res<ClientPlayMode>,
    cam: Res<CommanderCamera>,
    mirror: Res<TileMirror>,
    events: Res<GameEvents>,
    local: Res<LocalPlayer>,
) {
    // One press per frame is plenty; extra ones would toggle back.
    if requests.read().count() == 0 {
        return;
    }
    let Some(client) = local.0 else { return };
    let order = toggle_order(mode.0, &cam, mirror.home_offset());
    let _ = events.0.send(game::GameEventKind::Order(client, order));
}

/// The body's visibility for a mode and the body view (`None` = off): drawn
/// (inheriting) in Adventure — but not in first person, where the view is
/// inside it and its model would fill it — hidden in Overview.
pub fn body_visibility(mode: PlayMode, view: Option<BodyView>) -> Visibility {
    match (mode, view) {
        (PlayMode::Adventure, Some(BodyView::First)) => Visibility::Hidden,
        (PlayMode::Adventure, _) => Visibility::Inherited,
        (PlayMode::Overview, _) => Visibility::Hidden,
    }
}

/// Show or hide the local body (and its rig, a child that inherits) with
/// the mode and the body view. Writes only on a change of either or a newly
/// tagged body (the bridge
/// tags it `Hidden`), so the z-level cut (`cut_entities`), which watches for
/// other writers, is not disturbed every frame.
pub fn sync_body_visibility(
    mode: Res<ClientPlayMode>,
    first_person: Res<super::first_person::FirstPerson>,
    mut bodies: Query<(Ref<LocalBody>, &mut Visibility)>,
) {
    let want = body_visibility(mode.0, first_person.view_for_body());
    for (body, mut vis) in &mut bodies {
        if mode.is_changed() || first_person.is_changed() || body.is_added() {
            vis.set_if_neq(want);
        }
    }
}

/// On a mode change: Adventure puts the designate tool down; Overview drops
/// the body from the selection (it is not selectable there). The body view
/// is not entered here: `first_person::ensure_first_person` keeps it on in
/// Adventure every frame (a one-shot request from here was lost when it ran
/// before the local player was known).
pub fn on_mode_change(
    mode: Res<ClientPlayMode>,
    mut designate: ResMut<DesignateState>,
    mut selection: ResMut<Selection>,
    mut follow: ResMut<Follow>,
    bodies: Query<&SimEntity, With<LocalBody>>,
) {
    if !mode.is_changed() {
        return;
    }
    match mode.0 {
        PlayMode::Adventure => {
            if designate.tool.is_some() {
                designate.set_tool(None);
            }
        }
        PlayMode::Overview => {
            // The body is hidden in Overview: don't stay pinned to it.
            if follow.0 == Some(FollowTarget::LocalPlayer) {
                follow.0 = None;
            }
            for body in &bodies {
                selection.0.remove(&(body.region, body.key));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: player saved in Overview came back in the commander camera with no way out; seen in the owner's play session during third-person polish (21dbe0f8)
    /// A session that starts in Overview (a client saved there) without the
    /// autopilot: asks for Adventure once per interval, never every frame, and
    /// once the mode reads Adventure the body view is on.
    #[test]
    fn a_client_back_in_the_overview_asks_for_adventure_and_ends_in_the_body() {
        use super::super::first_person::{FirstPerson, ensure_first_person};
        use super::super::level::WorkingLevel;
        use crate::renderer::cursor::CursorPolicy;
        let (send, recv) = crossbeam::channel::unbounded();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_millis(500),
            ))
            .insert_resource(ClientPlayMode(PlayMode::Overview))
            .init_resource::<CursorPolicy>()
            .init_resource::<FirstPerson>()
            .init_resource::<WorkingLevel>()
            .init_resource::<TileMirror>()
            .init_resource::<CommanderCamera>()
            .insert_resource(LocalPlayer(Some(7)))
            .insert_resource(GameEvents(send))
            .add_systems(Update, (ask_for_adventure, ensure_first_person).chain());
        let asks = |recv: &crossbeam::channel::Receiver<game::GameEventKind>| {
            recv.try_iter()
                .filter(|e| {
                    matches!(
                        e,
                        game::GameEventKind::Order(
                            7,
                            Order::SetMode {
                                mode: PlayMode::Adventure,
                                at: None
                            }
                        )
                    )
                })
                .count()
        };
        // 4 frames of 0.5 s (the first frame's delta is zero): within one interval.
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(asks(&recv), 1, "one ask, not one a frame");
        assert!(
            !app.world().resource::<FirstPerson>().on,
            "no body view in the overview"
        );
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(asks(&recv), 1, "asked again after the interval");
        // The sim switched.
        app.world_mut().resource_mut::<ClientPlayMode>().0 = PlayMode::Adventure;
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(asks(&recv), 0, "no more asks in Adventure");
        assert!(app.world().resource::<FirstPerson>().on, "in the body view");
    }
}
