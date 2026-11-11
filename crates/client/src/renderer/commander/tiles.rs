//! The tile mirror: the home region's tiles on the main thread — stud chunks
//! as tiles, the stud ground recipes, designations and constructed tiles —
//! behind `game::TileSource`, so the fortress tools (hover pick, validity
//! preview, overlay) answer synchronously with the sim's own rules.
//!
//! Per mirrored stud chunk (the bridge's `SimStudChunk`) a table of
//! [`game::StudTile`]s (the sim's own `stud_chunk_tile_table`, rebuilt when the
//! chunk changes), else the stud recipe's implied ground, own region ∪ stud
//! halo — the read rule of `Rollback::stud_tile`, so walks planned over the
//! mirror see past the home region's edge as the sim does. The rest arrives as
//! messages from the bridge: a [`TileMirrorMsg::Reset`] per received region
//! snapshot, its `StudWorldMsg` (recipes and stud halo), then the sim's
//! `SetDesignations` / `SetConstructed` updates (do and undo alike are plain
//! writes). The non-chunk state is kept for every loaded region, so a home flip
//! switches without a re-send; chunks only for home.
//!
//! Tiles are 1.2 m tall: every tile ↔ unit conversion goes through
//! [`TileMirror::terrain`] ([`TileMirror::tile_ray`], `level.rs`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bevy::prelude::*;
use game::rapier3d::math::Vector;
use game::{
    ColumnCoords, Designation, RegionId, StudChunk, StudColumnGround, StudTile, TileFill, TilePos,
    TileSource, WorldChunkPos, REGION_BLOCKS, REGION_CHUNKS, STUD_CHUNK_STUDS, STUD_CHUNK_TILES, STUD_LAYER_TILES,
    TILE_PLATES, TILE_STUDS,
};

use crate::renderer::bridge::{RegionRoots, SimEntity};
use crate::renderer::hud::HudStatus;
use crate::renderer::stud::world::{StudGround, StudWorldMsg};
use crate::renderer::stud::SimStudChunk;

/// Tile state from the bridge, per region.
#[derive(Message, Debug, Clone)]
pub enum TileMirrorMsg {
    /// A region snapshot arrived: its designations and constructed tiles (its
    /// `StudWorldMsg` brings the recipes and the stud halo).
    Reset { region: RegionId, designations: Vec<(TilePos, Designation)>, constructed: Vec<TilePos> },
    /// Per tile: the new designation (`None` = cleared), applied in order.
    Designations { region: RegionId, changes: Vec<(TilePos, Option<Designation>)> },
    /// Per tile: whether it is now a construction, applied in order.
    Constructed { region: RegionId, changes: Vec<(TilePos, bool)> },
}

/// One mirrored stud chunk as tiles: the sim's tile table
/// (`game::stud_chunk_tile_table`) and, per tile holding tree wood, the tree
/// (`Rollback::stud_tree_at`'s rule: the first wood brick in chunk order).
#[derive(Debug, Clone, PartialEq)]
struct StudChunkTiles {
    tiles: Box<[StudTile]>,
    trees: BTreeMap<u16, u16>,
}

impl StudChunkTiles {
    fn of(chunk: &StudChunk) -> Self {
        let mut trees = BTreeMap::new();
        for b in chunk.bricks().filter(|b| b.data != 0 && b.material == game::MAT_WOOD) {
            let size = b.size();
            let first = [b.min[0] as usize / TILE_STUDS, b.min[1] as usize / TILE_PLATES, b.min[2] as usize / TILE_STUDS];
            let last = [
                (b.min[0] as usize + size[0] as usize - 1) / TILE_STUDS,
                (b.min[1] as usize + size[1] as usize - 1) / TILE_PLATES,
                (b.min[2] as usize + size[2] as usize - 1) / TILE_STUDS,
            ];
            for ty in first[1]..=last[1] {
                for tz in first[2]..=last[2] {
                    for tx in first[0]..=last[0] {
                        trees.entry(game::stud_tile_table_index([tx, ty, tz]) as u16).or_insert(b.data);
                    }
                }
            }
        }
        Self { tiles: game::stud_chunk_tile_table(chunk), trees }
    }
}

const _: () = assert!(STUD_CHUNK_TILES * STUD_LAYER_TILES * STUD_CHUNK_TILES <= u16::MAX as usize);

/// One region's non-chunk tile state.
#[derive(Debug, Clone, Default)]
struct RegionTiles {
    /// The ground recipes, by chunk column.
    stud_ground: StudGround,
    /// The stud halo's chunks as tiles, by world chunk (`y` = layer).
    stud_halo: BTreeMap<WorldChunkPos, StudChunkTiles>,
    /// The stud halo's recipes, by world chunk column (`y` 0).
    stud_halo_ground: BTreeMap<WorldChunkPos, StudColumnGround>,
    designations: BTreeMap<TilePos, Designation>,
    constructed: BTreeSet<TilePos>,
}

static NO_DESIGNATIONS: BTreeMap<TilePos, Designation> = BTreeMap::new();

