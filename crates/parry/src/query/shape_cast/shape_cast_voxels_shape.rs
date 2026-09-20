use crate::bounding_volume::BoundingVolume;
use crate::math::{Isometry, Point, RawReal, Real, Translation, Vector, DIM};
use crate::query::shape_cast::shape_cast_support_map_support_map::GRAZING_START_MARGIN;
use crate::query::{QueryDispatcher, ShapeCastHit, ShapeCastOptions, ShapeCastStatus};
use crate::shape::{AxisMask, Cuboid, Shape, SupportMap, Voxels};
use na::Unit;

/// Looks for solid geometry a shape does not know about itself (a neighbouring
/// collider) just across one of its faces. See [`without_internal_edges`].
#[derive(Copy, Clone)]
pub struct InternalEdgeProbe<'a> {
    /// Whether a point, in the local space of the shape, is inside something solid.
    pub is_solid: &'a dyn Fn(&Point<Real>) -> bool,
    /// How far across a face, and how far below the surface it meets, to look.
    pub reach: Real,
}

/// Normal components smaller than this are noise and snapped to the axis.
const NORMAL_SNAP: RawReal = 1.0e-4;

/// Time Of Impact of a voxels shape with any other shape, under a translational movement.
pub fn cast_shapes_voxels_shape<D>(
    dispatcher: &D,
    pos12: &Isometry<Real>,
    vel12: &Vector<Real>,
    g1: &Voxels,
    g2: &dyn Shape,
    options: ShapeCastOptions,
) -> Option<ShapeCastHit>
where
    D: ?Sized + QueryDispatcher,
{
    cast_shapes_voxels_shape_with_probe(dispatcher, pos12, vel12, g1, g2, options, None)
}

/// Removes the components of a box hit's normal that point through a covered
/// face of that box (`normal1` and `witness1` in the box's axes).
///
/// Each voxel is cast against as a free-standing cuboid, so a shape resting on
/// (or grazing) a flat run of voxels reports the edges between them: normals
/// leaning along the surface that no real geometry has. A sliding character
/// follows such a normal into the floor. A face is covered unless it is in
/// `free_faces`, or when `probe` finds solid just across it at the contact
/// point (a neighbouring collider). A normal with no component left is kept
/// as it was: the hit is on the face itself, not on an edge.
pub fn without_internal_edges(
    normal1: Unit<Vector<Real>>,
    witness1: &Point<Real>,
    free_faces: AxisMask,
    probe: Option<InternalEdgeProbe>,
) -> Unit<Vector<Real>> {
    let snap = Real::from(NORMAL_SNAP);
    let leaning = |i: usize| normal1[i].0.abs() > NORMAL_SNAP;
    let mut fixed = normal1.into_inner();
    let mut changed = false;

    for i in 0..DIM {
        if fixed[i] == Real::from(0.0) {
            continue;
        }
        if !leaning(i) {
            fixed[i] = Real::from(0.0);
            changed = true;
            continue;
        }
        let positive = normal1[i] > Real::from(0.0);
        let face = AxisMask::from_bits_truncate(1 << (i * 2 + usize::from(!positive)));
        let covered = !free_faces.contains(face)
            || probe.is_some_and(|probe| {
                // Just across the face, and just behind every other face the
                // normal leans on: flush material there continues the surface
                // the contact is on.
                let mut p = *witness1;
                for j in 0..DIM {
                    if !leaning(j) {
                        continue;
                    }
                    let outward = if normal1[j] > Real::from(0.0) { probe.reach } else { -probe.reach };
                    p[j] += if j == i { outward } else { -outward };
                }
                (probe.is_solid)(&p)
            });
        if covered {
            fixed[i] = Real::from(0.0);
            changed = true;
        }
    }

    if !changed {
        return normal1;
    }
    Unit::try_new(fixed, snap).unwrap_or(normal1)
}

/// A lower bound of the distance between a voxel and a support-mapped shape:
/// their gap along the line between their centres. Two support evaluations,
/// against the GJK run it spares the many voxels under a shape's AABB that
/// are nowhere near the shape itself.
fn separation_bound(
    center: &Point<Real>,
    half_extents: &Vector<Real>,
    pos12: &Isometry<Real>,
    g2: &dyn SupportMap,
) -> Real {
    let Some(dir) = Unit::try_new(pos12.translation.vector - center.coords, Real::from(1.0e-6)) else {
        return Real::from(0.0);
    };
    let nearest2 = g2.support_point(pos12, &-*dir);
    (nearest2 - center).dot(&dir) - half_extents.dot(&dir.abs())
}

