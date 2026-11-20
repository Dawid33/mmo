//! The one writer of the primary window's cursor grab. The sim asks for a
//! grab (fps-cam toggle via `SetFreeCam`, inventory via `SetCursorGrab`) —
//! only first person does; the commander camera never holds the pointer, so it
//! can leave the window freely, except while a right-drag turns it (`orbit`).
//! Client-local overlays (none yet; later chat) hold the pointer on top
//! of that. Both used to write `CursorOptions` directly, so an overlay would
//! either fight the sim or lose its request on close.
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPolicy {
    /// Last grab the sim asked for. Starts ungrabbed, like the window.
    pub sim_grab: bool,
    /// A client-local overlay needs a free pointer.
    pub overlay_open: bool,
    /// The pointer is over the Slint game UI, or a press that started there
    /// is still held (`slint_overlay`): world clicks, drags, edge panning and
    /// zoom leave it alone. Unlike `overlay_open` it frees nothing.
    pub ui_pointer: bool,
    /// The Slint menu or world map is open (written only by
    /// `slint_overlay`): the game behind it takes no keys, clicks, drags or
    /// wheel. Like `ui_pointer` it frees nothing.
    pub ui_modal: bool,
    /// A commander right-drag is turning the camera: the pointer is held
    /// hidden and still (`commander::hold_pointer_while_orbiting` puts it back
    /// where the press was) until the button is released.
    pub orbit: bool,
    /// The commander's adventurer is in first person
    /// (`commander::first_person`): mouse look holds the pointer, except
    /// while the menu or map is open.
    pub first_person: bool,
    /// The pointer was given back although a grab is wanted — the window lost
    /// focus, or first person began without the player asking (Adventure
    /// starting) — and stays free until a click in the window takes it again
    /// ([`retake_pointer`]).
    pub released: bool,
    /// The loading screen is up or fading (written only by
    /// `frontend::loading`): nothing the player does reaches the sim or the
    /// commander, and the pointer stays free.
    pub loading: bool,
    /// A Slint text field has the keyboard (written only by
    /// `text_entry::mute_keys_while_typing`): the pointer is free and the
    /// game behind takes no clicks, like an overlay.
    pub typing: bool,
    /// E was pressed in first person to open the inventory, at this
    /// `Time::elapsed` (written only by `input::forward_input`). Mouse look
    /// stops and the pointer is freed on that frame, not a tick later when the
    /// sim's open reaches the screen; the hold ends once the inventory is up
    /// (`ui_modal`), or after [`INVENTORY_OPEN_WAIT`] if it never comes.
    pub inventory_opening: Option<std::time::Duration>,
}

/// How long an E press holds mouse look waiting for the inventory to open (the
/// local prediction opens it within a tick; a press the sim never took — the
/// join replay drops input — gives the look back after this).
pub const INVENTORY_OPEN_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

impl CursorPolicy {
    /// Mouse look is asked for (by the sim or first person), whatever holds
    /// it back.
    pub fn wants_grab(&self) -> bool {
        self.sim_grab || self.first_person
    }

    /// A menu, the map or the inventory frees the pointer whoever asked for
    /// it, and so does an overlay; a released pointer waits for a click.
    pub fn grabbed(&self) -> bool {
        self.wants_grab()
            && !self.ui_modal
            && !self.overlay_open
            && !self.released
            && !self.loading
            && !self.typing
            && self.inventory_opening.is_none()
    }

    /// Mouse travel turns the body: the ONE rule both `input::forward_input`
    /// (what the sim is sent) and `look::integrate_local_look` (what the
    /// screen shows) apply. Gated apart, a frame one counted and the other
    /// did not turned the view and then snapped it back to the sim's look
    /// (`LOOK_RESYNC_IDLE`) — e.g. the frame the inventory went modal while
    /// the pointer was still locked, often a long one.
    pub fn look_live(&self) -> bool {
        !(self.blocks_game()
            || (self.released && self.wants_grab())
            || self.inventory_opening.is_some())
    }

    /// A client-local overlay or a modal game screen is open: commander
    /// input leaves the world alone.
    pub fn blocks_game(&self) -> bool {
        self.overlay_open || self.ui_modal || self.loading || self.typing
    }

    /// The grab mode and cursor visibility the window should have.
    pub fn mode(&self) -> (CursorGrabMode, bool) {
        if self.grabbed() || (self.orbit && !self.overlay_open) {
            (CursorGrabMode::Locked, false)
        } else {
            (CursorGrabMode::None, true)
        }
    }
}

/// Session reset: the world's grab requests go (the sim's grab, the menu or
/// map being open, an orbit or a UI press in flight), so the next screen gets
/// a free pointer. The client-local `overlay_open` is not the
/// world's and stays.
pub fn reset_session_grab(world: &mut World) {
    if let Some(mut policy) = world.get_resource_mut::<CursorPolicy>() {
        *policy = CursorPolicy {
            overlay_open: policy.overlay_open,
            ..CursorPolicy::default()
        };
    }
}

/// Give the pointer back when the window loses focus, and take it again on a
/// left click in the focused window (not on the game UI). A click while a
/// menu, the map or the inventory is open takes it back for when that closes.
/// Runs before [`apply_cursor_policy`].
pub fn retake_pointer(
    mut policy: ResMut<CursorPolicy>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let Ok(window) = windows.single() else { return };
    if !policy.wants_grab() {
        // Nothing to give back or take: the next grab starts as it is asked.
        if policy.released && !policy.first_person {
            policy.released = false;
        }
        return;
    }
    if !window.focused {
        if !policy.released {
            policy.released = true;
        }
        return;
    }
    if !policy.released {
        return;
    }
    let Some(buttons) = buttons else { return };
    // Under a menu, the map, the inventory or an overlay any click in the
    // window is the player back in it: the screen keeps the pointer free
    // while it is open (`grabbed`), and closing it grabs with no further
    // click. In the world it takes a left click off the game UI.
    let under_screen = (policy.ui_modal || policy.overlay_open) && !policy.loading;
    let clicked = if under_screen {
        buttons.get_just_pressed().next().is_some()
    } else {
        buttons.just_pressed(MouseButton::Left) && !policy.loading && !policy.ui_pointer
    };
    if clicked {
        policy.released = false;
    }
}

/// Apply the policy to the window. Idempotent: writing `CursorOptions` trips
/// change detection, which re-issues the winit grab — an X11 re-grab loop when
/// the grab is contended — so only a real change is written. An unfocused
/// window never holds the pointer, whatever the policy says.
pub fn apply_cursor_policy(
    policy: Res<CursorPolicy>,
    mut windows: Query<(&Window, &mut CursorOptions), With<PrimaryWindow>>,
) {
    if let Ok((window, mut cursor)) = windows.single_mut() {
        let (grab_mode, visible) = if window.focused {
            policy.mode()
        } else {
            (CursorGrabMode::None, true)
        };
        if cursor.grab_mode != grab_mode || cursor.visible != visible {
            cursor.grab_mode = grab_mode;
            cursor.visible = visible;
        }
    }
}