#[derive(Resource, Default)]
pub struct TileMirror {
    home: Option<RegionId>,
    /// Every loaded region's state; only `home`'s is read.
    regions: BTreeMap<RegionId, RegionTiles>,
    /// Home region stud chunks (region-local `[x, layer, z]`) as tiles, with the
    /// bridge entity that supplied them.
    stud_chunks: HashMap<[i32; 3], (Entity, StudChunkTiles)>,
    stud_chunk_of: HashMap<Entity, [i32; 3]>,
    version: u64,
}

impl TileMirror {
    /// The mirrored (home) region.
    pub fn region(&self) -> Option<RegionId> {
        self.home
    }

    pub fn designations(&self) -> &BTreeMap<TilePos, Designation> {
        self.home_tiles().map_or(&NO_DESIGNATIONS, |t| &t.designations)
    }

    /// Whether tile `p` stops a pick ray: any tile with something in it.
    pub fn pick_solid(&self, p: TilePos) -> bool {
        self.stud_tile(p).fill() != TileFill::Open
    }

    /// `ray` (region-local sim units, direction normalised) in TILE space: the
    /// frame in which every tile is a `BLOCK_UNITS` cube, which is what
    /// `game::pick_level_cell` / `pick_visible_cell` / `raycast_blocks` walk:
    /// `y` is scaled by block height over tile height (tiles are as wide as blocks).
    pub fn tile_ray(&self, ray: (Vector, Vector)) -> (Vector, Vector) {
        tile_ray(ray)
    }

    /// The home stud tile at `p` (region-local; it may lie past the region's
    /// edge), by `Rollback::stud_tile`'s rule: the materialised chunk, else the
    /// recipe's implied ground; past the edge the stud halo's chunk, else the
    /// neighbour's recipe. Unknown reads as open.
    pub fn stud_tile(&self, p: TilePos) -> StudTile {
        let Some((chunk, local)) = game::split_stud_tile(p) else { return StudTile::default() };
        let Some(tiles) = self.home_tiles() else { return StudTile::default() };
        let n = REGION_CHUNKS as i32;
        let [cx, layer, cz] = chunk;
        if (0..n).contains(&cx) && (0..n).contains(&cz) {
            if let Some((_, c)) = self.stud_chunks.get(&chunk) {
                return c.tiles[game::stud_tile_table_index(local)];
            }
            return match tiles.stud_ground.get(&ColumnCoords::new(cx as usize, cz as usize)) {
                Some(g) => StudTile::of_ground(g, layer, local),
                None => StudTile::default(),
            };
        }
        let (Some(home), true) = (self.home, game::halo::in_halo_band(cx, cz)) else { return StudTile::default() };
        let key = WorldChunkPos::from_local(home, chunk);
        if let Some(c) = tiles.stud_halo.get(&key) {
            return c.tiles[game::stud_tile_table_index(local)];
        }
        match tiles.stud_halo_ground.get(&WorldChunkPos::new(key.x, 0, key.z)) {
            Some(g) => StudTile::of_ground(g, layer, local),
            None => StudTile::default(),
        }
    }

    /// `Rollback::stud_tree_at`: the tree whose wood is in home stud tile `p`.
    fn stud_tree_at(&self, p: TilePos) -> Option<u16> {
        let (chunk, local) = game::split_stud_tile(p)?;
        self.stud_chunks.get(&chunk)?.1.trees.get(&(game::stud_tile_table_index(local) as u16)).copied()
    }

    /// `Rollback::stud_column_band`: the tile levels worth scanning in stud tile
    /// column `[x, z]` — its mirrored chunks' layers and the recipe's surface.
    fn stud_column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        let tiles = self.home_tiles()?;
        let (n, l) = (STUD_CHUNK_TILES as i32, STUD_LAYER_TILES as i32);
        let (cx, cz) = (column[0].div_euclid(n), column[1].div_euclid(n));
        let (lx, lz) = (column[0].rem_euclid(n) as usize, column[1].rem_euclid(n) as usize);
        let mut band: Option<(i32, i32)> = None;
        let mut add = |lo: i32, hi: i32| band = Some(band.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        let ground_band = |g: &StudColumnGround| {
            let tp = TILE_PLATES as i32;
            let tops = (0..TILE_STUDS)
                .flat_map(|dz| (0..TILE_STUDS).map(move |dx| (dx, dz)))
                .map(|(dx, dz)| g.surface(lx * TILE_STUDS + dx, lz * TILE_STUDS + dz));
            let (lo, hi) = tops.fold((i32::MAX, i32::MIN), |(lo, hi), s| (lo.min(s), hi.max(s)));
            (lo.saturating_sub(1).div_euclid(tp), hi.saturating_sub(1).div_euclid(tp).saturating_add(1))
        };
        let rn = REGION_CHUNKS as i32;
        if (0..rn).contains(&cx) && (0..rn).contains(&cz) {
            for key in self.stud_chunks.keys().filter(|k| (k[0], k[2]) == (cx, cz)) {
                add(key[1] * l, key[1] * l + l - 1);
            }
            if let Some(g) = tiles.stud_ground.get(&ColumnCoords::new(cx as usize, cz as usize)) {
                let (lo, hi) = ground_band(g);
                add(lo, hi);
            }
        } else if game::halo::in_halo_band(cx, cz) {
            let key = WorldChunkPos::from_local(self.home?, [cx, 0, cz]);
            let layers = tiles.stud_halo.range(WorldChunkPos::new(key.x, i32::MIN, key.z)..=WorldChunkPos::new(key.x, i32::MAX, key.z));
            for (k, _) in layers {
                add(k.y * l, k.y * l + l - 1);
            }
            if let Some(g) = tiles.stud_halo_ground.get(&key) {
                let (lo, hi) = ground_band(g);
                add(lo, hi);
            }
        }
        band
    }

