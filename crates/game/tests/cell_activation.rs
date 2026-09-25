//! Cell loading inside one region — spec
//! docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md.
use game::{state_hash, Region, RegionCoords};

pub fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

/// 8-chunk cells: a 4×4 grid in the default 32-chunk region.
pub const CC: usize = 8;

#[test]
fn legacy_regions_are_not_in_cell_mode() {
    let r = Region::from_chunks(r0(), Vec::new());
    assert!(!r.data().cells.enabled());
    assert_eq!(r.data().cells.cell_chunks, 0);
}

#[test]
fn a_cell_mode_region_starts_empty() {
    let r = Region::new_cells(r0(), CC, None, None);
    assert!(r.data().cells.enabled());
    assert!(r.data().cells.active.is_empty());
    assert_eq!(r.data().ecs.chunk.iter().filter(|(_, c)| c.is_some()).count(), 0);
}

#[test]
fn cell_state_is_part_of_the_state_hash() {
    let a = Region::new_cells(r0(), CC, None, None);
    let b = Region::from_chunks(r0(), Vec::new());
    assert_ne!(state_hash(a.data()), state_hash(b.data()));
}
