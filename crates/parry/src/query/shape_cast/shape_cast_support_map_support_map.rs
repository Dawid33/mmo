use na::Unit;

use crate::math::{Isometry, Real, RawReal, Translation, Vector};
use crate::query::details;
use crate::query::details::ShapeCastOptions;
use crate::query::gjk::{self, VoronoiSimplex};
use crate::query::{ShapeCastHit, ShapeCastStatus};
use crate::shape::{RoundShapeRef, SupportMap};
use num::Zero;

/// Starts within this fraction of `target_distance` beyond it count as already
/// within it. Covers the time-of-impact error of the cast that parked the shape.
pub(crate) const GRAZING_START_MARGIN: RawReal = 0.01;

/// Rounding noise of a closest-points normal, per unit of length.
const NORMAL_NOISE: RawReal = 1.0e-4;

/// Time of impacts between two support-mapped shapes under translational movement.
pub fn cast_shapes_support_map_support_map<G1, G2>(
    pos12: &Isometry<Real>,
    vel12: &Vector<Real>,
    g1: &G1,
    g2: &G2,
    options: ShapeCastOptions,
) -> Option<ShapeCastHit>
where
    G1: ?Sized + SupportMap,
    G2: ?Sized + SupportMap,
{
    // A cast that starts on (or a hair either side of) the target-distance
    // surface is the *normal* case for a character controller: it parks the
    // shape exactly there every step. That start is degenerate for the ray
    // cast below (the ray origin lies on the rounded shape's boundary): it
    // misses outright or reports the raw GJK search direction as a normal,
    // several degrees off a flat face. Classify such starts from the real
    // distance instead.
    if options.target_distance > Real::from(0.0)
        && (options.compute_impact_geometry_on_penetration || !options.stop_at_penetration)
    {
        let within = options.target_distance * Real::from(1.0 + GRAZING_START_MARGIN);
        if let Some(contact) = details::contact_support_map_support_map(pos12, g1, g2, within) {
            // Closing in by less than the normal's own rounding noise is a
            // slide along the surface, not an approach.
            let closing = -contact.normal1.dot(vel12);
            if !options.stop_at_penetration
                && closing <= vel12.norm() * Real::from(NORMAL_NOISE)
            {
                return None;
            }
            return Some(ShapeCastHit {
                time_of_impact: Real::from(0.0),
                normal1: contact.normal1,
                normal2: contact.normal2,
                witness1: contact.point1,
                witness2: contact.point2,
                status: ShapeCastStatus::PenetratingOrWithinTargetDist,
            });
        }
    }

    let gjk_result = if options.target_distance > Real::from(0.0) {
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
            && time_of_impact < Real::from(1.0e-5)
        {
            let contact = details::contact_support_map_support_map(pos12, g1, g2, Real::from(RawReal::MAX))?;
            let normal_vel = contact.normal1.dot(vel12);

            if !options.stop_at_penetration && normal_vel >= Real::from(0.0) {
                None
            } else {
                Some(ShapeCastHit {
                    time_of_impact,
                    normal1: contact.normal1,
                    normal2: contact.normal2,
                    witness1: contact.point1,
                    witness2: contact.point2,
                    status: ShapeCastStatus::PenetratingOrWithinTargetDist,
                })
            }
        } else if let Some(contact) = (options.target_distance > Real::from(0.0))
            .then(|| {
                // The ray cast's normal is only its last search direction.
                // The shapes sit `target_distance` apart at the impact pose,
                // where closest points give the true one.
                let pos12_hit = Translation::from(*vel12 * time_of_impact) * pos12;
                details::contact_support_map_support_map(&pos12_hit, g1, g2, Real::from(RawReal::MAX))
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
            })
        } else {
            Some(ShapeCastHit {
                time_of_impact,
                normal1: Unit::new_unchecked(normal1),
                normal2: Unit::new_unchecked(pos12.inverse_transform_vector(&-normal1)),
                witness1: witness1 - normal1 * options.target_distance,
                witness2: pos12.inverse_transform_point(&witness2),
                status: if time_of_impact.is_zero() {
                    ShapeCastStatus::PenetratingOrWithinTargetDist
                } else {
                    ShapeCastStatus::Converged
                },
            })
        }
    })
}
