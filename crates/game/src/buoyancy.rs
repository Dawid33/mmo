//! Buoyancy and drag of dynamic bodies in water (water plan Task 4).
//!
//! Every owned [`EntityKind::Body`] (generic bodies and stud bodies alike;
//! halo mirrors are kinematic and skipped) has, at the opening of each tick,
//! its constant user force and damping set from the share `f` of its AABB
//! height under the water surface ([`submerged_fraction`]):
//!
//! - user force `(0, (f·V·ρw − m)·G·UNITS_PER_METER, 0)` — its weight less
//!   the weight of the water it displaces (`V` its collider volume,
//!   [`ColliderSpec::volume_m3`]; `m` its mass from its kind's own source;
//!   `G` its kind's gravity);
//! - linear damping `f · WATER_LINEAR_DAMPING`, angular damping
//!   `max(base, f · WATER_ANGULAR_DAMPING)` (`base` its kind's own).
//!
//! Dry (`f == 0`) a body keeps EXACTLY what its kind's own code gives it —
//! the weight force bit for bit, no drag, its own angular damping — so dry
//! bodies behave as before. The pass writes only on change, through the
//! undo-safe `mutate_body_safe`. Nothing water-specific crosses a seam: the
//! force and damping ride `BodyState`, and the receiver recomputes them from
//! its own ∪ halo water.

use rapier3d::math::Vector;

use crate::body::{BODY_ANGULAR_DAMPING, BODY_GRAVITY};
use crate::mass_props::{mass_properties, BODY_DEFAULT_DENSITY};
use crate::movement::UNITS_PER_METER;
use crate::stud_body::{STUD_BODY_ANGULAR_DAMPING, STUD_BODY_GRAVITY};
use crate::{ColliderSpec, EntityKey, EntityKind, Rollback};

/// Density of water, kg/m³.
pub const WATER_DENSITY: f32 = 1000.0;
/// Linear damping (1/s) of a body wholly under water; scaled by the
/// submerged share. Near-critical for floating bodies at the game's 45 m/s²
/// gravity: at 2.0 a dropped 1 m cube leapt out of the water and still
/// bobbed ±0.5 block after 9 s.
pub const WATER_LINEAR_DAMPING: f32 = 6.0;
/// Angular damping (1/s) of a body wholly under water; scaled by the
/// submerged share, never below the body's own.
pub const WATER_ANGULAR_DAMPING: f32 = 1.0;

/// The share, in `[0, 1]`, of the AABB height `aabb_min.y..aabb_max.y`
/// (region-local units) below the water surface over the stud column under
/// `com`: the recipe's surface there, exact. The column may lie in the halo;
/// beyond it there is no water.
pub fn submerged_fraction(rb: &Rollback, aabb_min: Vector, aabb_max: Vector, com: Vector) -> f32 {
    let height = aabb_max.y - aabb_min.y;
    if height <= 0.0 {
        return 0.0;
    }
    match rb.stud_water_surface_units(com.x, com.z) {
        Some(surface) => ((surface - aabb_min.y) / height).clamp(0.0, 1.0),
        None => 0.0,
    }
}

impl Rollback {
    /// Sets every owned dynamic body's user force and damping from the
    /// water it is in (module docs). Runs at the opening of the tick, after
    /// `tick_immersion_safe`, before the physics step. Entities in key
    /// order; writes only on change.
    pub(crate) fn tick_buoyancy_safe(&mut self) {
        let bodies: Vec<EntityKey> = self
            .data
            .ecs
            .kind
            .iter()
            .filter_map(|(k, kind)| (*kind == Some(EntityKind::Body)).then_some(k))
            .collect();
        for e in bodies {
            let Some(handle) = *self.data.ecs.rigidbody.try_get(e) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            if !body.is_dynamic() {
                continue;
            }
            let Some(collider) = body.colliders().first().and_then(|c| self.data.physics.colliders.get(*c)) else {
                continue;
            };
            let aabb = collider.compute_aabb();
            // The column under its world centre of mass.
            let f = submerged_fraction(self, aabb.mins, aabb.maxs, body.center_of_mass());
            // Dry and already in its dry state (only this pass gives a body
            // linear damping, and only in water): nothing to do — and no mass
            // to compute.
            if f == 0.0 && body.linear_damping() == 0.0 {
                continue;
            }
            let Some(spec) = ColliderSpec::of_shape(collider.shape()) else { continue };
            // The kind's own mass, gravity and angular damping (as its spawn
            // code computes them, so dry is bit-exact).
            let (mass, gravity, base_angular) = match self.data.ecs.stud_body.try_get(e) {
                Some(sb) => (
                    crate::stud_body::stud_model_mass_properties(&sb.model).mass(),
                    STUD_BODY_GRAVITY,
                    STUD_BODY_ANGULAR_DAMPING,
                ),
                None => (mass_properties(&spec, BODY_DEFAULT_DENSITY).mass(), BODY_GRAVITY, BODY_ANGULAR_DAMPING),
            };
            let (force, linear, angular) = if f == 0.0 {
                (Vector::new(0.0, -mass * gravity * UNITS_PER_METER, 0.0), 0.0, base_angular)
            } else {
                let lift = f * spec.volume_m3() * WATER_DENSITY;
                (
                    Vector::new(0.0, (lift - mass) * gravity * UNITS_PER_METER, 0.0),
                    f * WATER_LINEAR_DAMPING,
                    base_angular.max(f * WATER_ANGULAR_DAMPING),
                )
            };
            let bits = |v: Vector| v.to_array().map(f32::to_bits);
            let changed = bits(body.user_force()) != bits(force)
                || body.linear_damping().to_bits() != linear.to_bits()
                || body.angular_damping().to_bits() != angular.to_bits();
            if changed {
                self.data.physics.bodies.mutate_body_safe(handle, |b| {
                    // Not woken: a body asleep in water stays asleep.
                    b.reset_forces(false);
                    b.add_force(force, false);
                    b.set_linear_damping(linear);
                    b.set_angular_damping(angular);
                });
            }
        }
    }
}
