//! Surface material assignment: a pure function of depth-below-surface and
//! block Y. No noise in v1 (see design spec §5).

use crate::pipeline::SEA_LEVEL;
use game::BlockId;

#[derive(Clone, Copy)]
pub struct Materials {
    pub stone: BlockId,
    pub dirt: BlockId,
    pub grass: BlockId,
    pub sand: BlockId,
    pub water: BlockId,
}

impl Materials {
    /// Block placed at a solid voxel/block. `depth_below_surface == 0` is the
    /// topmost solid block of a column.
    pub fn material_at(&self, depth_below_surface: i32, block_y: i32) -> BlockId {
        let sea = SEA_LEVEL as i32;
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
    use game::BlockId;

    fn mats() -> Materials {
        Materials {
            stone: BlockId(2),
            dirt: BlockId(1),
            grass: BlockId(3),
            sand: BlockId(4),
            water: BlockId(5),
        }
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
        // Top block at/just above sea level -> sand.
        assert_eq!(m.material_at(0, 64), BlockId(4));
        assert_eq!(m.material_at(0, 65), BlockId(4));
    }

    #[test]
    fn manifest_defines_terrain_materials() {
        let manifest = include_str!("../../../assets/blocks/blocks.ron");
        let reg = game::BlockRegistry::from_ron(manifest).expect("valid manifest");
        for name in ["stone", "dirt", "grass", "sand", "water"] {
            assert!(reg.id_of(name).is_some(), "manifest must define {name}");
        }
    }
}