    /// The recipe of region stud column `[x, z]` (own, or the halo's past the
    /// edge) and the column inside it.
    fn stud_recipe_at(&self, column: [i64; 2]) -> Option<(&StudColumnGround, usize, usize)> {
        let tiles = self.home_tiles()?;
        let n = STUD_CHUNK_STUDS as i64;
        let (cx, cz) = (column[0].div_euclid(n), column[1].div_euclid(n));
        let (sx, sz) = (column[0].rem_euclid(n) as usize, column[1].rem_euclid(n) as usize);
        let rn = REGION_CHUNKS as i64;
        let g = if (0..rn).contains(&cx) && (0..rn).contains(&cz) {
            tiles.stud_ground.get(&ColumnCoords::new(cx as usize, cz as usize))?
        } else {
            tiles.stud_halo_ground.get(&WorldChunkPos::from_local(self.home?, [cx as i32, 0, cz as i32]))?
        };
        Some((g, sx, sz))
    }

    /// `Rollback::stud_water_units`: recipe water standing over stud tile `p`'s
    /// own solid top, out of a tile's plates.
    fn stud_water_units(&self, p: TilePos) -> u16 {
        let (ts, tp) = (TILE_STUDS as i64, TILE_PLATES as i64);
        let bottom = i64::from(p[1]) * tp;
        let mut level = 0;
        for dz in 0..ts {
            for dx in 0..ts {
                let column = [i64::from(p[0]) * ts + dx, i64::from(p[2]) * ts + dz];
                let span = self.stud_recipe_at(column).and_then(|(g, sx, sz)| {
                    let bed = g.surface(sx, sz);
                    g.water_top(sx, sz).filter(|&top| top > bed).map(|top| (bed, top))
                });
                if let Some((_, top)) = span.filter(|&(bed, _)| i64::from(bed) < bottom + tp) {
                    level = level.max((i64::from(top) - bottom).clamp(0, tp));
                }
            }
        }
        if level == 0 {
            return 0;
        }
        let solid = i64::from(self.stud_tile(p).floor_plates());
        if level <= solid {
            return 0;
        }
        (i64::from(game::WATER_FULL) * (level - solid) / tp) as u16
    }

    /// File a home stud chunk (region-local `[x, layer, z]`) as tiles.
    pub fn set_stud_chunk(&mut self, entity: Entity, key: [i32; 3], chunk: &StudChunk) {
        if let Some(old) = self.stud_chunk_of.insert(entity, key) {
            if old != key && self.stud_chunks.get(&old).is_some_and(|(owner, _)| *owner == entity) {
                self.stud_chunks.remove(&old);
            }
        }
        let tiles = StudChunkTiles::of(chunk);
        let same = self.stud_chunks.get(&key).is_some_and(|(_, old)| *old == tiles);
        self.stud_chunks.insert(key, (entity, tiles));
        if !same {
            self.version += 1;
        }
    }

    /// A stud chunk entity went away: drop its tiles unless another entity
    /// holds that key by now.
    fn remove_stud_entity(&mut self, entity: Entity) {
        let Some(key) = self.stud_chunk_of.remove(&entity) else { return };
        if self.stud_chunks.get(&key).is_some_and(|(owner, _)| *owner == entity) {
            self.stud_chunks.remove(&key);
            self.version += 1;
        }
    }

    /// Apply one stud mirror message (after the region's `Reset`): the region's
    /// stud recipes and its stud halo. Updates for a region
    /// without a `Reset` yet are dropped.
    pub fn apply_stud(&mut self, msg: &StudWorldMsg) {
        let halo = |tiles: &mut RegionTiles,
                    chunks: &[(WorldChunkPos, Option<StudChunk>)],
                    ground: &[(WorldChunkPos, Option<StudColumnGround>)]| {
            for (k, chunk) in chunks {
                match chunk {
                    Some(c) => tiles.stud_halo.insert(*k, StudChunkTiles::of(c)),
                    None => tiles.stud_halo.remove(k),
                };
            }
            for (k, g) in ground {
                match g {
                    Some(g) => tiles.stud_halo_ground.insert(*k, g.clone()),
                    None => tiles.stud_halo_ground.remove(k),
                };
            }
        };
        let region = match msg {
            StudWorldMsg::Region { region, ground, halo: chunks, halo_ground, .. } => {
                let Some(tiles) = self.regions.get_mut(region) else { return };
                tiles.stud_ground = ground.clone();
                tiles.stud_halo.clear();
                tiles.stud_halo_ground.clear();
                halo(tiles, chunks, halo_ground);
                *region
            }
            StudWorldMsg::Halo { region, chunks, ground } => {
                let Some(tiles) = self.regions.get_mut(region) else { return };
                halo(tiles, chunks, ground);
                *region
            }
        };
        if self.home == Some(region) {
            self.version += 1;
        }
    }

