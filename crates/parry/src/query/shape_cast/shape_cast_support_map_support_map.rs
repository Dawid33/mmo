use crate::math::{Pose, Real, Vector};
use crate::query::details;
use crate::query::details::ShapeCastOptions;
use crate::query::gjk::{self, VoronoiSimplex};
use crate::query::{ShapeCastHit, ShapeCastStatus};
use crate::shape::{RoundShapeRef, SupportMap};
use num::Zero;

// mmo-fork: the grazing-start policy (Task 3.2 uses it in the voxels cast).
/// Starts within this fraction of `target_distance` beyond it count as already
/// within it. Covers the time-of-impact error of the cast that parked the shape.
pub(crate) const GRAZING_START_MARGIN: Real = 0.01;

// mmo-fork:
/// Rounding noise of a closest-points normal, per unit of length.
const NORMAL_NOISE: Real = 1.0e-4;

/// Time of impacts between two support-mapped shapes under translational movement.
pub fn cast_shapes_support_map_support_map<G1, G2>(
    pos12: &Pose,
    vel12: Vector,
    g1: &G1,
    g2: &G2,
    options: ShapeCastOptions,
) -> Option<ShapeCastHit>
where
    G1: ?Sized + SupportMap,
    G2: ?Sized + SupportMap,
{
    // mmo-fork: grazing starts. A cast that starts on (or a hair either side
    // of) the target-distance surface is the *normal* case for a character
    // controller: it parks the shape exactly there every step. That start is
    // degenerate for the ray cast below (the ray origin lies on the rounded
    // shape's boundary): it misses, reports a TOI of a few 1e-3 or a normal
    // that is only the last search direction. Classify such starts from the
    // real distance instead: within `GRAZING_START_MARGIN` beyond the target
    // distance is a resting contact at TOI 0. Its status follows #106 (D8):
    // `Converged` unless the start is really within the target distance.
    if options.target_distance > 0.0
        && (options.compute_impact_geometry_on_penetration || !options.stop_at_penetration)
    {
        let within = options.target_distance * (1.0 + GRAZING_START_MARGIN);
        if let Some(contact) = details::contact_support_map_support_map(pos12, g1, g2, within) {
            // Closing in by less than the normal's own rounding noise is a
            // slide along the surface, not an approach.
            let closing = -contact.normal1.dot(vel12);
            if !options.stop_at_penetration && closing <= vel12.length() * NORMAL_NOISE {
                return None;
            }
            return Some(ShapeCastHit {
                time_of_impact: 0.0,
                normal1: contact.normal1,
                normal2: contact.normal2,
                witness1: contact.point1,
                witness2: contact.point2,
                status: if contact.dist < options.target_distance {
                    ShapeCastStatus::PenetratingOrWithinTargetDist
                } else {
                    ShapeCastStatus::Converged
                },
                subshape1: 0,
                subshape2: 0,
            });
        }
    }

    let gjk_result = if options.target_distance > 0.0 {
        let round_g1 = RoundShapeRef {
            inner_shape: g1,
            border_radius: options.target_distance,
        };
        gjk::directional_distance(pos12, &round_g1, g2, vel12, &mut VoronoiSimplex::new())
    } else {
        gjk::directional_distance(pos12, g1, g2, vel12, &mut VoronoiSimplex::new())
    };

    gjk_result.and_then(|(time_of_impact, normal1, witness1, witness2)| {
        if time_of_impact > options.max_time_of_impact {
            None
        } else if (options.compute_impact_geometry_on_penetration || !options.stop_at_penetration)
            && time_of_impact < 1.0e-4
        {
            // The GJK-derived normal becomes unreliable for very small TOIs — typically
            // when the cast starts at (or very close to) the `target_distance` boundary
            // with a direction nearly tangent to the contact. Fall back on the contact
            // query, which gives a robust closest-point normal and lets us detect the
            // case where the cast direction is actually moving away from the contact.
            let contact = details::contact_support_map_support_map(pos12, g1, g2, Real::MAX)?;
            let normal_vel = contact.normal1.dot(vel12);

            if !options.stop_at_penetration && normal_vel >= 0.0 {
                None
            } else {
                Some(ShapeCastHit {
                    time_of_impact,
                    normal1: contact.normal1,
                    normal2: contact.normal2,
                    witness1: contact.point1,
                    witness2: contact.point2,
                    // A cast starting exactly touching, neither penetrating nor within
                    // the target distance, converged like ball-ball does.
                    status: if contact.dist < options.target_distance {
                        ShapeCastStatus::PenetratingOrWithinTargetDist
                    } else {
                        ShapeCastStatus::Converged
                    },
                    subshape1: 0,
                    subshape2: 0,
                })
            }
        } else if let Some(contact) = (options.target_distance > 0.0)
            .then(|| {
                // mmo-fork: the ray cast's normal is only its last search
                // direction. The shapes sit `target_distance` apart at the
                // impact pose, where closest points give the true one.
                let pos12_hit = Pose::from_translation(vel12 * time_of_impact) * *pos12;
                details::contact_support_map_support_map(&pos12_hit, g1, g2, Real::MAX)
            })
            .flatten()
        {
            Some(ShapeCastHit {
                time_of_impact,
                normal1: contact.normal1,
                normal2: contact.normal2,
                witness1: contact.point1,
                witness2: contact.point2,
                status: ShapeCastStatus::Converged,
                subshape1: 0,
                subshape2: 0,
            })
        } else {
            Some(ShapeCastHit {
                time_of_impact,
                normal1,
                normal2: pos12.rotation.inverse() * -normal1,
                witness1: witness1 - normal1 * options.target_distance,
                witness2: pos12.inverse_transform_point(witness2),
                status: if time_of_impact.is_zero() {
                    // A zero TOI means either a real penetration (or a start within the
                    // target distance) or a mere touching contact; only the former is
                    // `PenetratingOrWithinTargetDist`, as for ball-ball.
                    match details::contact_support_map_support_map(pos12, g1, g2, Real::MAX) {
                        Some(contact) if contact.dist >= options.target_distance => {
                            ShapeCastStatus::Converged
                        }
                        _ => ShapeCastStatus::PenetratingOrWithinTargetDist,
                    }
                } else {
                    ShapeCastStatus::Converged
                },
                subshape1: 0,
                subshape2: 0,
            })
        }
    })
}
