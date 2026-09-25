//! The one writer of the primary window's cursor grab. The sim asks for a
//! grab (fps-cam toggle via `SetFreeCam`, inventory via `SetCursorGrab`);
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
}

impl CursorPolicy {
    pub fn grabbed(&self) -> bool {
        self.sim_grab && !self.overlay_open
    }
}

/// Apply the policy to the window. Idempotent: writing `CursorOptions` trips
/// change detection, which re-issues the winit grab — an X11 re-grab loop when
/// the grab is contended — so only a real change is written.
pub fn apply_cursor_policy(
    policy: Res<CursorPolicy>,
    mut windows: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    let (grab_mode, visible) = if policy.grabbed() {
        (CursorGrabMode::Locked, false)
    } else {
        (CursorGrabMode::None, true)
    };
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
}
