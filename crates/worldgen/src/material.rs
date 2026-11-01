//! Surface material assignment. The procedural world keeps `material_at`, a
//! pure function of depth-below-surface and block Y (design spec §5). Forged
//! worlds pick a `GroundCover` per biome (`ground_cover`, DF-scale terrain
//! spec §5b): surface block, soil block and soil depth.

use crate::pipeline::SEA_TOP_BLOCK;
use game::{Biome, BlockId};

#[derive(Clone, Copy)]
pub struct Materials {
    pub stone: BlockId,
    pub dirt: BlockId,
    pub grass: BlockId,
    pub sand: BlockId,
    pub water: BlockId,
    pub bedrock: BlockId,
    /// Indexed like `forge::geology::ROCKS`.
    pub rocks: [BlockId; 6],
    /// Tree blocks; `TreeBlocks::NONE` generates no trees.
    pub trees: TreeBlocks,
    /// Biome ground-cover, snow and ice blocks; `BiomeBlocks::NONE` keeps
    /// forged worlds on `material_at` (no biome covers, ice or glaciers).
    pub biome: BiomeBlocks,
}

/// The blocks biome ground cover, snow and ice are made of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BiomeBlocks {
    pub snow: BlockId,
    pub ice: BlockId,
    pub glacier_ice: BlockId,
    pub mud: BlockId,
    pub peat: BlockId,
    pub gravel: BlockId,
    pub red_sand: BlockId,
    pub dry_grass: BlockId,
    pub podzol: BlockId,
    pub frozen_dirt: BlockId,
    pub salt: BlockId,
}

impl BiomeBlocks {
    /// Biome covers off (every block air).
    pub const NONE: BiomeBlocks = BiomeBlocks {
        snow: BlockId::AIR,
        ice: BlockId::AIR,
        glacier_ice: BlockId::AIR,
        mud: BlockId::AIR,
        peat: BlockId::AIR,
        gravel: BlockId::AIR,
        red_sand: BlockId::AIR,
        dry_grass: BlockId::AIR,
        podzol: BlockId::AIR,
        frozen_dirt: BlockId::AIR,
        salt: BlockId::AIR,
    };

    pub fn enabled(&self) -> bool {
        *self != BiomeBlocks::NONE
    }

    /// Resolve every block by its manifest name; `NONE` if any resolves to air
    /// (a manifest without them).
    pub fn from_names(resolve: impl Fn(&str) -> BlockId) -> BiomeBlocks {
        let b = BiomeBlocks {
            snow: resolve("snow"),
            ice: resolve("ice"),
            glacier_ice: resolve("glacier_ice"),
            mud: resolve("mud"),
            peat: resolve("peat"),
            gravel: resolve("gravel"),
            red_sand: resolve("red_sand"),
            dry_grass: resolve("dry_grass"),
            podzol: resolve("podzol"),
            frozen_dirt: resolve("frozen_dirt"),
            salt: resolve("salt"),
        };
        let all = [
            b.snow, b.ice, b.glacier_ice, b.mud, b.peat, b.gravel, b.red_sand, b.dry_grass, b.podzol, b.frozen_dirt, b.salt,
        ];
        if all.contains(&BlockId::AIR) { BiomeBlocks::NONE } else { b }
    }
}

/// A column's ground cover: the surface block (depth 0), then `soil` for
/// depths `1..=soil_depth`, then the column's rock. `BlockId::AIR` in
/// `surface` or `soil` means bare rock: the rock the ground recipe has there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GroundCover {
    pub surface: BlockId,
    pub soil: BlockId,
    pub soil_depth: i32,
}

/// Soil depth (blocks) of an ordinary cover: `material_at`'s dirt / sand band.
pub const SOIL_DEPTH_BLOCKS: i32 = 3;
/// Sand depth (blocks) of a sand desert.
pub const DESERT_SAND_DEPTH_BLOCKS: i32 = 8;
/// Soil depth (blocks) on mountains below the snow line (altitude thinning).
pub const MOUNTAIN_SOIL_DEPTH_BLOCKS: i32 = 1;
/// Gravel over the rock of a rocky wasteland (blocks).
pub const WASTELAND_GRAVEL_DEPTH_BLOCKS: i32 = 1;
/// Tundra: frozen dirt over this much gravel (blocks).
pub const TUNDRA_GRAVEL_DEPTH_BLOCKS: i32 = 2;
/// Peat under a marsh's mud (blocks); swamps hold more.
pub const MARSH_PEAT_DEPTH_BLOCKS: i32 = 3;
pub const SWAMP_PEAT_DEPTH_BLOCKS: i32 = 5;
/// Salt crust over sand around closed salt lakes (blocks of sand).
pub const SALT_FLAT_SAND_DEPTH_BLOCKS: i32 = 2;