    /// Bumped on any change to what the mirror answers.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Whether world block column `(x, z)` lies in the home region.
    pub fn in_home(&self, x: i32, z: i32) -> bool {
        let n = REGION_BLOCKS as i32;
        self.home == Some(game::RegionCoords::new(x.div_euclid(n), z.div_euclid(n)))
    }

    /// Switch the mirrored region; `true` when it changed (chunks dropped,
    /// to be re-read for the new home).
    pub(crate) fn set_home(&mut self, home: Option<RegionId>) -> bool {
        if self.home == home {
            return false;
        }
        self.home = home;
        self.stud_chunks.clear();
        self.stud_chunk_of.clear();
        self.version += 1;
        true
    }

    /// Drop the state of regions no longer loaded. Losing the home region's
    /// changes what the mirror answers (no designations, no ground), so that
    /// bumps the version — the overlay rebuilds on it.
    fn prune_regions(&mut self, loaded: impl Fn(&RegionId) -> bool) {
        let had_home = self.home_tiles().is_some();
        self.regions.retain(|r, _| loaded(r));
        if had_home && self.home_tiles().is_none() {
            self.version += 1;
        }
    }

    fn home_tiles(&self) -> Option<&RegionTiles> {
        self.regions.get(&self.home?)
    }

    /// Apply one bridge message. Updates for a region without a `Reset`
    /// yet are dropped (its snapshot will carry them).
    pub fn apply(&mut self, msg: TileMirrorMsg) {
        let region = match msg {
            TileMirrorMsg::Reset { region, designations, constructed } => {
                let tiles = RegionTiles {
                    designations: designations.into_iter().collect(),
                    constructed: constructed.into_iter().collect(),
                    ..RegionTiles::default()
                };
                self.regions.insert(region, tiles);
                region
            }
            TileMirrorMsg::Designations { region, changes } => {
                let Some(tiles) = self.regions.get_mut(&region) else { return };
                for (p, d) in changes {
                    match d {
                        Some(d) => tiles.designations.insert(p, d),
                        None => tiles.designations.remove(&p),
                    };
                }
                region
            }
            TileMirrorMsg::Constructed { region, changes } => {
                let Some(tiles) = self.regions.get_mut(&region) else { return };
                for (p, built) in changes {
                    if built {
                        tiles.constructed.insert(p);
                    } else {
                        tiles.constructed.remove(&p);
                    }
                }
                region
            }
        };
        if self.home == Some(region) {
            self.version += 1;
        }
    }
}

/// Same rules as `impl TileSource for Rollback` on stud terrain, over the
/// mirrored data.
impl TileSource for TileMirror {
    /// Stud terrain has no blocks: tools read [`TileSource::tile_fill`].


    fn tile_fill(&self, p: TilePos) -> TileFill {
        self.stud_tile(p).fill()
    }

    fn tile_material(&self, p: TilePos) -> Option<u16> {
        self.stud_tile(p).material()
    }

    fn is_bedrock(&self, p: TilePos) -> bool {
        self.stud_tile(p).is_bedrock()
    }

    fn is_constructed(&self, p: TilePos) -> bool {
        self.home_tiles().is_some_and(|t| t.constructed.contains(&p))
    }

    /// The tree whose wood is in the tile.
    fn tree_id(&self, p: TilePos) -> u32 {
        self.stud_tree_at(p).map_or(0, u32::from)
    }


    fn water_units(&self, p: TilePos) -> u16 {
        self.stud_water_units(p)
    }

    fn column_band(&self, column: [i32; 2]) -> Option<(i32, i32)> {
        self.stud_column_band(column)
    }
}

/// [`TileMirror::tile_ray`] for terrain `kind`.
pub fn tile_ray((origin, dir): (Vector, Vector)) -> (Vector, Vector) {
    let s = game::BLOCK_UNITS as f32 / game::tile_height_units();
    if s == 1.0 {
        return (origin, dir);
    }
    let d = Vec3::new(dir.x, dir.y * s, dir.z).normalize_or_zero();
    (Vector::new(origin.x, origin.y * s, origin.z), Vector::new(d.x, d.y, d.z))
}

/// Region-local `[x, layer, z]` of a stud chunk.
fn stud_chunk_key(c: game::StudChunkCoords) -> [i32; 3] {
    [c.x as i32, c.y, c.z as i32]
}

