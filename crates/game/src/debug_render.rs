//! Collider wireframe debug view: an engine-neutral `DebugRenderBackend`
//! that collects rapier's debug-render line callbacks into plain buffers,
//! radius-filtered around the viewer. Consumed by the client, which ships
//! the buffers to the render thread per region. Not sim state — nothing
//! here may touch GameData, the rollback log, or the wire protocol. See
//! docs/superpowers/specs/2026-07-08-collider-debug-render-design.md.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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

    fn draw_line(&mut self, _object: DebugRenderObject, a: Vector, b: Vector, color: DebugColor) {
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