/// Does any face in `free_faces` look into a motion along `vel`?
fn faces_motion(free_faces: AxisMask, vel: &Vector<Real>) -> bool {
    (0..DIM).any(|i| {
        let facing = if vel[i] > Real::from(0.0) {
            1 << (i * 2 + 1)
        } else if vel[i] < Real::from(0.0) {
            1 << (i * 2)
        } else {
            return false;
        };
        free_faces.contains(AxisMask::from_bits_truncate(facing))
    })
}

/// [`cast_shapes_voxels_shape`] that also asks `probe` whether a voxel face
/// the shape believes exposed is covered by other geometry.
pub fn cast_shapes_voxels_shape_with_probe<D>(
    dispatcher: &D,
    pos12: &Isometry<Real>,
    vel12: &Vector<Real>,
    g1: &Voxels,
    g2: &dyn Shape,
    options: ShapeCastOptions,
    probe: Option<InternalEdgeProbe>,
) -> Option<ShapeCastHit>
where
    D: ?Sized + QueryDispatcher,
{
    use num_traits::Bounded;

    let mut hit = None;
    // The shape touches whatever comes within the target distance (and the
    // per-voxel cast counts a start slightly beyond it as within), so the
    // voxels in play are those its loosened AABB sweeps through. A shape
    // resting `target_distance` above a floor does not overlap the floor's
    // voxels at all otherwise.
    let start_aabb2_1 = g2
        .compute_aabb(pos12)
        .loosened(options.target_distance * Real::from(1.0 + GRAZING_START_MARGIN));
    // The separating-start filter of `stop_at_penetration: false` has to see
    // the corrected normal, so it is applied here rather than per voxel.
    let voxel_options = ShapeCastOptions {
        stop_at_penetration: true,
        compute_impact_geometry_on_penetration: options.compute_impact_geometry_on_penetration
            || !options.stop_at_penetration,
        ..options
    };

    let half_extents = g1.voxel_size() / Real::from(2.0);
    let cuboid = Cuboid::new(half_extents);
    let mut search_domain = g1.voxel_range_intersecting_local_aabb(&start_aabb2_1);

    // A shape already within the target distance of a voxel is the usual
    // start of a character's cast, and nothing can beat its time of impact of
    // zero: look for it first, which takes no ray cast at all. Same voxel
    // order as the casts below, so the same voxel wins as would there.
    if options.target_distance > Real::from(0.0)
        && (options.compute_impact_geometry_on_penetration || !options.stop_at_penetration)
    {
        let within = options.target_distance * Real::from(1.0 + GRAZING_START_MARGIN);
        let support_map2 = g2.as_support_map();
        for vox in g1.voxels_in_range(search_domain[0], search_domain[1]) {
            if vox.state.is_empty() {
                continue;
            }
            let center = g1.voxel_center(vox.grid_coords);
            if support_map2.is_some_and(|g2| {
                separation_bound(&center, &half_extents, pos12, g2) > within
            }) {
                continue;
            }
            let vox_pos12 = Translation::from(center).inverse() * pos12;
            let Ok(Some(contact)) = dispatcher.contact(&vox_pos12, &cuboid, g2, within) else {
                continue;
            };
            let witness1 = contact.point1 + center.coords;
            let normal1 =
                without_internal_edges(contact.normal1, &witness1, vox.state.free_faces(), probe);
            if !options.stop_at_penetration && normal1.dot(vel12) >= Real::from(0.0) {
                continue;
            }
            return Some(ShapeCastHit {
                time_of_impact: Real::from(0.0),
                normal1,
                normal2: pos12.inverse_transform_unit_vector(&-normal1),
                witness1,
                witness2: contact.point2,
                status: ShapeCastStatus::PenetratingOrWithinTargetDist,
            });
        }
    }

    let smallest_t = core::cell::Cell::new(options.max_time_of_impact);
    let mut check_voxels_in_range = |search_domain: [Point<i32>; 2]| {
        for vox in g1.voxels_in_range(search_domain[0], search_domain[1]) {
            // A voxel is reached through a face that looks into the motion.
            // With all of those covered, the neighbours covering them are
            // reached first (and are in this search too).
            if vox.state.is_empty() || !faces_motion(vox.state.free_faces(), vel12) {
                continue;
            }
            // PERF: could we check the canonical shape instead, and deduplicate accordingly?
            let center = g1.voxel_center(vox.grid_coords);
            let vox_pos12 = Translation::from(center).inverse() * pos12;
            let capped = ShapeCastOptions {
                max_time_of_impact: smallest_t.get(),
                ..voxel_options
            };
            if let Some(mut new_hit) = dispatcher
                .cast_shapes(&vox_pos12, vel12, &cuboid, g2, capped)
                .ok()
                .flatten()
            {
                if new_hit.time_of_impact >= smallest_t.get() {
                    continue;
                }
                let witness1 = new_hit.witness1 + center.coords;
                let normal1 = without_internal_edges(
                    new_hit.normal1,
                    &witness1,
                    vox.state.free_faces(),
                    probe,
                );
                if normal1 != new_hit.normal1 {
                    new_hit.normal1 = normal1;
                    new_hit.normal2 = pos12.inverse_transform_unit_vector(&-normal1);
                }
                if !options.stop_at_penetration
                    && new_hit.status == ShapeCastStatus::PenetratingOrWithinTargetDist
                    && new_hit.normal1.dot(vel12) >= Real::from(0.0)
                {
                    continue;
                }
                smallest_t.set(new_hit.time_of_impact);
                new_hit.witness1 = witness1;
                hit = Some(new_hit);
            }
        }
    };

    check_voxels_in_range(search_domain);

    // Run the propagation.
    let [domain_mins, domain_maxs] = g1.domain();

    loop {
        let search_domain_aabb = g1.voxel_range_aabb(search_domain[0], search_domain[1]);

        // Figure out if we should move the aabb up/down/right/left.
        #[cfg(feature = "dim2")]
        let ii = [0, 1];
        #[cfg(feature = "dim3")]
        let ii = [0, 1, 2];

        let toi = ii.map(|i| {
            if vel12[i] > Real::from(0.0) {
                let t = (search_domain_aabb.maxs[i] - start_aabb2_1.maxs[i]) / vel12[i];
                if t < Real::from(0.0) {
                    (Real::max_value(), true)
                } else {
                    (t, true)
                }
            } else if vel12[i] < Real::from(0.0) {
                let t = (search_domain_aabb.mins[i] - start_aabb2_1.mins[i]) / vel12[i];
                if t < Real::from(0.0) {
                    (Real::max_value(), false)
                } else {
                    (t, false)
                }
            } else {
                (Real::max_value(), false)
            }
        });

        #[cfg(feature = "dim2")]
        if toi[0].0 > options.max_time_of_impact && toi[1].0 > options.max_time_of_impact {
            break;
        }

        #[cfg(feature = "dim3")]
        if toi[0].0 > options.max_time_of_impact
            && toi[1].0 > options.max_time_of_impact
            && toi[2].0 > options.max_time_of_impact
        {
            break;
        }

        let imin = Vector::from(toi.map(|t| t.0)).imin();

        if toi[imin].0 > smallest_t.get() {
            // Whatever lies further is reached after the hit already found.
            break;
        }

        // Rows the swept AABB has fully left were all checked when it entered
        // them. Rows it still overlaps stay in the search: the next row added
        // across them would otherwise skip the voxels under its trailing part.
        let travelled = *vel12 * toi[imin].0;
        let trailing_lo = g1.voxel_at_point(start_aabb2_1.mins + travelled);
        let trailing_hi = g1.voxel_at_point(start_aabb2_1.maxs + travelled);
        for i in ii {
            if vel12[i] > Real::from(0.0) {
                search_domain[0][i] = search_domain[0][i]
                    .max(trailing_lo[i])
                    .min(search_domain[1][i] - 1);
            } else if vel12[i] < Real::from(0.0) {
                search_domain[1][i] = search_domain[1][i]
                    .min(trailing_hi[i] + 1)
                    .max(search_domain[0][i] + 1);
            }
        }

        if toi[imin].1 {
            search_domain[1][imin] += 1;

            if search_domain[1][imin] <= domain_maxs[imin] {
                // Check the voxels on the added row.
                let mut prev_row = search_domain[0];
                prev_row[imin] = search_domain[1][imin] - 1;

                let range_to_check = [prev_row, search_domain[1]];
                check_voxels_in_range(range_to_check);
            } else if search_domain[0][imin] >= domain_maxs[imin] {
                // Leaving the shape’s bounds.
                break;
            }
        } else {
            search_domain[0][imin] -= 1;

            if search_domain[0][imin] >= domain_mins[imin] {
                // Check the voxels on the added row.
                let mut next_row = search_domain[1];
                next_row[imin] = search_domain[0][imin] + 1;

                let range_to_check = [search_domain[0], next_row];
                check_voxels_in_range(range_to_check);
            } else if search_domain[1][imin] <= domain_mins[imin] {
                // Leaving the shape’s bounds.
                break;
            }
        }
    }

    hit
}

/// Time Of Impact of any shape with a composite shape, under a rigid motion (translation + rotation).
pub fn cast_shapes_shape_voxels<D>(
    dispatcher: &D,
    pos12: &Isometry<Real>,
    vel12: &Vector<Real>,
    g1: &dyn Shape,
    g2: &Voxels,
    options: ShapeCastOptions,
) -> Option<ShapeCastHit>
where
    D: ?Sized + QueryDispatcher,
{
    cast_shapes_voxels_shape(
        dispatcher,
        &pos12.inverse(),
        &-pos12.inverse_transform_vector(vel12),
        g2,
        g1,
        options,
    )
    .map(|time_of_impact| time_of_impact.swapped())
}
