use bevy::prelude::*;
use game::BlockId;

use super::brick::{BrickDag, RootEntryKind, RootHandle, BRICKS_PER_AXIS, BRICK_EDGE};
use crate::renderer::meshing::ao::{vertex_ao, AO_CURVE};

const CELL_EDGE: i32 = 32;
const DDA_EPSILON: f32 = 1.0e-5;

/// World-space ray. `t` follows `origin + direction * t`; callers that need
/// distance-valued `t` should supply a normalized direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    pub origin: Vec3,
    pub direction: Vec3,
}

impl Ray {
    pub const fn new(origin: Vec3, direction: Vec3) -> Self {
        Self { origin, direction }
    }

    pub fn at(self, t: f32) -> Vec3 {
        self.origin + self.direction * t
    }
}

/// CPU mirror of the immutable GPU metadata needed to trace one far cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TraceCell {
    pub origin: Vec3,
    pub factor: u32,
    pub root: RootHandle,
}

impl TraceCell {
    pub const fn new(origin: Vec3, factor: u32, root: RootHandle) -> Self {
        Self {
            origin,
            factor,
            root,
        }
    }

    pub fn max(self) -> Vec3 {
        self.origin + Vec3::splat(CELL_EDGE as f32 * self.factor as f32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SlabHit {
    pub entry_t: f32,
    pub exit_t: f32,
    pub entry_normal: IVec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FarHit {
    pub t: f32,
    pub position: Vec3,
    pub voxel: IVec3,
    pub normal: IVec3,
    pub material: BlockId,
    /// Repeat-addressed texture coordinates, matching the existing raster
    /// renderer's cell-local coordinate masks and 16-voxel block scale.
    pub uv: Vec2,
    pub corner_ao: [u8; 4],
    pub corner_shade: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NearestFarHit {
    pub cell_index: usize,
    pub hit: FarHit,
}

/// Intersects a ray with an axis-aligned box. Axis ties deliberately retain
/// X, then Y, then Z priority so CPU tests and WGSL can make the same choice.
pub fn intersect_aabb(ray: Ray, min: Vec3, max: Vec3, t_limit: f32) -> Option<SlabHit> {
    if ray.origin.is_nan()
        || ray.direction.is_nan()
        || min.is_nan()
        || max.is_nan()
        || t_limit.is_nan()
        || ray.direction == Vec3::ZERO
    {
        return None;
    }

    let mut entry_t = f32::NEG_INFINITY;
    let mut exit_t = f32::INFINITY;
    let mut entry_normal = IVec3::ZERO;
    for axis in 0..3 {
        let origin = ray.origin[axis];
        let direction = ray.direction[axis];
        if direction.abs() <= f32::EPSILON {
            if origin < min[axis] || origin > max[axis] {
                return None;
            }
            continue;
        }

        let a = (min[axis] - origin) / direction;
        let b = (max[axis] - origin) / direction;
        let near = a.min(b);
        let far = a.max(b);
        if near > entry_t {
            entry_t = near;
            entry_normal = axis_normal(axis, if direction > 0.0 { -1 } else { 1 });
        }
        exit_t = exit_t.min(far);
        if entry_t > exit_t {
            return None;
        }
    }

    let starts_inside = entry_t < 0.0;
    entry_t = entry_t.max(0.0);
    exit_t = exit_t.min(t_limit);
    if entry_t > exit_t || exit_t < 0.0 {
        return None;
    }
    if starts_inside {
        entry_normal = IVec3::ZERO;
    }
    Some(SlabHit {
        entry_t,
        exit_t,
        entry_normal,
    })
}

/// Traces a single cell using a 4^3 brick DDA and, only for non-empty bricks,
/// a clipped 8^3 voxel DDA. `peers` are consulted only for same-factor AO.
pub fn trace_cell(
    dag: &BrickDag,
    cell: &TraceCell,
    peers: &[TraceCell],
    ray: Ray,
    t_limit: f32,
) -> Option<FarHit> {
    if cell.factor == 0 {
        return None;
    }
    let root = dag.root(cell.root)?;
    let cell_hit = intersect_aabb(ray, cell.origin, cell.max(), t_limit)?;
    let factor = cell.factor as f32;
    let brick_size = BRICK_EDGE as f32 * factor;
    let mut bricks = DdaState::new(
        ray,
        cell.origin,
        brick_size,
        BRICKS_PER_AXIS as i32,
        cell_hit.entry_t,
        cell_hit.entry_normal,
    )?;

    while bricks.in_bounds(BRICKS_PER_AXIS as i32) && bricks.t <= cell_hit.exit_t + DDA_EPSILON {
        let brick = bricks.coord;
        let root_index = brick.x as usize
            + BRICKS_PER_AXIS * brick.y as usize
            + BRICKS_PER_AXIS * BRICKS_PER_AXIS * brick.z as usize;
        let entry = root.entries[root_index];
        let brick_exit = bricks.next_t().min(cell_hit.exit_t);
        if !matches!(entry.kind(), RootEntryKind::Empty) {
            if let Some(hit) = trace_nonempty_brick(
                dag,
                cell,
                peers,
                ray,
                BrickSegment {
                    brick,
                    entry_t: bricks.t,
                    exit_t: brick_exit,
                    entry_normal: bricks.normal,
                },
            ) {
                return Some(hit);
            }
        }
        if bricks.next_t() > cell_hit.exit_t + DDA_EPSILON {
            break;
        }
        bricks.advance();
    }
    None
}

/// Traces all supplied proxy cells and returns the smallest world-ray `t`.
/// Equal-depth overlaps retain the lower cell index deterministically.
pub fn trace_nearest(
    dag: &BrickDag,
    cells: &[TraceCell],
    ray: Ray,
    t_limit: f32,
) -> Option<NearestFarHit> {
    let mut nearest: Option<NearestFarHit> = None;
    for (cell_index, cell) in cells.iter().enumerate() {
        let limit = nearest.map_or(t_limit, |nearest| nearest.hit.t.min(t_limit));
        let Some(hit) = trace_cell(dag, cell, cells, ray, limit) else {
            continue;
        };
        if nearest.is_none_or(|nearest| hit.t < nearest.hit.t) {
            nearest = Some(NearestFarHit { cell_index, hit });
        }
    }
    nearest
}

#[derive(Clone, Copy)]
struct BrickSegment {
    brick: IVec3,
    entry_t: f32,
    exit_t: f32,
    entry_normal: IVec3,
}

fn trace_nonempty_brick(
    dag: &BrickDag,
    cell: &TraceCell,
    peers: &[TraceCell],
    ray: Ray,
    segment: BrickSegment,
) -> Option<FarHit> {
    let mut voxels = DdaState::new(
        ray,
        cell.origin,
        cell.factor as f32,
        CELL_EDGE,
        segment.entry_t,
        segment.entry_normal,
    )?;
    while voxels.in_bounds(CELL_EDGE)
        && voxel_brick(voxels.coord) == segment.brick
        && voxels.t <= segment.exit_t + DDA_EPSILON
    {
        let material = material_at(dag, cell.root, voxels.coord);
        if material != BlockId::AIR {
            let position = ray.at(voxels.t);
            let normal = fallback_inside_normal(voxels.normal, ray.direction);
            let corner_ao = face_corner_ao(dag, cell, peers, voxels.coord, normal);
            return Some(FarHit {
                t: voxels.t,
                position,
                voxel: voxels.coord,
                normal,
                material,
                uv: face_uv((position - cell.origin) / cell.factor as f32, normal),
                corner_ao,
                corner_shade: corner_ao.map(|ao| AO_CURVE[ao as usize]),
            });
        }
        if voxels.next_t() > segment.exit_t + DDA_EPSILON {
            break;
        }
        voxels.advance();
    }
    None
}

fn material_at(dag: &BrickDag, root_handle: RootHandle, voxel: IVec3) -> BlockId {
    if voxel.x < 0 || voxel.y < 0 || voxel.z < 0 {
        return BlockId::AIR;
    }
    if voxel.x >= CELL_EDGE || voxel.y >= CELL_EDGE || voxel.z >= CELL_EDGE {
        return BlockId::AIR;
    }
    let root = match dag.root(root_handle) {
        Some(root) => root,
        None => return BlockId::AIR,
    };
    let brick = voxel_brick(voxel);
    let root_index = brick.x as usize
        + BRICKS_PER_AXIS * brick.y as usize
        + BRICKS_PER_AXIS * BRICKS_PER_AXIS * brick.z as usize;
    let entry = root.entries[root_index];
    match entry.kind() {
        RootEntryKind::Empty => BlockId::AIR,
        RootEntryKind::Uniform(material) => material,
        RootEntryKind::Mixed { .. } => {
            let local = voxel - brick * BRICK_EDGE as i32;
            let local_index = local.x as usize
                + BRICK_EDGE * local.y as usize
                + BRICK_EDGE * BRICK_EDGE * local.z as usize;
            dag.mixed_brick(entry)
                .map_or(BlockId::AIR, |mixed| mixed.material(local_index))
        }
    }
}

fn face_corner_ao(
    dag: &BrickDag,
    cell: &TraceCell,
    peers: &[TraceCell],
    voxel: IVec3,
    normal: IVec3,
) -> [u8; 4] {
    let axis = normal_axis(normal);
    let (u_axis, v_axis) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let layer = voxel + normal;
    std::array::from_fn(|corner| {
        let du = if corner & 1 == 0 { -1 } else { 1 };
        let dv = if corner & 2 == 0 { -1 } else { 1 };
        let mut side_u = layer;
        side_u[u_axis] += du;
        let mut side_v = layer;
        side_v[v_axis] += dv;
        let mut diagonal = layer;
        diagonal[u_axis] += du;
        diagonal[v_axis] += dv;
        vertex_ao(
            sample_same_factor(dag, cell, peers, side_u) != BlockId::AIR,
            sample_same_factor(dag, cell, peers, side_v) != BlockId::AIR,
            sample_same_factor(dag, cell, peers, diagonal) != BlockId::AIR,
        )
    })
}

fn sample_same_factor(
    dag: &BrickDag,
    cell: &TraceCell,
    peers: &[TraceCell],
    voxel: IVec3,
) -> BlockId {
    if voxel.x >= 0
        && voxel.y >= 0
        && voxel.z >= 0
        && voxel.x < CELL_EDGE
        && voxel.y < CELL_EDGE
        && voxel.z < CELL_EDGE
    {
        return material_at(dag, cell.root, voxel);
    }

    let offset = IVec3::new(
        voxel.x.div_euclid(CELL_EDGE),
        voxel.y.div_euclid(CELL_EDGE),
        voxel.z.div_euclid(CELL_EDGE),
    );
    if offset.x.abs() > 1 || offset.y.abs() > 1 || offset.z.abs() > 1 {
        return BlockId::AIR;
    }
    let expected_origin = cell.origin + offset.as_vec3() * (CELL_EDGE as f32 * cell.factor as f32);
    let Some(peer) = peers.iter().find(|peer| {
        peer.factor == cell.factor
            && (peer.origin - expected_origin).abs().max_element()
                <= DDA_EPSILON * cell.factor as f32
    }) else {
        return BlockId::AIR;
    };
    material_at(
        dag,
        peer.root,
        IVec3::new(
            voxel.x.rem_euclid(CELL_EDGE),
            voxel.y.rem_euclid(CELL_EDGE),
            voxel.z.rem_euclid(CELL_EDGE),
        ),
    )
}

fn face_uv(local_position: Vec3, normal: IVec3) -> Vec2 {
    match normal_axis(normal) {
        0 => Vec2::new(local_position.z, -local_position.y) / 16.0,
        1 => Vec2::new(local_position.x, local_position.z) / 16.0,
        _ => Vec2::new(local_position.x, -local_position.y) / 16.0,
    }
}

fn normal_axis(normal: IVec3) -> usize {
    if normal.x != 0 {
        0
    } else if normal.y != 0 {
        1
    } else {
        2
    }
}

fn fallback_inside_normal(normal: IVec3, direction: Vec3) -> IVec3 {
    if normal != IVec3::ZERO {
        return normal;
    }
    let abs = direction.abs();
    let axis = if abs.x >= abs.y && abs.x >= abs.z {
        0
    } else if abs.y >= abs.z {
        1
    } else {
        2
    };
    axis_normal(axis, if direction[axis] >= 0.0 { -1 } else { 1 })
}

fn axis_normal(axis: usize, sign: i32) -> IVec3 {
    let mut normal = IVec3::ZERO;
    normal[axis] = sign;
    normal
}

fn voxel_brick(voxel: IVec3) -> IVec3 {
    voxel / BRICK_EDGE as i32
}

#[derive(Clone, Copy)]
struct DdaState {
    coord: IVec3,
    step: IVec3,
    t_max: Vec3,
    t_delta: Vec3,
    t: f32,
    normal: IVec3,
}

impl DdaState {
    fn new(
        ray: Ray,
        grid_origin: Vec3,
        cell_size: f32,
        dimension: i32,
        entry_t: f32,
        entry_normal: IVec3,
    ) -> Option<Self> {
        if !cell_size.is_finite() || cell_size <= 0.0 || !entry_t.is_finite() {
            return None;
        }
        let point = ray.at(entry_t);
        let relative = (point - grid_origin) / cell_size;
        let mut coord = relative.floor().as_ivec3();
        for axis in 0..3 {
            let rounded = relative[axis].round();
            if ray.direction[axis] < 0.0 && (relative[axis] - rounded).abs() <= DDA_EPSILON {
                coord[axis] -= 1;
            }
            coord[axis] = coord[axis].clamp(0, dimension - 1);
        }

        let mut step = IVec3::ZERO;
        let mut t_max = Vec3::splat(f32::INFINITY);
        let mut t_delta = Vec3::splat(f32::INFINITY);
        for axis in 0..3 {
            let direction = ray.direction[axis];
            if direction > f32::EPSILON {
                step[axis] = 1;
                let boundary = grid_origin[axis] + (coord[axis] + 1) as f32 * cell_size;
                t_max[axis] = (boundary - ray.origin[axis]) / direction;
                t_delta[axis] = cell_size / direction;
            } else if direction < -f32::EPSILON {
                step[axis] = -1;
                let boundary = grid_origin[axis] + coord[axis] as f32 * cell_size;
                t_max[axis] = (boundary - ray.origin[axis]) / direction;
                t_delta[axis] = -cell_size / direction;
            }
        }
        Some(Self {
            coord,
            step,
            t_max,
            t_delta,
            t: entry_t,
            normal: entry_normal,
        })
    }

    fn in_bounds(self, dimension: i32) -> bool {
        self.coord.x >= 0
            && self.coord.y >= 0
            && self.coord.z >= 0
            && self.coord.x < dimension
            && self.coord.y < dimension
            && self.coord.z < dimension
    }

    fn next_axis(self) -> usize {
        if self.t_max.x <= self.t_max.y && self.t_max.x <= self.t_max.z {
            0
        } else if self.t_max.y <= self.t_max.z {
            1
        } else {
            2
        }
    }

    fn next_t(self) -> f32 {
        self.t_max[self.next_axis()]
    }

    fn advance(&mut self) {
        let axis = self.next_axis();
        self.t = self.t_max[axis];
        self.coord[axis] += self.step[axis];
        self.t_max[axis] += self.t_delta[axis];
        self.normal = axis_normal(axis, -self.step[axis]);
    }
}

#[cfg(test)]
mod tests {
    use bevy::prelude::*;
    use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

    use super::*;
    use crate::renderer::far_voxel::brick::{BrickDag, CellId, RootHandle};

    const EPSILON: f32 = 1.0e-5;

    fn voxel_index(x: usize, y: usize, z: usize) -> usize {
        x + 32 * y + 32 * 32 * z
    }

    fn root_for(dag: &mut BrickDag, id: u64, grid: &[Voxel]) -> RootHandle {
        dag.refresh_cell(CellId(id), grid)
            .root()
            .expect("test cell is resident")
    }

    fn sparse_grid(position: [usize; 3], material: BlockId) -> Vec<Voxel> {
        let mut grid = vec![Voxel::new(BlockId::AIR); CHUNK_VOXEL_COUNT];
        grid[voxel_index(position[0], position[1], position[2])] = Voxel::new(material);
        grid
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn slab_intersection_returns_entry_exit_and_outward_normal() {
        let hit = intersect_aabb(
            Ray::new(Vec3::new(-2.0, 1.0, 1.0), Vec3::X),
            Vec3::ZERO,
            Vec3::splat(2.0),
            f32::INFINITY,
        )
        .expect("ray enters box");

        assert_close(hit.entry_t, 2.0);
        assert_close(hit.exit_t, 4.0);
        assert_eq!(hit.entry_normal, IVec3::NEG_X);
    }

    #[test]
    fn slab_intersection_misses_parallel_ray_outside_box() {
        assert!(intersect_aabb(
            Ray::new(Vec3::new(-2.0, 3.0, 1.0), Vec3::X),
            Vec3::ZERO,
            Vec3::splat(2.0),
            f32::INFINITY,
        )
        .is_none());
    }

    #[test]
    fn hierarchy_skips_empty_bricks_and_hits_scaled_mixed_voxel() {
        let mut dag = BrickDag::default();
        let root = root_for(&mut dag, 1, &sparse_grid([20, 5, 6], BlockId(7)));
        let cell = TraceCell::new(Vec3::ZERO, 2, root);

        let hit = trace_cell(
            &dag,
            &cell,
            &[],
            Ray::new(Vec3::new(-10.0, 11.0, 13.0), Vec3::X),
            f32::INFINITY,
        )
        .expect("sparse voxel is hit after empty bricks");

        assert_close(hit.t, 50.0);
        assert_eq!(hit.voxel, IVec3::new(20, 5, 6));
        assert_eq!(hit.material, BlockId(7));
        assert_eq!(hit.normal, IVec3::NEG_X);
        assert_close(hit.uv.x, 6.5 / 16.0);
        assert_close(hit.uv.y, -5.5 / 16.0);
    }

    #[test]
    fn trace_misses_air_and_honors_the_near_depth_t_limit() {
        let mut dag = BrickDag::default();
        let root = root_for(&mut dag, 2, &sparse_grid([4, 5, 6], BlockId(9)));
        let cell = TraceCell::new(Vec3::ZERO, 2, root);
        let ray = Ray::new(Vec3::new(-10.0, 11.0, 13.0), Vec3::X);

        assert!(trace_cell(&dag, &cell, &[], ray, 17.999).is_none());
        assert!(trace_cell(
            &dag,
            &cell,
            &[],
            Ray::new(Vec3::new(-10.0, 30.0, 13.0), Vec3::X),
            f32::INFINITY,
        )
        .is_none());
        assert_close(
            trace_cell(&dag, &cell, &[], ray, 18.0)
                .expect("limit is inclusive at the surface")
                .t,
            18.0,
        );
    }

    #[test]
    fn all_six_hit_normals_point_out_of_the_occupied_voxel() {
        let mut dag = BrickDag::default();
        let root = root_for(&mut dag, 3, &sparse_grid([16, 16, 16], BlockId(4)));
        let cell = TraceCell::new(Vec3::ZERO, 1, root);
        let cases = [
            (Vec3::new(-1.0, 16.5, 16.5), Vec3::X, IVec3::NEG_X),
            (Vec3::new(34.0, 16.5, 16.5), Vec3::NEG_X, IVec3::X),
            (Vec3::new(16.5, -1.0, 16.5), Vec3::Y, IVec3::NEG_Y),
            (Vec3::new(16.5, 34.0, 16.5), Vec3::NEG_Y, IVec3::Y),
            (Vec3::new(16.5, 16.5, -1.0), Vec3::Z, IVec3::NEG_Z),
            (Vec3::new(16.5, 16.5, 34.0), Vec3::NEG_Z, IVec3::Z),
        ];

        for (origin, direction, normal) in cases {
            assert_eq!(
                trace_cell(&dag, &cell, &[], Ray::new(origin, direction), f32::INFINITY,)
                    .expect("center voxel is hit")
                    .normal,
                normal,
            );
        }
    }

    #[test]
    fn simultaneous_axis_entry_uses_deterministic_xyz_tie_priority() {
        let mut dag = BrickDag::default();
        let root = root_for(&mut dag, 4, &sparse_grid([0, 0, 0], BlockId(3)));
        let cell = TraceCell::new(Vec3::ZERO, 1, root);

        let hit = trace_cell(
            &dag,
            &cell,
            &[],
            Ray::new(Vec3::new(-1.0, -1.0, 0.5), Vec3::new(1.0, 1.0, 0.0)),
            f32::INFINITY,
        )
        .expect("ray enters the corner voxel");

        assert_eq!(hit.normal, IVec3::NEG_X);
    }

    #[test]
    fn same_factor_neighbor_contributes_face_edge_and_corner_ao() {
        let mut dag = BrickDag::default();
        let center_root = root_for(&mut dag, 5, &sparse_grid([31, 10, 10], BlockId(1)));
        let mut neighbor_grid = vec![Voxel::new(BlockId::AIR); CHUNK_VOXEL_COUNT];
        neighbor_grid[voxel_index(0, 9, 10)] = Voxel::new(BlockId(2));
        let neighbor_root = root_for(&mut dag, 6, &neighbor_grid);
        let center = TraceCell::new(Vec3::ZERO, 1, center_root);
        let neighbor = TraceCell::new(Vec3::new(32.0, 0.0, 0.0), 1, neighbor_root);

        let hit = trace_cell(
            &dag,
            &center,
            &[neighbor],
            Ray::new(Vec3::new(40.0, 10.5, 10.5), Vec3::NEG_X),
            f32::INFINITY,
        )
        .expect("border voxel is hit");

        assert_eq!(hit.normal, IVec3::X);
        assert_eq!(hit.corner_ao, [2, 3, 2, 3]);
        assert_eq!(hit.corner_shade, [0.85, 1.0, 0.85, 1.0]);
    }

    #[test]
    fn absent_or_different_factor_neighbor_counts_as_air_for_ao() {
        let mut dag = BrickDag::default();
        let center_root = root_for(&mut dag, 7, &sparse_grid([31, 10, 10], BlockId(1)));
        let different_root = root_for(&mut dag, 8, &sparse_grid([0, 9, 10], BlockId(2)));
        let center = TraceCell::new(Vec3::ZERO, 1, center_root);
        let different_factor = TraceCell::new(Vec3::new(32.0, 0.0, 0.0), 2, different_root);
        let ray = Ray::new(Vec3::new(40.0, 10.5, 10.5), Vec3::NEG_X);

        assert_eq!(
            trace_cell(&dag, &center, &[], ray, f32::INFINITY)
                .unwrap()
                .corner_ao,
            [3; 4]
        );
        assert_eq!(
            trace_cell(&dag, &center, &[different_factor], ray, f32::INFINITY,)
                .unwrap()
                .corner_ao,
            [3; 4]
        );
    }

    #[test]
    fn mixed_factor_adjacent_cells_share_a_boundary_and_nearest_hit_wins() {
        let mut dag = BrickDag::default();
        let left_root = root_for(&mut dag, 9, &sparse_grid([31, 0, 0], BlockId(11)));
        let right_root = root_for(&mut dag, 10, &sparse_grid([0, 0, 0], BlockId(12)));
        let cells = [
            TraceCell::new(Vec3::ZERO, 2, left_root),
            TraceCell::new(Vec3::new(64.0, 0.0, 0.0), 4, right_root),
        ];
        assert_close(cells[0].max().x, cells[1].origin.x);

        let nearest = trace_nearest(
            &dag,
            &cells,
            Ray::new(Vec3::new(60.0, 1.0, 1.0), Vec3::X),
            f32::INFINITY,
        )
        .expect("adjacent cells contain ray-visible material");

        assert_eq!(nearest.cell_index, 0);
        assert_eq!(nearest.hit.material, BlockId(11));
        assert_close(nearest.hit.t, 2.0);
    }
}
