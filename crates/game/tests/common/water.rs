//! Recipe water for test fixtures (water plan Task 5): water is the ground
//! recipe's `water_top` over AIR cells (`ColumnGround::recipe_water`), not
//! liquid blocks. [`floor_with_water`] builds it into a region's genesis
//! columns (what the seam harness's ground hook uses); [`set_recipe_water`]
//! writes it into an already built region (the hand-built fixtures).
