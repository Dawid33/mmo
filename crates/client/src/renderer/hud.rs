use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use game::{ConnectionState, RegionCoords};

/// Region + connection status mirrored from the game thread via
/// `ClientUpdateEvent::HudStatus`. Written by `drain_client_updates`, read by
/// the F3 dev panel (`dev_ui`). Options are `None` until the first status arrives.
#[derive(Resource, Default)]
pub struct HudStatus {
    pub home_region: Option<RegionCoords>,
    pub viewer_region: Option<RegionCoords>,
    pub connection: ConnectionState,
}

/// Marker: root of the centered crosshair.
#[derive(Component)]
pub struct Crosshair;

/// Spawn the crosshair. Runs once at startup; the entity exists before any
/// camera does and simply does not render until the local player's
/// `IsDefaultUiCamera` appears.
///
/// The root is `Pickable::IGNORE`: UI picking is on (widget clicks in
/// `ui_input`), and the full-viewport crosshair container would otherwise
/// swallow clicks meant for widgets underneath it.
pub fn setup_hud(mut commands: Commands) {
    // Crosshair: full-viewport centering container with two thin white bars.
    commands
        .spawn((
            Crosshair,
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
        ))
        .with_children(|p| {
            // Horizontal bar.
            p.spawn((
                Node {
                    width: Val::Px(16.0),
                    height: Val::Px(2.0),
                    position_type: PositionType::Absolute,
                    ..default()
                },
                BackgroundColor(Color::WHITE),
            ));
            // Vertical bar.
            p.spawn((
                Node {
                    width: Val::Px(2.0),
                    height: Val::Px(16.0),
                    position_type: PositionType::Absolute,
                    ..default()
                },
                BackgroundColor(Color::WHITE),
            ));
        });
}

/// Show the crosshair only while the cursor is locked for mouse look (not
/// when free for menus, nor merely confined for the commander's edge pan).
pub fn update_crosshair_visibility(
    windows: Query<&CursorOptions, With<PrimaryWindow>>,
    mut crosshair: Query<&mut Visibility, With<Crosshair>>,
) {
    let grabbed = windows
        .iter()
        .next()
        .map(|c| c.grab_mode == CursorGrabMode::Locked)
        .unwrap_or(false);
    for mut vis in &mut crosshair {
        *vis = if grabbed { Visibility::Visible } else { Visibility::Hidden };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_hud_spawns_crosshair() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_systems(Startup, setup_hud);
        app.update();

        let w = app.world_mut();
        assert_eq!(w.query_filtered::<(), With<Crosshair>>().iter(w).count(), 1);
    }
}
