//! The one writer of the primary window's cursor grab. The sim asks for a
//! grab (fps-cam toggle via `SetFreeCam`, inventory via `SetCursorGrab`); the
//! commander camera asks for a confined, visible pointer (edge panning);
//! client-local overlays (the dev panel, later chat) hold the pointer on top
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
    /// Keep the (visible) pointer inside the window: the commander camera's
    /// edge panning needs it. Only while the window is focused.
    pub confine: bool,
}

impl CursorPolicy {
    pub fn grabbed(&self) -> bool {
        self.sim_grab && !self.overlay_open
    }

    /// The grab mode and cursor visibility the window should have.
    pub fn mode(&self) -> (CursorGrabMode, bool) {
        if self.grabbed() {
            (CursorGrabMode::Locked, false)
        } else if self.confine && !self.overlay_open {
            (CursorGrabMode::Confined, true)
        } else {
            (CursorGrabMode::None, true)
        }
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
    fn confine_keeps_the_cursor_visible_and_yields_to_lock_and_overlays() {
        let confined = CursorPolicy { confine: true, ..Default::default() };
        assert_eq!(confined.mode(), (CursorGrabMode::Confined, true));
        let locked = CursorPolicy { sim_grab: true, ..confined };
        assert_eq!(locked.mode(), (CursorGrabMode::Locked, false));
        let overlay = CursorPolicy { overlay_open: true, ..confined };
        assert_eq!(overlay.mode(), (CursorGrabMode::None, true));
    }
}
