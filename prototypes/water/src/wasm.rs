//! wasm32 entry: run one scenario and return its final state hash, for comparison
//! with the native `water_proto --golden` output.

use crate::{Mode, scenario};

#[unsafe(no_mangle)]
pub extern "C" fn golden(scenario_index: u32, mode: u32) -> u64 {
    let mode = if mode == 0 { Mode::CellOnly } else { Mode::Bodies };
    let mut sc = scenario::build(scenario::NAMES[scenario_index as usize], mode);
    scenario::run(&mut sc, |_| {}).final_hash
}

/// Run seam scenario `index` to rest and return the pair's final hash.
#[unsafe(no_mangle)]
pub extern "C" fn golden_seam(index: u32) -> u64 {
    let mut p = crate::seam::build(crate::seam::NAMES[index as usize]);
    p.run(crate::seam::SEAM_MAX_STEPS);
    p.hash()
}
