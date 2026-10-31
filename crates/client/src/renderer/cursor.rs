//! The one writer of the primary window's cursor grab. The sim asks for a
//! grab (fps-cam toggle via `SetFreeCam`, inventory via `SetCursorGrab`) —
//! only first person does; the commander camera never holds the pointer, so it
//! can leave the window freely, except while a right-drag turns it (`orbit`).
//! Client-local overlays (the dev panel, later chat) hold the pointer on top
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
}

impl CursorPolicy {
    pub fn grabbed(&self) -> bool {
        self.sim_grab && !self.overlay_open
    }

    /// A client-local overlay or a modal game screen is open: commander
    /// input leaves the world alone.
    pub fn blocks_game(&self) -> bool {
        self.overlay_open || self.ui_modal
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
/// a free pointer. The client-local `overlay_open` (the dev panel) is not the
/// world's and stays.
pub fn reset_session_grab(world: &mut World) {
    if let Some(mut policy) = world.get_resource_mut::<CursorPolicy>() {
        *policy = CursorPolicy { overlay_open: policy.overlay_open, ..CursorPolicy::default() };
    }
}

/// Apply the policy to the window. Idempotent: writing `CursorOptions` trips
/// change detection, which re-issues the winit grab — an X11 re-grab loop when
/// the grab is contended — so only a real change is written.
pub fn apply_cursor_policy(
    policy: Res<CursorPolicy>,
    mut windows: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    let (grab_mode, visible) = policy.mode();
    if let Ok(mut cursor) = windows.single_mut() {
        if cursor.grab_mode != grab_mode || cursor.visible != visible {
            cursor.grab_mode = grab_mode;
            cursor.visible = visible;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<CursorPolicy>()
            .add_systems(Update, apply_cursor_policy);
        app.world_mut()
            .spawn((Window::default(), CursorOptions::default(), PrimaryWindow));
        app
    }

    fn grab_mode(app: &mut App) -> CursorGrabMode {
        let w = app.world_mut();
        w.query_filtered::<&CursorOptions, With<PrimaryWindow>>()
            .single(w)
            .unwrap()
            .grab_mode
    }

    #[test]
    fn overlay_frees_the_cursor_and_closing_it_restores_the_sim_grab() {
        let mut app = app();
        app.world_mut().resource_mut::<CursorPolicy>().sim_grab = true;
        app.update();
        assert_eq!(grab_mode(&mut app), CursorGrabMode::Locked);

        app.world_mut().resource_mut::<CursorPolicy>().overlay_open = true;
        app.update();
        assert_eq!(grab_mode(&mut app), CursorGrabMode::None);

        // The sim toggling while the overlay is open is kept, not lost.
        app.world_mut().resource_mut::<CursorPolicy>().sim_grab = false;
        app.world_mut().resource_mut::<CursorPolicy>().sim_grab = true;
        app.world_mut().resource_mut::<CursorPolicy>().overlay_open = false;
        app.update();
        assert_eq!(grab_mode(&mut app), CursorGrabMode::Locked);
    }

    #[test]
    fn only_a_sim_grab_holds_the_pointer() {
        assert_eq!(CursorPolicy::default().mode(), (CursorGrabMode::None, true));
        let locked = CursorPolicy { sim_grab: true, ..Default::default() };
        assert_eq!(locked.mode(), (CursorGrabMode::Locked, false));
        let overlay = CursorPolicy { overlay_open: true, ..locked };
        assert_eq!(overlay.mode(), (CursorGrabMode::None, true));
    }

    #[test]
    fn an_orbit_drag_holds_the_pointer_hidden_unless_an_overlay_is_open() {
        let orbit = CursorPolicy { orbit: true, ..Default::default() };
        assert_eq!(orbit.mode(), (CursorGrabMode::Locked, false));
        assert!(!orbit.grabbed(), "an orbit drag is not the sim's mouse look");
        let overlay = CursorPolicy { overlay_open: true, ..orbit };
        assert_eq!(overlay.mode(), (CursorGrabMode::None, true));
    }
}
