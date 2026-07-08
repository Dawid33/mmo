//! Collider wireframe debug view, render side: per-region line buffers
//! shipped from the game thread (`ClientUpdateEvent::DebugLines`), drawn as
//! world-space gizmos while the F3 overlay is visible. See
//! docs/superpowers/specs/2026-07-08-collider-debug-render-design.md.
use std::collections::BTreeMap;

use bevy::prelude::*;
use game::{DebugLine, RegionId};

use super::hud::DebugOverlay;

/// Latest collider wireframe buffer per region, region-local coordinates.
/// Written by `bridge::drain_client_updates` (replace-on-receipt); entries
/// drop on RemoveRegion; cleared wholesale when the overlay hides.
#[derive(Resource, Default)]
pub struct DebugColliderLines(pub BTreeMap<RegionId, Vec<DebugLine>>);

/// Render-side handle to the shared debug toggles. The same
/// `game::DebugFlags` lives in the game thread's manager; F3 flips it here.
#[derive(Resource, Clone)]
pub struct DebugFlagsRes(pub game::DebugFlags);

/// Draw the buffered wireframes while the F3 overlay is visible, shifting
/// each region's lines by its world offset (the sim is region-local; the
/// world offset exists only at this render boundary). When hidden, clear
/// the buffers once so stale geometry never flashes on the next toggle.
pub fn draw_collider_wireframes(
    overlay: Query<&Visibility, With<DebugOverlay>>,
    mut lines: ResMut<DebugColliderLines>,
    mut gizmos: Gizmos,
) {
    let visible = overlay.iter().any(|v| *v == Visibility::Visible);
    if !visible {
        if !lines.0.is_empty() {
            lines.0.clear();
        }
        return;
    }
    for (id, buf) in &lines.0 {
        let off = id.world_offset();
        let off = Vec3::new(off[0], off[1], off[2]);
        for l in buf {
            gizmos.line(
                Vec3::from(l.a) + off,
                Vec3::from(l.b) + off,
                // rapier's DebugRenderStyle emits HSLA, not RGBA.
                Color::hsla(l.color[0], l.color[1], l.color[2], l.color[3]),
            );
        }
    }
}