impl GroundCover {
    /// No soil: the rock comes straight to the surface.
    pub const BARE_ROCK: GroundCover = GroundCover { surface: BlockId::AIR, soil: BlockId::AIR, soil_depth: 0 };

    const fn of(surface: BlockId, soil: BlockId, soil_depth: i32) -> GroundCover {
        GroundCover { surface, soil, soil_depth }
    }
}

/// One species' blocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpeciesBlocks {
    pub log: BlockId,
    pub branch: BlockId,
    pub leaves: BlockId,
}

/// Tree blocks per species, indexed like `trees::SPECIES`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TreeBlocks {
    pub species: [SpeciesBlocks; 3],
}

impl TreeBlocks {
    /// Trees disabled (every block air).
    pub const NONE: TreeBlocks = TreeBlocks {
        species: [SpeciesBlocks { log: BlockId::AIR, branch: BlockId::AIR, leaves: BlockId::AIR }; 3],
    };

    pub fn enabled(&self) -> bool {
        *self != TreeBlocks::NONE
    }

    /// Resolve `<species>_log` / `_branch` / `_leaves` for every species.
    pub fn from_names(resolve: impl Fn(&str) -> BlockId) -> TreeBlocks {
        TreeBlocks {
            species: crate::trees::SPECIES.map(|s| SpeciesBlocks {
                log: resolve(&format!("{}_log", s.name)),
                branch: resolve(&format!("{}_branch", s.name)),
                leaves: resolve(&format!("{}_leaves", s.name)),
            }),
        }
    }
}

impl Materials {
    /// Resolve every material by block name (the registry lookup of the
    /// server / local server / harnesses).
    pub fn from_names(resolve: impl Fn(&str) -> BlockId) -> Materials {
        Materials {
            stone: resolve("stone"),
            dirt: resolve("dirt"),
            grass: resolve("grass"),
            sand: resolve("sand"),
            water: resolve("water"),
            bedrock: resolve("bedrock"),
            rocks: crate::forge::geology::ROCKS.map(|r| resolve(r.name)),
            trees: TreeBlocks::from_names(&resolve),
            biome: BiomeBlocks::from_names(&resolve),
        }
    }

    /// `material_at`'s cover at surface block `block_y`: grass over dirt, or
    /// sand on beaches.
    fn temperate_cover(&self, block_y: i32) -> GroundCover {
        GroundCover::of(self.material_at(0, block_y), self.material_at(1, block_y), SOIL_DEPTH_BLOCKS)
    }

