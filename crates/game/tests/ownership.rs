//! Super-region membership (`game::ownership`).

use game::{super_region_of, RegionCoords, DEFAULT_SUPER_REGION_REGIONS};

#[test]
fn super_region_floor_div() {
    let s = DEFAULT_SUPER_REGION_REGIONS;
    assert_eq!(s, 16);
    assert_eq!(super_region_of(RegionCoords::new(-1, -1), s), (-1, -1));
    assert_eq!(super_region_of(RegionCoords::new(-16, 0), s), (-1, 0));
    assert_eq!(super_region_of(RegionCoords::new(-17, 0), s), (-2, 0));
    assert_eq!(super_region_of(RegionCoords::new(0, 15), s), (0, 0));
    assert_eq!(super_region_of(RegionCoords::new(16, -16), s), (1, -1));
}
