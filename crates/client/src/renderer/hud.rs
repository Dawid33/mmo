use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::ui::IsDefaultUiCamera;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use game::{ConnectionState, RegionCoords};

/// Region + connection status mirrored from the game thread via
/// `ClientUpdateEvent::HudStatus`. Written by `drain_client_updates`, read by
/// the debug overlay. Options are `None` until the first status arrives.
#[derive(Resource, Default)]
pub struct HudStatus {
    pub home_region: Option<RegionCoords>,
    pub viewer_region: Option<RegionCoords>,
    pub connection: ConnectionState,
}

/// Render the F3 debug overlay text. Pure so it is unit-testable without Bevy
/// systems. Coordinates print with 2 decimals; FPS rounds to a whole number.
pub fn format_debug_text(
    pos: Vec3,
    home: Option<RegionCoords>,
    viewer: Option<RegionCoords>,
    conn: ConnectionState,
    fps: Option<f64>,
) -> String {
    let region_line = match (home, viewer) {
        (Some(h), Some(v)) => format!("home ({}, {})  viewer ({}, {})", h.x, h.z, v.x, v.z),
        _ => "--".to_string(),
    };
    let status = match conn {
        ConnectionState::Connecting => "Connecting",
        ConnectionState::CatchingUp => "Catching up",
        ConnectionState::Ready => "Ready",
    };
    let fps_line = match fps {
        Some(f) => format!("{}", f.round() as i64),
        None => "--".to_string(),
    };
    format!(
        "XYZ: {:.2} / {:.2} / {:.2}\nRegion: {}\nStatus: {}\nFPS: {}",
        pos.x, pos.y, pos.z, region_line, status, fps_line
    )
}

/// Marker: root of the centered crosshair.
#[derive(Component)]
pub struct Crosshair;

/// Marker: root panel of the F3 debug overlay (toggled visibility).
#[derive(Component)]
pub struct DebugOverlay;

/// Marker: the `Text` entity inside the debug overlay.
#[derive(Component)]
pub struct DebugText;

/// Spawn the crosshair and (hidden) debug overlay. Runs once at startup; the
/// entities exist before any camera does and simply do not render until the
/// local player's `IsDefaultUiCamera` appears.
///
/// No `Pickable::IGNORE` here: the client does not enable the `bevy_picking`
/// feature, so no UI picking backend runs and these nodes cannot intercept
/// world clicks in the first place.
pub fn setup_hud(mut commands: Commands) {
    // Crosshair: full-viewport centering container with two thin white bars.
    commands
        .spawn((
            Crosshair,
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

    // Debug overlay: top-left translucent panel, hidden until F3.
    commands
        .spawn((
            DebugOverlay,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(4.0),
                left: Val::Px(4.0),
                padding: UiRect::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.5)),
            Visibility::Hidden,
        ))
        .with_children(|p| {
            p.spawn((
                DebugText,
                Text::new(""),
                TextFont { font_size: 14.0, ..default() },
                TextColor(Color::WHITE),
            ));
        });
}

/// F3 toggles the debug overlay's visibility.
pub fn toggle_debug(
    keys: Res<ButtonInput<KeyCode>>,
    mut overlay: Query<&mut Visibility, With<DebugOverlay>>,
) {
    if !keys.just_pressed(KeyCode::F3) {
        return;
    }
    for mut vis in &mut overlay {
        *vis = match *vis {
            Visibility::Hidden => Visibility::Visible,
            _ => Visibility::Hidden,
        };
    }
}

/// While the overlay is visible, rebuild its text from live state. The player
/// position is read straight off the local render camera (the one marked
/// `IsDefaultUiCamera`); its `GlobalTransform` already folds in the region
/// world offset, so this is the true world-space eye position. Falls back to
/// the origin before the camera exists.
pub fn update_debug_text(
    overlay: Query<&Visibility, With<DebugOverlay>>,
    mut text: Query<&mut Text, With<DebugText>>,
    status: Res<HudStatus>,
    camera: Query<&GlobalTransform, With<IsDefaultUiCamera>>,
    diagnostics: Res<DiagnosticsStore>,
) {
    // Cheap early-out when hidden.
    if overlay.iter().all(|v| *v == Visibility::Hidden) {
        return;
    }

    let pos = camera
        .iter()
        .next()
        .map(|t| t.translation())
        .unwrap_or(Vec3::ZERO);

    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed());

    let s = format_debug_text(pos, status.home_region, status.viewer_region, status.connection, fps);
    for mut t in &mut text {
        *t = Text::new(s.clone());
    }
}

/// Hide the crosshair whenever the cursor is ungrabbed (free-cam / menus).
pub fn update_crosshair_visibility(
    windows: Query<&CursorOptions, With<PrimaryWindow>>,
    mut crosshair: Query<&mut Visibility, With<Crosshair>>,
) {
    let grabbed = windows
        .iter()
        .next()
        .map(|c| c.grab_mode != CursorGrabMode::None)
        .unwrap_or(false);
    for mut vis in &mut crosshair {
        *vis = if grabbed { Visibility::Visible } else { Visibility::Hidden };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{ConnectionState, RegionCoords};

    #[test]
    fn setup_hud_spawns_markers() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_systems(Startup, setup_hud);
        app.update();

        let w = app.world_mut();
        assert_eq!(w.query_filtered::<(), With<Crosshair>>().iter(w).count(), 1);
        assert_eq!(w.query_filtered::<(), With<DebugOverlay>>().iter(w).count(), 1);
        assert_eq!(w.query_filtered::<(), With<DebugText>>().iter(w).count(), 1);
    }

    #[test]
    fn toggle_debug_flips_visibility() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .add_systems(Startup, setup_hud)
            .add_systems(Update, toggle_debug);
        app.update(); // runs setup_hud

        // Overlay starts Hidden.
        {
            let w = app.world_mut();
            let vis = w.query_filtered::<&Visibility, With<DebugOverlay>>().single(w).unwrap();
            assert_eq!(*vis, Visibility::Hidden);
        }

        // Press F3.
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::F3);
        app.update();
        {
            let w = app.world_mut();
            let vis = w.query_filtered::<&Visibility, With<DebugOverlay>>().single(w).unwrap();
            assert_eq!(*vis, Visibility::Visible);
        }
    }

    #[test]
    fn format_debug_text_full() {
        let s = format_debug_text(
            Vec3::new(12.345, 65.0, -8.1),
            Some(RegionCoords::new(0, 0)),
            Some(RegionCoords::new(0, 1)),
            ConnectionState::Ready,
            Some(143.4),
        );
        assert!(s.contains("XYZ: 12.35 / 65.00 / -8.10"), "got: {s}");
        assert!(s.contains("home (0, 0)"), "got: {s}");
        assert!(s.contains("viewer (0, 1)"), "got: {s}");
        assert!(s.contains("Status: Ready"), "got: {s}");
        assert!(s.contains("FPS: 143"), "got: {s}");
    }

    #[test]
    fn format_debug_text_missing_fields() {
        let s = format_debug_text(
            Vec3::ZERO,
            None,
            None,
            ConnectionState::Connecting,
            None,
        );
        assert!(s.contains("Region: --"), "got: {s}");
        assert!(s.contains("Status: Connecting"), "got: {s}");
        assert!(s.contains("FPS: --"), "got: {s}");
    }
}