    /// The ground cover of a column of `biome` whose surface block is
    /// `block_y`; `above_snow_line` puts snow on top (straight on rock on
    /// mountains, on the biome's own soil elsewhere). Beaches (the band
    /// `material_at` makes sand) stay sand except in biomes with their own
    /// shore (desert, wetlands, tundra, wastelands); a mountain at the shore gets the
    /// beach too. Without biome blocks every
    /// biome is `material_at`'s cover.
    pub fn ground_cover(&self, biome: Biome, above_snow_line: bool, block_y: i32) -> GroundCover {
        let b = &self.biome;
        if !b.enabled() {
            return self.temperate_cover(block_y);
        }
        let beach = block_y <= SEA_TOP_BLOCK + 1;
        let beach_cover = || GroundCover::of(self.sand, self.sand, SOIL_DEPTH_BLOCKS);
        let base = match biome {
            Biome::Ocean
            | Biome::FrozenOcean
            | Biome::Lake
            | Biome::Grassland
            | Biome::Shrubland
            | Biome::ConiferForest
            | Biome::BroadleafForest
            | Biome::TropicalConiferForest
            | Biome::TropicalBroadleafForest => self.temperate_cover(block_y),
            Biome::Savanna if beach => beach_cover(),
            Biome::Savanna => GroundCover::of(b.dry_grass, self.dirt, SOIL_DEPTH_BLOCKS),
            Biome::Taiga if beach => beach_cover(),
            Biome::Taiga => GroundCover::of(b.podzol, self.dirt, SOIL_DEPTH_BLOCKS),
            Biome::Tundra => GroundCover::of(b.frozen_dirt, b.gravel, TUNDRA_GRAVEL_DEPTH_BLOCKS),
            Biome::Glacier => GroundCover::BARE_ROCK,
            Biome::SandDesert => GroundCover::of(self.sand, self.sand, DESERT_SAND_DEPTH_BLOCKS),
            Biome::RockyWasteland => GroundCover::of(b.gravel, b.gravel, WASTELAND_GRAVEL_DEPTH_BLOCKS),
            Biome::Badlands => GroundCover::of(b.red_sand, b.red_sand, SOIL_DEPTH_BLOCKS),
            Biome::Marsh => GroundCover::of(b.mud, b.peat, MARSH_PEAT_DEPTH_BLOCKS),
            Biome::Swamp => GroundCover::of(b.mud, b.peat, SWAMP_PEAT_DEPTH_BLOCKS),
            Biome::MangroveSwamp => GroundCover::of(b.mud, b.mud, SWAMP_PEAT_DEPTH_BLOCKS),
            Biome::Mountain if beach => beach_cover(),
            Biome::Mountain => GroundCover::of(self.grass, self.dirt, MOUNTAIN_SOIL_DEPTH_BLOCKS),
        };
        if !above_snow_line {
            return base;
        }
        match biome {
            Biome::Mountain | Biome::Glacier => GroundCover::of(b.snow, BlockId::AIR, 0),
            _ if base.soil_depth == 0 => GroundCover::of(b.snow, BlockId::AIR, 0),
            // Snow over the biome's soil (its surface block becomes snow).
            _ => GroundCover::of(b.snow, base.soil, base.soil_depth),
        }
    }

    /// A salt flat around a closed salt lake: a salt crust over sand.
    pub fn salt_flat(&self) -> GroundCover {
        GroundCover::of(self.biome.salt, self.sand, SALT_FLAT_SAND_DEPTH_BLOCKS)
    }

