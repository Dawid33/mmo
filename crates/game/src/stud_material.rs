//! Stud materials: what a brick is made of, as far as the sim cares.
//!
//! A material id below [`NATURAL_BASE`] is a **plastic** of that LDraw colour code (so
//! every baked colour is a material, and player builds pick a colour). Ids from
//! [`NATURAL_BASE`] up index [`STUD_NATURALS`], the terrain materials. Both are pure
//! tables: nothing to install, equal on every machine.

use crate::stud_catalog::STUD_COLORS;

/// First natural material id; `NATURAL_BASE + i` is `STUD_NATURALS[i]`.
pub const NATURAL_BASE: u16 = 0x8000;
// Every LDraw colour code must stay below the natural ids.
const _: () = {
    let mut i = 0;
    while i < STUD_COLORS.len() {
        assert!(STUD_COLORS[i] < NATURAL_BASE);
        i += 1;
    }
};

/// Work units to dig one stud cell of plastic.
pub const PLASTIC_HARDNESS: u16 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StudMaterialDef {
    pub name: &'static str,
    /// LDraw colour code the client draws it in (a member of `STUD_COLORS`).
    pub color: u16,
    /// Work units to dig one stud cell; `u16::MAX` cannot be dug.
    pub hardness: u16,
}

/// Terrain materials. APPEND-ONLY: ids are stored in sim state, orders and the world store.
pub const STUD_NATURALS: &[StudMaterialDef] = &[
    StudMaterialDef { name: "grass", color: 2, hardness: 2 },
    StudMaterialDef { name: "dirt", color: 28, hardness: 2 },
    StudMaterialDef { name: "stone", color: 72, hardness: 8 },
    StudMaterialDef { name: "sand", color: 19, hardness: 1 },
    StudMaterialDef { name: "gravel", color: 71, hardness: 3 },
    StudMaterialDef { name: "clay", color: 84, hardness: 3 },
    StudMaterialDef { name: "snow", color: 15, hardness: 1 },
    StudMaterialDef { name: "ice", color: 9, hardness: 4 },
    StudMaterialDef { name: "wood", color: 70, hardness: 5 },
    StudMaterialDef { name: "leaves", color: 288, hardness: 1 },
    StudMaterialDef { name: "bedrock", color: 0, hardness: u16::MAX },
    StudMaterialDef { name: "sandstone", color: 92, hardness: 6 },
    StudMaterialDef { name: "shale", color: 379, hardness: 6 },
    StudMaterialDef { name: "limestone", color: 151, hardness: 7 },
    StudMaterialDef { name: "conglomerate", color: 335, hardness: 7 },
    StudMaterialDef { name: "basalt", color: 8, hardness: 10 },
    StudMaterialDef { name: "granite", color: 7, hardness: 10 },
    StudMaterialDef { name: "glacier_ice", color: 212, hardness: 4 },
    StudMaterialDef { name: "mud", color: 308, hardness: 1 },
    StudMaterialDef { name: "peat", color: 6, hardness: 2 },
    StudMaterialDef { name: "red_sand", color: 484, hardness: 1 },
    StudMaterialDef { name: "dry_grass", color: 330, hardness: 2 },
    StudMaterialDef { name: "podzol", color: 86, hardness: 2 },
    StudMaterialDef { name: "frozen_dirt", color: 378, hardness: 4 },
    StudMaterialDef { name: "salt", color: 226, hardness: 3 },
];

pub const MAT_GRASS: u16 = NATURAL_BASE;
pub const MAT_DIRT: u16 = NATURAL_BASE + 1;
pub const MAT_STONE: u16 = NATURAL_BASE + 2;
pub const MAT_SAND: u16 = NATURAL_BASE + 3;
pub const MAT_GRAVEL: u16 = NATURAL_BASE + 4;
pub const MAT_CLAY: u16 = NATURAL_BASE + 5;
pub const MAT_SNOW: u16 = NATURAL_BASE + 6;
pub const MAT_ICE: u16 = NATURAL_BASE + 7;
pub const MAT_WOOD: u16 = NATURAL_BASE + 8;
pub const MAT_LEAVES: u16 = NATURAL_BASE + 9;
pub const MAT_BEDROCK: u16 = NATURAL_BASE + 10;

/// The material with this id: a natural one, or the plastic of a baked LDraw colour.
pub fn stud_material(id: u16) -> Option<StudMaterialDef> {
    if id >= NATURAL_BASE {
        return STUD_NATURALS.get((id - NATURAL_BASE) as usize).copied();
    }
    STUD_COLORS
        .binary_search(&id)
        .ok()
        .map(|_| StudMaterialDef { name: "plastic", color: id, hardness: PLASTIC_HARDNESS })
}

/// The LDraw colour code a material is drawn in (unknown ids draw as colour 0).
pub fn stud_material_color(id: u16) -> u16 {
    stud_material(id).map_or(0, |m| m.color)
}

/// The natural material called `name` (the voxel block names, so worldgen's material
/// tables resolve against either), if there is one.
pub fn stud_material_id(name: &str) -> Option<u16> {
    STUD_NATURALS.iter().position(|m| m.name == name).map(|i| NATURAL_BASE + i as u16)
}

/// The plastic a build order uses when its block has no stud material of the same
/// name: LDraw colour 71, light bluish grey.
pub const STUD_BUILD_FALLBACK_MATERIAL: u16 = 71;
const _: () = {
    let mut i = 0;
    let mut found = false;
    while i < STUD_COLORS.len() {
        found |= STUD_COLORS[i] == STUD_BUILD_FALLBACK_MATERIAL;
        i += 1;
    }
    assert!(found, "the fallback build material must be a baked colour");
};

/// The stud material a build of voxel block `block` uses, until items are ported:
/// the natural material with the block's registry name (`block_props().name_of`),
/// else [`STUD_BUILD_FALLBACK_MATERIAL`].
pub fn stud_material_of_block(block: crate::BlockId) -> u16 {
    let Some(name) = crate::block_props().name_of(block) else { return STUD_BUILD_FALLBACK_MATERIAL };
    // Every species' log and branch is the one stud wood, its leaves the one leaves.
    if name.ends_with("_log") || name.ends_with("_branch") {
        return MAT_WOOD;
    }
    if name.ends_with("_leaves") {
        return MAT_LEAVES;
    }
    stud_material_id(name).unwrap_or(STUD_BUILD_FALLBACK_MATERIAL)
}

/// The voxel block whose drop stud wood yields: stud trees are one wood whatever the
/// species, and there is no block called "wood".
pub const STUD_WOOD_DROP_BLOCK: &str = "oak_log";

/// The voxel block with the same registry name as natural material `material`, if the
/// installed block props have one (plastics have none): what its drops are read from.
pub fn block_of_stud_material(material: u16) -> Option<crate::BlockId> {
    if material < NATURAL_BASE {
        return None;
    }
    if material == MAT_WOOD {
        return crate::block_props().id_named(STUD_WOOD_DROP_BLOCK);
    }
    crate::block_props().id_named(stud_material(material)?.name)
}
