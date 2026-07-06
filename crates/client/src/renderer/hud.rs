use bevy::prelude::*;
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

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::prelude::Vec3;
    use game::{ConnectionState, RegionCoords};

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