    /// Block placed at a solid voxel/block. `depth_below_surface == 0` is the
    /// topmost solid block of a column.
    pub fn material_at(&self, depth_below_surface: i32, block_y: i32) -> BlockId {
        let sea = SEA_TOP_BLOCK;
        if depth_below_surface == 0 {
            // Beach band around sea level, else grass.
            if block_y <= sea + 1 {
                self.sand
            } else {
                self.grass
            }
        } else if depth_below_surface <= 3 {
            // Sand keeps going a few blocks down on beaches; dirt inland.
            if block_y <= sea + 1 {
                self.sand
            } else {
                self.dirt
            }
        } else {
            self.stone
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{Biome, BlockId};

    fn mats() -> Materials {
        Materials {
            stone: BlockId(2),
            dirt: BlockId(1),
            grass: BlockId(3),
            sand: BlockId(4),
            water: BlockId(5),
            bedrock: game::BlockId(6),
            rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
            trees: crate::material::TreeBlocks::NONE,
            biome: BiomeBlocks::NONE,
        }
    }

    /// `mats()` with the biome blocks on (ids as in the manifest).
    fn biome_mats() -> Materials {
        let manifest = include_str!("../../../assets/blocks/blocks.ron");
        let reg = game::BlockRegistry::from_ron(manifest).expect("valid manifest");
        Materials { biome: BiomeBlocks::from_names(|n| reg.id_of(n).unwrap_or(BlockId::AIR)), ..mats() }
    }

    #[test]
    fn ground_cover_table() {
        let m = biome_mats();
        let b = m.biome;
        assert!(b.enabled());
        let inland = SEA_TOP_BLOCK + 40;
        // Desert: sand over sand, deep.
        let desert = m.ground_cover(Biome::SandDesert, false, inland);
        assert_eq!((desert.surface, desert.soil), (m.sand, m.sand));
        assert!(desert.soil_depth >= 6, "{desert:?}");
        // Swamp: mud over peat.
        let swamp = m.ground_cover(Biome::Swamp, false, inland);
        assert_eq!((swamp.surface, swamp.soil), (b.mud, b.peat));
        // Tundra: frozen dirt.
        assert_eq!(m.ground_cover(Biome::Tundra, false, inland).surface, b.frozen_dirt);
        // Alpine above the snow line: snow straight over rock.
        let alpine = m.ground_cover(Biome::Mountain, true, SEA_TOP_BLOCK + 2500);
        assert_eq!((alpine.surface, alpine.soil_depth), (b.snow, 0));
        // Other covers of the table.
        assert_eq!(m.ground_cover(Biome::Savanna, false, inland).surface, b.dry_grass);
        assert_eq!(m.ground_cover(Biome::Taiga, false, inland).surface, b.podzol);
        assert_eq!(m.ground_cover(Biome::Badlands, false, inland).surface, b.red_sand);
        assert_eq!(m.ground_cover(Biome::RockyWasteland, false, inland).surface, b.gravel);
        assert_eq!(m.ground_cover(Biome::Glacier, false, inland), GroundCover::BARE_ROCK);
        assert_eq!(m.salt_flat().surface, b.salt);
        // A mountain reaching the shore gets the beach, not grass.
        for y in [SEA_TOP_BLOCK - 3, SEA_TOP_BLOCK + 1] {
            assert_eq!(m.ground_cover(Biome::Mountain, false, y).surface, m.sand, "mountain shore at y {y}");
        }
        // Temperate: grass over dirt, exactly `material_at`'s answer, beaches included.
        for biome in [Biome::Grassland, Biome::BroadleafForest, Biome::ConiferForest] {
            for y in [SEA_TOP_BLOCK - 3, SEA_TOP_BLOCK, SEA_TOP_BLOCK + 1, SEA_TOP_BLOCK + 2, inland] {
                let c = m.ground_cover(biome, false, y);
                assert_eq!((c.surface, c.soil, c.soil_depth), (m.material_at(0, y), m.material_at(1, y), 3), "{biome:?} y {y}");
            }
        }
        // Without biome blocks every biome falls back to `material_at`.
        let plain = mats();
        let c = plain.ground_cover(Biome::SandDesert, true, inland);
        assert_eq!((c.surface, c.soil, c.soil_depth), (plain.material_at(0, inland), plain.material_at(1, inland), 3));
    }

    #[test]
    fn top_block_is_grass_inland() {
        let m = mats();
        assert_eq!(m.material_at(0, 100), BlockId(3)); // grass on top, well above sea
        assert_eq!(m.material_at(1, 100), BlockId(1)); // dirt just below
        assert_eq!(m.material_at(4, 100), BlockId(2)); // stone deeper
    }

    #[test]
    fn beaches_are_sand_near_sea_level() {
        let m = mats();
        // Top block level with the water's top block, or one above -> sand;
        // the band rides the water surface, not an absolute height.
        assert_eq!(m.material_at(0, SEA_TOP_BLOCK), BlockId(4));
        assert_eq!(m.material_at(0, SEA_TOP_BLOCK + 1), BlockId(4));
        assert_eq!(m.material_at(0, SEA_TOP_BLOCK + 2), BlockId(3));
    }

    #[test]
    fn manifest_defines_terrain_materials() {
        let manifest = include_str!("../../../assets/blocks/blocks.ron");
        let reg = game::BlockRegistry::from_ron(manifest).expect("valid manifest");
        for name in ["stone", "dirt", "grass", "sand", "water"] {
            assert!(reg.id_of(name).is_some(), "manifest must define {name}");
        }
    }

    #[test]
    fn manifest_defines_every_material() {
        let manifest = include_str!("../../../assets/blocks/blocks.ron");
        let reg = game::BlockRegistry::from_ron(manifest).expect("valid manifest");
        let m = Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing {n}")));
        assert_eq!(m.bedrock, BlockId(6));
        assert_eq!(m.rocks, [7, 8, 9, 10, 11, 12].map(BlockId));
        assert!(m.trees.enabled());
        assert_eq!(m.trees.species[0], SpeciesBlocks { log: BlockId(13), branch: BlockId(14), leaves: BlockId(15) });
        assert_eq!(m.trees.species[2].leaves, BlockId(21));
        assert!(m.biome.enabled());
        assert_eq!((m.biome.snow, m.biome.salt), (BlockId(22), BlockId(32)));
    }
}
