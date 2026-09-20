use crate::bounding_volume::BoundingVolume;
use crate::math::{Isometry, Point, RawReal, Real, Translation, Vector, DIM};
use crate::query::shape_cast::shape_cast_support_map_support_map::GRAZING_START_MARGIN;
use crate::query::{QueryDispatcher, ShapeCastHit, ShapeCastOptions, ShapeCastStatus};
use crate::shape::{AxisMask, Cuboid, Shape, Voxels};
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
    let mut smallest_t = options.max_time_of_impact;
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

    let mut check_voxels_in_range = |search_domain: [Point<i32>; 2]| {
        for vox in g1.voxels_in_range(search_domain[0], search_domain[1]) {
            if !vox.state.is_empty() {
                // PERF: could we check the canonical shape instead, and deduplicate accordingly?
                let center = g1.voxel_center(vox.grid_coords);
                let cuboid = Cuboid::new(g1.voxel_size() / Real::from(2.0));
                let vox_pos12 = Translation::from(center).inverse() * pos12;
                if let Some(mut new_hit) = dispatcher
                    .cast_shapes(&vox_pos12, vel12, &cuboid, g2, voxel_options)
                    .ok()
                    .flatten()
                {
                    if new_hit.time_of_impact >= smallest_t {
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
                    smallest_t = new_hit.time_of_impact;
                    new_hit.witness1 = witness1;
                    hit = Some(new_hit);
                }
            }
        }
    };

    let mut search_domain = g1.voxel_range_intersecting_local_aabb(&start_aabb2_1);
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
