//! Collider wireframe debug view: an engine-neutral `DebugRenderBackend`
//! that collects rapier's debug-render line callbacks into plain buffers,
//! radius-filtered around the viewer. Consumed by the client, which ships
//! the buffers to the render thread per region. Not sim state — nothing
//! here may touch GameData, the rollback log, or the wire protocol. See
//! docs/superpowers/specs/2026-07-08-collider-debug-render-design.md.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rapier3d::geometry::Aabb;
use rapier3d::math::Vector;
use rapier3d::pipeline::{
    DebugColor, DebugRenderBackend, DebugRenderMode, DebugRenderObject, DebugRenderPipeline,
    DebugRenderStyle,
};
use rapier3d::prelude::{ColliderSet, RigidBodySet};

/// Only colliders whose AABB comes within this distance (world units) of the
/// viewer get debug wireframes. Keeps gizmo counts sane in dense
/// chisel-scale geometry; tune here.
pub const DEBUG_COLLIDER_RADIUS: f32 = 64.0;

/// One wireframe segment, region-local coordinates. `color` is HSLA exactly
/// as rapier's `DebugRenderStyle` emits it (fixed vs dynamic bodies get
/// different hues for free); the render side converts with `Color::hsla`.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugLine {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub color: [f32; 4],
}

/// Process-local debug toggles shared between the render thread (F3 dev panel)
/// and the game thread (line generation). Deliberately NOT sim state: never
/// serialized, never hashed, never in the rollback log. Clones share state.
#[derive(Clone, Default, Debug)]
pub struct DebugFlags {
    collider_wireframes: Arc<AtomicBool>,
}

impl DebugFlags {
    pub fn set_collider_wireframes(&self, on: bool) {
        self.collider_wireframes.store(on, Ordering::Relaxed);
    }
    pub fn collider_wireframes(&self) -> bool {
        self.collider_wireframes.load(Ordering::Relaxed)
    }
}

/// Squared distance from a point to an AABB (0 inside).
fn aabb_dist_sq(aabb: &Aabb, p: &Vector) -> f32 {
    let mut d2 = 0.0;
    for i in 0..3 {
        let c = p[i].clamp(aabb.mins[i], aabb.maxs[i]);
        let d = c - p[i];
        d2 += d * d;
    }
    d2
}

/// `DebugRenderBackend` that buffers lines and drops colliders whose AABB is
/// beyond `DEBUG_COLLIDER_RADIUS` of the viewer.
struct LineCollector {
    lines: Vec<DebugLine>,
    viewer: Vector,
    max_dist_sq: f32,
}

impl DebugRenderBackend for LineCollector {
    fn filter_object(&self, object: DebugRenderObject) -> bool {
        match object {
            DebugRenderObject::Collider(_, co) => {
                aabb_dist_sq(&co.compute_aabb(), &self.viewer) <= self.max_dist_sq
            }
            // Only COLLIDER_SHAPES mode runs, so nothing else is offered.
            _ => true,
        }
    }

    fn draw_line(
        &mut self,
        _object: DebugRenderObject,
        a: Vector,
        b: Vector,
        color: DebugColor,
    ) {
        self.lines.push(DebugLine {
            a: [a.x, a.y, a.z],
            b: [b.x, b.y, b.z],
            color,
        });
    }
}

/// Reusable wrapper around rapier's `DebugRenderPipeline` (the pipeline
/// caches unit outlines for curved shapes, so keep one alive per manager
/// instead of rebuilding per call).
pub struct ColliderDebug {
    pipeline: DebugRenderPipeline,
}

impl Default for ColliderDebug {
    fn default() -> Self {
        Self {
            pipeline: DebugRenderPipeline::new(
                DebugRenderStyle::default(),
                DebugRenderMode::COLLIDER_SHAPES,
            ),
        }
    }
}