/// Keep the mirror in step with the bridge: its messages, the home region
/// and the home region's stud chunks.
pub fn update_tile_mirror(
    mut mirror: ResMut<TileMirror>,
    mut msgs: MessageReader<TileMirrorMsg>,
    status: Res<HudStatus>,
    roots: Res<RegionRoots>,
    (stud_msgs, mut stud_cursor): (Option<Res<Messages<StudWorldMsg>>>, Local<bevy::ecs::message::MessageCursor<StudWorldMsg>>),
    changed_studs: Query<(Entity, &SimStudChunk, &SimEntity), Changed<SimStudChunk>>,
    all_studs: Query<(Entity, &SimStudChunk, &SimEntity)>,
    mut removed_studs: RemovedComponents<SimStudChunk>,
) {
    for msg in msgs.read() {
        mirror.apply(msg.clone());
    }
    // After the `Reset`s: a region's stud state hangs off its `RegionTiles`.
    if let Some(stud_msgs) = stud_msgs {
        for msg in stud_cursor.read(&stud_msgs) {
            mirror.apply_stud(msg);
        }
    }
    mirror.prune_regions(|r| roots.0.contains_key(r));
    for entity in removed_studs.read() {
        mirror.remove_stud_entity(entity);
    }
    let Some(home) = status.home_region else {
        mirror.set_home(None);
        return;
    };
    let switched = mirror.set_home(Some(home));
    let studs: Vec<(Entity, &SimStudChunk)> = if switched {
        all_studs.iter().filter(|(_, _, sim)| sim.region == home).map(|(e, c, _)| (e, c)).collect()
    } else {
        changed_studs.iter().filter(|(_, _, sim)| sim.region == home).map(|(e, c, _)| (e, c)).collect()
    };
    for (entity, c) in studs {
        mirror.set_stud_chunk(entity, stud_chunk_key(c.coords), &c.chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{DesignationKind, OpId, RegionCoords};

    fn home() -> RegionId {
        RegionCoords::new(1, -2)
    }

    /// A mirror of `home()` after its (empty) snapshot.
    fn mirror() -> TileMirror {
        let mut m = TileMirror::default();
        m.set_home(Some(home()));
        m.apply(TileMirrorMsg::Reset { region: home(), designations: Vec::new(), constructed: Vec::new() });
        m
    }

    #[test]
    fn pruning_the_home_region_bumps_version() {
        let mut m = mirror();
        let v = m.version();
        m.prune_regions(|_| true);
        assert_eq!(m.version(), v, "nothing pruned");
        m.prune_regions(|r| *r != home());
        assert!(m.version() > v, "the home region's state went away");
        assert!(m.designations().is_empty());
        let v = m.version();
        m.prune_regions(|_| false);
        assert_eq!(m.version(), v, "the home region was already gone");
    }

    #[test]
    fn designation_messages_apply_and_bump_version() {
        let mut m = mirror();
        let d = Designation { kind: DesignationKind::Dig, op: OpId::default(), priority: 4 };
        let (a, b) = ([3, 10, 4], [5, 10, 4]);
        let v = m.version();
        m.apply(TileMirrorMsg::Designations { region: home(), changes: vec![(a, Some(d)), (b, Some(d))] });
        assert_eq!(m.designations().len(), 2);
        assert!(m.version() > v);
        // Undo sends the prior values the same way.
        m.apply(TileMirrorMsg::Designations { region: home(), changes: vec![(b, None)] });
        assert_eq!(m.designations().keys().copied().collect::<Vec<_>>(), vec![a]);
        m.apply(TileMirrorMsg::Constructed { region: home(), changes: vec![(b, true)] });
        assert!(m.is_constructed(b));
        // Another region's updates never touch the mirrored one.
        let v = m.version();
        m.apply(TileMirrorMsg::Designations { region: RegionCoords::new(9, 9), changes: vec![(b, Some(d))] });
        m.apply(TileMirrorMsg::Constructed { region: home(), changes: vec![(b, false)] });
        assert!(!m.is_constructed(b));
        assert_eq!(m.designations().len(), 1);
        assert!(m.version() > v);
    }

    #[test]
    fn home_flip_switches_region_state_and_drops_chunks() {
        let mut m = mirror();
        let other = RegionCoords::new(0, 0);
        let d = Designation { kind: DesignationKind::Dig, op: OpId::default(), priority: 4 };
        m.apply(TileMirrorMsg::Reset { region: other, designations: vec![([1, 1, 1], d)], constructed: Vec::new() });
        let chunk = StudChunk::new([game::StudBrick::filler([0, 0, 0], [2, 6, 2], game::MAT_STONE)]).unwrap();
        m.set_stud_chunk(Entity::from_raw_u32(1).unwrap(), [0, 0, 0], &chunk);
        assert_eq!(m.tile_fill([0, 0, 0]), TileFill::Solid);
        assert!(m.designations().is_empty());
        assert!(m.set_home(Some(other)));
        assert_eq!(m.region(), Some(other));
        assert_eq!(m.designations().len(), 1);
        assert_eq!(m.tile_fill([0, 0, 0]), TileFill::Open, "the old home's chunks are gone");
    }

    // --- Tiles ---------------------------------------------------------------

    mod stud {
        use super::super::*;
        use crate::renderer::commander::designate::{pick_level, pick_visible};
        use game::{
            plate_units, DesignationKind, GameEventKind, Order, Region, RegionCoords, Rollback, StudBrick, StudChunkCoords,
            StudProfile, StudRegionSeed, MAT_BEDROCK, MAT_DIRT, MAT_GRASS, MAT_STONE, STUD_CHUNK_PLATES, STUD_COLUMN_FOOTPRINT,
            STUD_NO_WATER,
        };

        /// Surface plate of the dry ground: the top of tile level 4.
        const TOP: i32 = 30;
        const BEDROCK: i32 = -12;
        const SOIL: StudProfile = StudProfile { cover: MAT_GRASS, cover_plates: 1, soil: MAT_DIRT, soil_plates: 5 };
        const CLIENT: game::ClientId = 7;
        /// The pond: stud columns 40..56 x 8..24 (chunk column (1, 0)), bed at plate
        /// 22, water to plate 28.
        const POND_BED: i32 = 22;
        const POND_TOP: i32 = 28;

        fn home() -> RegionId {
            RegionCoords::new(0, 0)
        }

        fn recipe(cx: usize, cz: usize) -> StudColumnGround {
            let n = STUD_CHUNK_STUDS;
            let pond = |i: usize| {
                let (x, z) = (cx * n + i % n, cz * n + i / n);
                (40..56).contains(&x) && (8..24).contains(&z)
            };
            StudColumnGround::new(
                (0..STUD_COLUMN_FOOTPRINT).map(|i| if pond(i) { POND_BED } else { TOP }).collect(),
                (0..STUD_COLUMN_FOOTPRINT).map(|i| if pond(i) { POND_TOP } else { STUD_NO_WATER }).collect(),
                vec![0; STUD_COLUMN_FOOTPRINT].into(),
                vec![SOIL],
                vec![0; STUD_COLUMN_FOOTPRINT].into(),
                vec![(i32::MAX, MAT_STONE)],
                BEDROCK,
            )
            .unwrap()
        }

        /// A 2 x 2 chunk-column stud region with a pond, then edited through the
        /// sim's own orders: a pit, a refilled tile, single bricks on the ground.
        fn region() -> Region {
            let mut r = Region::new(Rollback::new(None), None, home(), None);
            let ground = (0..2).flat_map(|x| (0..2).map(move |z| (ColumnCoords::new(x, z), recipe(x, z)))).collect();
            r.seed_stud_world(StudRegionSeed { ground, features: Vec::new() });
            let mut order = |o: Order| r.handle_event(GameEventKind::Order(CLIENT, o)).unwrap();
            // A pit two tiles wide, three deep, down from the surface.
            order(Order::CarveStud { lo: [8, 12, 8], hi: [12, 30, 12] });
            // Half a tile back in, of another material.
            order(Order::FillStud { lo: [8, 12, 8], hi: [10, 15, 10], material: MAT_DIRT });
            // A deep shaft into implied ground (materialises the layer below).
            order(Order::CarveStud { lo: [20, -8, 20], hi: [22, 30, 22] });
            // A wall two tiles tall and a single plate on the ground.
            order(Order::FillStud { lo: [20, 30, 4], hi: [22, 42, 6], material: MAT_STONE });
            order(Order::FillStud { lo: [26, 30, 4], hi: [27, 31, 5], material: MAT_STONE });
            r
        }

        /// The mirror as the bridge would leave it for `r`: the reset, the stud
        /// snapshot message, every materialised chunk.
        fn mirror_of(r: &Region) -> TileMirror {
            let mut m = TileMirror::default();
            m.set_home(Some(home()));
            m.apply(TileMirrorMsg::Reset { region: home(), designations: Vec::new(), constructed: Vec::new() });
            m.apply_stud(&StudWorldMsg::of_snapshot(home(), r.data()));
            let mut n = 0;
            for x in 0..2 {
                for z in 0..2 {
                    for y in -4..4 {
                        let c = StudChunkCoords { x, y, z };
                        if let Some(chunk) = r.data().stud_chunk_at(c) {
                            n += 1;
                            m.set_stud_chunk(Entity::from_raw_u32(n).unwrap(), stud_chunk_key(c), chunk);
                        }
                    }
                }
            }
            assert!(n >= 5, "the surface layer of each column and the shaft's lower layer: {n}");
            m
        }

        #[test]
        fn the_stud_mirror_agrees_with_the_sim_tile_for_tile() {
            let r = region();
            let m = mirror_of(&r);
            let sim = r.data().tiles();
            let mut seen = [0u32; 3];
            for x in -1..33 {
                for z in -1..33 {
                    assert_eq!(m.column_band([x, z]), sim.column_band([x, z]), "band of column [{x}, {z}]");
                    for y in -20..9 {
                        let p = [x, y, z];
                        let fill = sim.tile_fill(p);
                        assert_eq!(m.tile_fill(p), fill, "fill of {p:?}");
                        assert_eq!(m.tile_material(p), sim.tile_material(p), "material of {p:?}");
                        assert_eq!(m.is_bedrock(p), sim.is_bedrock(p), "bedrock at {p:?}");
                        assert_eq!(m.water_units(p), sim.water_units(p), "water in {p:?}");
                        assert_eq!(m.tree_id(p), sim.tree_id(p), "tree at {p:?}");
                        for kind in [DesignationKind::Dig, DesignationKind::Build(game::edit::PLACE_BLOCK), DesignationKind::Chop] {
                            assert_eq!(game::validate(&m, kind, p), game::validate(&sim, kind, p), "{kind:?} on {p:?}");
                        }
                        seen[match fill {
                            TileFill::Open => 0,
                            TileFill::Floor(_) => 1,
                            TileFill::Solid => 2,
                        }] += 1;
                    }
                }
            }
            assert!(seen.iter().all(|&n| n > 0), "open, floor and solid tiles were all compared: {seen:?}");
            // Spot checks, so the comparison is not two equal wrongs.
            assert_eq!(m.tile_fill([0, 4, 0]), TileFill::Solid, "the top tile of the dry ground");
            assert_eq!(m.tile_fill([0, 5, 0]), TileFill::Open);
            assert_eq!(m.tile_material([0, 4, 0]), Some(MAT_GRASS));
            assert_eq!(m.tile_fill([4, 3, 4]), TileFill::Open, "the pit");
            assert_eq!(m.tile_fill([4, 2, 4]), TileFill::Floor(3), "half refilled");
            assert_eq!(m.tile_material([4, 2, 4]), Some(MAT_DIRT));
            assert_eq!(m.tile_fill([13, 5, 2]), TileFill::Floor(1), "one plate on the ground");
            assert_eq!(m.tile_fill([10, -1, 10]), TileFill::Open, "the shaft, in the layer below");
            assert_eq!(m.tile_fill([10, -2, 10]), TileFill::Floor(4), "its bottom: carved down to plate -8");
            assert_eq!(m.tile_fill([11, -1, 10]), TileFill::Solid, "implied or materialised, rock beside the shaft");
            assert_eq!(m.tile_material([0, -3, 0]), Some(MAT_BEDROCK));
            assert!(m.is_bedrock([0, -3, 0]) && !m.is_bedrock([0, 0, 0]));
            // The pond: 6 plates of water over a bed at plate 22 (tile 3 holds 4 of
            // the bed, tile 4 is all water above it).
            assert!(m.water_units([22, 3, 6]) > 0 && m.water_units([22, 4, 6]) > 0);
            assert_eq!(m.water_units([0, 4, 0]), 0);
        }

        #[test]
        fn hand_built_chunks_implied_ground_and_the_halo_read_by_the_sims_rule() {
            let mut m = TileMirror::default();
            m.set_home(Some(home()));
            m.apply(TileMirrorMsg::Reset { region: home(), designations: Vec::new(), constructed: Vec::new() });
            let flat = StudColumnGround::flat(TOP, SOIL, MAT_STONE, BEDROCK);
            let west = WorldChunkPos::from_local(home(), [-1, 0, 0]);
            let halo_chunk = StudChunk::new([StudBrick::filler([30, 0, 0], [2, 3, 2], MAT_DIRT)]).unwrap();
            m.apply_stud(&StudWorldMsg::Region {
                region: home(),
                ground: std::sync::Arc::new([(ColumnCoords::new(0, 0), flat.clone())].into_iter().collect()),
                halo: vec![(west, Some(halo_chunk))],
                halo_ground: vec![(WorldChunkPos::new(west.x, 0, west.z), Some(flat.clone()))],
            });
            // No chunk mirrored yet: the recipe's implied ground.
            assert_eq!(m.tile_fill([0, 4, 0]), TileFill::Solid);
            assert_eq!(m.tile_fill([0, 5, 0]), TileFill::Open);
            assert_eq!(m.stud_tile([3, 2, 3]), StudTile::of_ground(&flat, 0, [3, 2, 3]));
            // A mirrored chunk is the truth, whatever the recipe says.
            let chunk = StudChunk::new([
                StudBrick::filler([0, 0, 0], [2, 6, 2], MAT_STONE),
                StudBrick::filler([2, 0, 0], [2, 1, 2], MAT_DIRT),
                StudBrick::filler([4, 0, 0], [1, 6, 1], MAT_STONE),
            ])
            .unwrap();
            let e = Entity::from_raw_u32(1).unwrap();
            let v = m.version();
            m.set_stud_chunk(e, [0, 0, 0], &chunk);
            assert!(m.version() > v);
            assert_eq!(m.tile_fill([0, 0, 0]), TileFill::Solid);
            assert_eq!(m.tile_fill([1, 0, 0]), TileFill::Floor(1));
            assert_eq!(m.tile_fill([2, 0, 0]), TileFill::Floor(6), "one column to the top: not solid");
            assert_eq!(m.tile_fill([0, 4, 0]), TileFill::Open, "the chunk has nothing there");
            for local in [[0, 0, 0], [1, 0, 0], [2, 0, 0], [5, 3, 5]] {
                let p = [local[0] as i32, local[1] as i32, local[2] as i32];
                assert_eq!(m.stud_tile(p), StudTile::of_chunk(&chunk, local));
            }
            // Layer 1 has no chunk: the recipe again (air above the surface).
            let l = STUD_LAYER_TILES as i32;
            assert_eq!(m.tile_fill([0, l, 0]), TileFill::Open);
            // Past the west edge: the halo chunk where there is one…
            assert_eq!(m.tile_fill([-1, 0, 0]), TileFill::Floor(3));
            assert_eq!(m.tile_material([-1, 0, 0]), Some(MAT_DIRT));
            assert_eq!(m.tile_fill([-1, 4, 0]), TileFill::Open, "the halo chunk is the truth for its layer");
            // …else the neighbour's recipe (the layer below has no halo chunk).
            assert_eq!(m.tile_fill([-1, -1, 0]), TileFill::Solid);
            // Beyond the halo band nothing is known.
            let far = -(STUD_CHUNK_TILES as i32) * (game::halo::HALO_CHUNKS + 1);
            assert_eq!(m.tile_fill([far, 0, 0]), TileFill::Open);
            // The entity going away drops its tiles; an undo of the halo drops its.
            m.remove_stud_entity(e);
            assert_eq!(m.tile_fill([0, 4, 0]), TileFill::Solid, "back to implied ground");
            m.apply_stud(&StudWorldMsg::Halo { region: home(), chunks: vec![(west, None)], ground: Vec::new() });
            assert_eq!(m.tile_fill([-1, 0, 0]), TileFill::Solid, "the neighbour's recipe");
            assert_eq!(STUD_CHUNK_PLATES % TILE_PLATES, 0);
        }

        #[test]
        fn picking_returns_stud_tiles() {
            let r = region();
            let m = mirror_of(&r);
            let (w, h) = (game::tile_width_units(), game::tile_height_units());
            let down = Vector::new(0.0, -1.0, 0.0);
            // Straight down onto the dry ground (its top tile is level 4; in voxel
            // blocks the same height would be level 5).
            let over = |tx: f32, tz: f32| Vector::new(tx * w, 400.0, tz * w);
            assert_eq!(pick_visible((over(3.5, 7.25), down), false, &m), Some([3, 4, 7]));
            assert_eq!(pick_visible((over(3.5, 7.25), down), true, &m), Some([3, 5, 7]), "a build goes on top");
            assert_eq!(plate_units(TOP), 5.0 * h);
            // Into the pit: its half-refilled floor tile, three levels down.
            assert_eq!(pick_visible((over(4.5, 4.5), down), false, &m), Some([4, 2, 4]));
            // Working level 5 (standing on the ground): the tile under the cursor at
            // that level, over ground, over the pit and on the wall.
            assert_eq!(pick_level((over(3.5, 7.25), down), 5, &m), Some([3, 5, 7]));
            assert_eq!(pick_level((over(4.5, 4.5), down), 5, &m), Some([4, 5, 4]), "open space clamps to the level");
            assert_eq!(pick_level((over(10.5, 2.5), down), 5, &m), Some([10, 5, 2]), "the wall's lower tile");
            assert_eq!(pick_level((over(4.5, 4.5), down), 2, &m), Some([4, 2, 4]), "the pit floor at its own level");
            // A slanted ray from the west at the wall (stud tiles [10, 5..7, 2]): the
            // tile it enters, and the air tile in front of that face.
            let origin = Vector::new(2.0 * w, 5.5 * h, 2.5 * w);
            let dir = Vector::new(1.0, 0.0, 0.0);
            assert_eq!(pick_visible((origin, dir), false, &m), Some([10, 5, 2]));
            assert_eq!(pick_visible((origin, dir), true, &m), Some([9, 5, 2]));
            let origin = Vector::new(2.0 * w, 6.5 * h, 2.5 * w);
            assert_eq!(pick_visible((origin, dir), false, &m), Some([10, 6, 2]), "1.2 m levels: the wall's upper tile");
            assert_eq!(pick_visible((Vector::new(2.0 * w, 7.5 * h, 2.5 * w), dir), false, &m), None, "over the wall");
        }

        #[test]
        fn tile_space_squashes_stud_levels() {
            let ray = (Vector::new(3.0, 96.0, 5.0), Vector::new(0.0, -1.0, 0.0));
            let (o, d) = tile_ray(ray);
            // 96 units is the top of stud level 4: 5 tiles up, i.e. 5 blocks in tile space.
            assert!((o.y - 5.0 * game::BLOCK_UNITS as f32).abs() < 1e-3, "{o:?}");
            assert_eq!((o.x, o.z), (3.0, 5.0));
            assert!((d.y + 1.0).abs() < 1e-6);
            let (_, d) = tile_ray((ray.0, Vector::new(1.0, -1.0, 0.0).normalize()));
            assert!((d.length() - 1.0).abs() < 1e-5 && d.x > -d.y, "flatter in tile space, still unit length");
        }
    }
}
