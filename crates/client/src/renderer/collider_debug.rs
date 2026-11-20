//! Collider wireframe debug view, render side: per-region line buffers
//! shipped from the game thread (`ClientUpdateEvent::DebugLines`), drawn as
//! world-space gizmos while the F3 dev panel's collider toggle is on. See
//! docs/superpowers/specs/2026-07-08-collider-debug-render-design.md.
use std::collections::BTreeMap;

use bevy::prelude::*;
use game::{DebugLine, RegionId};

/// Latest collider wireframe buffer per region, region-local coordinates.
/// Written by `bridge::drain_client_updates` (replace-on-receipt); entries
/// drop on RemoveRegion; cleared wholesale when the toggle goes off.
#[derive(Resource, Default)]
pub struct DebugColliderLines(pub BTreeMap<RegionId, Vec<DebugLine>>);

/// Render-side handle to the shared debug toggles. The same
/// `game::DebugFlags` lives in the game thread's manager; the F3 dev panel
/// flips it here.
#[derive(Resource, Clone)]
pub struct DebugFlagsRes(pub game::DebugFlags);

/// Draw the buffered wireframes while the collider toggle is on, shifting
/// each region's lines by its offset in the `WrapFrame` (the sim is
/// region-local; the world offset exists only at this render boundary). When off, clear
/// the buffers once so stale geometry never flashes on the next toggle.
pub fn draw_collider_wireframes(
    flags: Res<DebugFlagsRes>,
    mut lines: ResMut<DebugColliderLines>,
    frame: Option<Res<crate::wrap_frame::WrapFrame>>,
    roots: Option<Res<super::bridge::RegionRoots>>,
    mut gizmos: Gizmos,
) {
    if !flags.0.collider_wireframes() {
        if !lines.0.is_empty() {
            lines.0.clear();
        }
        return;
    }
    let frame = frame.as_deref().copied().unwrap_or_default();
    for (id, buf) in &lines.0 {
        // Not for a tree the frame moved away from (`RegionRoots::drawn`).
        if roots.as_ref().is_some_and(|r| !r.drawn(*id)) {
            continue;
        }
        let off = frame.offset(*id);
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