impl ColliderDebug {
    /// Wireframe every collider within `DEBUG_COLLIDER_RADIUS` of `viewer`.
    /// All coordinates (input viewer, output lines) are region-local.
    pub fn collect(
        &mut self,
        bodies: &RigidBodySet,
        colliders: &ColliderSet,
        viewer: [f32; 3],
    ) -> Vec<DebugLine> {
        let mut backend = LineCollector {
            lines: Vec::new(),
            viewer: Vector::from_array(viewer),
            max_dist_sq: DEBUG_COLLIDER_RADIUS * DEBUG_COLLIDER_RADIUS,
        };
        self.pipeline
            .render_colliders(&mut backend, bodies, colliders);
        backend.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapier3d::math::Vector;
    use rapier3d::prelude::{ColliderBuilder, ColliderSet, RigidBodyBuilder, RigidBodySet};

    /// A fixed body with a 2×2×2 cuboid at the origin, viewer at the origin.
    fn one_cuboid() -> (RigidBodySet, ColliderSet) {
        let mut bodies = RigidBodySet::new();
        let mut colliders = ColliderSet::new();
        let body = bodies.insert(RigidBodyBuilder::fixed().build());
        colliders.insert_with_parent(
            ColliderBuilder::cuboid(1.0, 1.0, 1.0).build(),
            body,
            &mut bodies,
        );
        (bodies, colliders)
    }

    #[test]
    fn cuboid_emits_wireframe_lines_near_viewer() {
        let (bodies, colliders) = one_cuboid();
        let mut dbg = ColliderDebug::default();
        let lines = dbg.collect(&bodies, &colliders, [0.0, 0.0, 0.0]);
        // A cuboid outline is 12 edges; don't pin the exact count (style
        // subdivisions could change), just that it's a real wireframe whose
        // endpoints all sit on the unit cuboid's surface envelope.
        assert!(lines.len() >= 12, "expected a wireframe, got {} lines", lines.len());
        for l in &lines {
            for p in [l.a, l.b] {
                assert!(
                    p.iter().all(|c| c.abs() <= 1.0 + 1e-3),
                    "endpoint {:?} escapes the 2x2x2 cuboid",
                    p
                );
            }
        }
    }

    #[test]
    fn radius_filter_excludes_far_colliders() {
        let (mut bodies, mut colliders) = one_cuboid();
        let near_only = {
            let mut dbg = ColliderDebug::default();
            dbg.collect(&bodies, &colliders, [0.0, 0.0, 0.0]).len()
        };
        // Add a second cuboid far beyond DEBUG_COLLIDER_RADIUS.
        let far = bodies.insert(
            RigidBodyBuilder::fixed()
                .translation(Vector::new(1000.0, 0.0, 0.0))
                .build(),
        );
        colliders.insert_with_parent(
            ColliderBuilder::cuboid(1.0, 1.0, 1.0).build(),
            far,
            &mut bodies,
        );
        let mut dbg = ColliderDebug::default();
        let lines = dbg.collect(&bodies, &colliders, [0.0, 0.0, 0.0]);
        assert_eq!(lines.len(), near_only, "far collider must be filtered out");
        // Viewer standing next to the far collider sees it (and not the near one).
        let lines_far = dbg.collect(&bodies, &colliders, [1000.0, 0.0, 0.0]);
        assert_eq!(lines_far.len(), near_only, "same shape, same line count");
        assert!(lines_far.iter().all(|l| l.a[0] > 500.0 && l.b[0] > 500.0));
    }

    #[test]
    fn dynamic_bodies_render_with_distinct_color() {
        let (mut bodies, mut colliders) = one_cuboid();
        let fixed_only = {
            let mut dbg = ColliderDebug::default();
            dbg.collect(&bodies, &colliders, [0.0, 0.0, 0.0]).len()
        };
        // A dynamic ball next to the fixed cuboid (player capsule stand-in).
        let ball = bodies.insert(RigidBodyBuilder::dynamic().build());
        colliders.insert_with_parent(
            ColliderBuilder::ball(0.5).build(),
            ball,
            &mut bodies,
        );
        let mut dbg = ColliderDebug::default();
        let lines = dbg.collect(&bodies, &colliders, [0.0, 0.0, 0.0]);
        assert!(lines.len() > fixed_only, "ball outline must add lines");
        // Fixed vs dynamic bodies get different HSLA colors from the style.
        let colors: std::collections::BTreeSet<[u32; 4]> =
            lines.iter().map(|l| l.color.map(f32::to_bits)).collect();
        assert!(colors.len() >= 2, "expected distinct fixed/dynamic colors");
    }

    #[test]
    fn flags_default_off_and_toggle() {
        let flags = DebugFlags::default();
        assert!(!flags.collider_wireframes());
        flags.set_collider_wireframes(true);
        assert!(flags.collider_wireframes());
        // Clones share the same underlying atomic.
        let clone = flags.clone();
        clone.set_collider_wireframes(false);
        assert!(!flags.collider_wireframes());
    }
}
