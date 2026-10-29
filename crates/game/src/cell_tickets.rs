//! Server-side cell tickets (spec §1): which cells should be active, given
//! player positions. Pure and deterministic; its output becomes one
//! `CellChanges` event per tick. Not sim state.

use std::collections::{BTreeMap, BTreeSet};

use crate::{cell_of_pos, cells_within, chebyshev, CellCoords};

/// Cells within this Chebyshev radius of a player are wanted active.
pub const SIM_RADIUS_CELLS: u32 = 3;
/// Extra radius a cell may drift out to before its release countdown starts.
pub const CELL_HYSTERESIS: u32 = 1;
/// Ticks a cell must stay beyond radius + hysteresis before it deactivates.
pub const CELL_RELEASE_GRACE_TICKS: u32 = 40;
/// Wanted (non-required) activations per tick.
pub const MAX_CELL_ACTIVATIONS_PER_TICK: usize = 1;
/// Rings past `SIM_RADIUS_CELLS` whose worldgen runs ahead in the background
/// (server), so an activation rarely waits for generation.
pub const PREFETCH_CELLS: u32 = 1;

/// Every cell within `sim_radius + PREFETCH_CELLS` of a player, sorted —
/// what the server's store should have generated before it is wanted.
pub fn prefetch_cells(players: &[(f32, f32)], cfg: &CellConfig) -> Vec<CellCoords> {
    let mut out = BTreeSet::new();
    for p in players {
        if let Some(c) = cell_of_pos(p.0, p.1, cfg.cell_chunks) {
            out.extend(cells_within(c, cfg.sim_radius + PREFETCH_CELLS, cfg.cell_chunks));
        }
    }
    out.into_iter().collect()
}

#[derive(Debug, Clone)]
pub struct CellConfig {
    pub cell_chunks: usize,
    pub sim_radius: u32,
    pub hysteresis: u32,
    pub release_grace_ticks: u32,
    pub max_activations_per_tick: usize,
}

impl CellConfig {
    pub fn new(cell_chunks: usize) -> Self {
        crate::cells_per_side(cell_chunks); // validates the size
        Self {
            cell_chunks,
            sim_radius: SIM_RADIUS_CELLS,
            hysteresis: CELL_HYSTERESIS,
            release_grace_ticks: CELL_RELEASE_GRACE_TICKS,
            max_activations_per_tick: MAX_CELL_ACTIVATIONS_PER_TICK,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TicketPlan {
    /// Sorted.
    pub deactivate: Vec<CellCoords>,
    /// Sorted.
    pub activate: Vec<CellCoords>,
}

/// A player's own cell and its 8 neighbours (clipped): always active.
pub fn required_cells(pos: (f32, f32), cell_chunks: usize) -> Vec<CellCoords> {
    match cell_of_pos(pos.0, pos.1, cell_chunks) {
        Some(c) => cells_within(c, 1, cell_chunks),
        None => Vec::new(),
    }
}

#[derive(Debug, Default)]
pub struct CellTickets {
    /// Consecutive plans each active cell has spent outside keep range.
    out_of_range: BTreeMap<CellCoords, u32>,
}

impl CellTickets {
    pub fn plan(
        &mut self,
        active: &BTreeSet<CellCoords>,
        players: &[(f32, f32)],
        cfg: &CellConfig,
    ) -> TicketPlan {
        let player_cells: Vec<CellCoords> = players
            .iter()
            .filter_map(|p| cell_of_pos(p.0, p.1, cfg.cell_chunks))
            .collect();
        let dist = |c: CellCoords| player_cells.iter().map(|p| chebyshev(*p, c)).min();

        let mut required = BTreeSet::new();
        let mut wanted = BTreeSet::new();
        for p in &player_cells {
            required.extend(cells_within(*p, 1, cfg.cell_chunks));
            wanted.extend(cells_within(*p, cfg.sim_radius, cfg.cell_chunks));
        }

        let mut activate: BTreeSet<CellCoords> =
            required.iter().filter(|c| !active.contains(c)).copied().collect();
        let mut candidates: Vec<CellCoords> = wanted
            .iter()
            .filter(|c| !active.contains(c) && !required.contains(c))
            .copied()
            .collect();
        candidates.sort_by_key(|c| (dist(*c).unwrap_or(u32::MAX), *c));
        activate.extend(candidates.into_iter().take(cfg.max_activations_per_tick));

        let keep = cfg.sim_radius + cfg.hysteresis;
        let mut deactivate = Vec::new();
        for c in active {
            if dist(*c).is_some_and(|d| d <= keep) {
                self.out_of_range.remove(c);
                continue;
            }
            let n = self.out_of_range.entry(*c).or_insert(0);
            *n += 1;
            if *n >= cfg.release_grace_ticks {
                self.out_of_range.remove(c);
                deactivate.push(*c);
            }
        }
        self.out_of_range.retain(|c, _| active.contains(c));

        TicketPlan { deactivate, activate: activate.into_iter().collect() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cells 16 blocks (256 units) wide, in chunks: 8 at 2-block chunks, 1 at 16.
    const CELL: usize = 16 / crate::CHUNK_BLOCKS;

    fn cfg() -> CellConfig {
        CellConfig::new(CELL) // 4×4 grid of 256-unit cells
    }
    fn centre(x: u32, z: u32) -> (f32, f32) {
        let s = crate::cell_size_units(CELL);
        ((x as f32 + 0.5) * s, (z as f32 + 0.5) * s)
    }

    #[test]
    fn required_cells_bypass_the_budget() {
        let mut t = CellTickets::default();
        let plan = t.plan(&BTreeSet::new(), &[centre(1, 1)], &cfg());
        // 3×3 around (1,1) = 9 required, plus the budgeted wanted cells.
        assert_eq!(plan.activate.len(), 9 + MAX_CELL_ACTIVATIONS_PER_TICK);
        for c in required_cells(centre(1, 1), CELL) {
            assert!(plan.activate.contains(&c));
        }
    }

    #[test]
    fn wanted_cells_come_nearest_first_one_per_tick() {
        let mut t = CellTickets::default();
        let mut active: BTreeSet<CellCoords> = required_cells(centre(0, 0), CELL).into_iter().collect();
        let plan = t.plan(&active, &[centre(0, 0)], &cfg());
        assert_eq!(plan.activate.len(), 1);
        // Distance-2 ring, smallest coords first among equals.
        assert_eq!(plan.activate[0], CellCoords::new(0, 2));
        active.extend(plan.activate);
        let plan2 = t.plan(&active, &[centre(0, 0)], &cfg());
        assert_eq!(plan2.activate, vec![CellCoords::new(1, 2)]);
    }

    #[test]
    fn release_waits_for_the_grace_period() {
        let mut t = CellTickets::default();
        let active: BTreeSet<CellCoords> = [CellCoords::new(3, 3)].into_iter().collect();
        // (3,3) is Chebyshev 3 from (0,0) = radius + hysteresis: kept.
        let plan = t.plan(&active, &[centre(0, 0)], &cfg());
        assert!(plan.deactivate.is_empty(), "within radius + hysteresis is kept");
        let mut released = None;
        for tick in 0..CELL_RELEASE_GRACE_TICKS + 1 {
            let p = t.plan(&active, &[], &cfg());
            if !p.deactivate.is_empty() {
                released = Some(tick);
                break;
            }
        }
        assert_eq!(released, Some(CELL_RELEASE_GRACE_TICKS - 1));
    }

    #[test]
    fn coming_back_resets_the_grace_countdown() {
        let mut t = CellTickets::default();
        let active: BTreeSet<CellCoords> = [CellCoords::new(3, 3)].into_iter().collect();
        for _ in 0..CELL_RELEASE_GRACE_TICKS - 2 {
            assert!(t.plan(&active, &[], &cfg()).deactivate.is_empty());
        }
        t.plan(&active, &[centre(3, 3)], &cfg());
        for _ in 0..CELL_RELEASE_GRACE_TICKS - 2 {
            assert!(t.plan(&active, &[], &cfg()).deactivate.is_empty());
        }
    }

    #[test]
    fn prefetch_reaches_one_ring_past_the_wanted_radius() {
        let cells = prefetch_cells(&[centre(0, 0)], &cfg());
        // 4×4 grid: radius 2 + PREFETCH_CELLS (1) = 3 covers every cell from (0,0).
        assert_eq!(cells.len(), 16);
        let near = prefetch_cells(&[centre(1, 1)], &CellConfig { sim_radius: 0, ..cfg() });
        assert_eq!(near.len(), 9, "radius 0 + 1 = the 3×3");
        assert!(prefetch_cells(&[], &cfg()).is_empty());
    }

    #[test]
    fn plans_are_sorted_and_never_repeat_active_cells() {
        let mut t = CellTickets::default();
        let active: BTreeSet<CellCoords> = [CellCoords::new(1, 1)].into_iter().collect();
        let plan = t.plan(&active, &[centre(1, 1), centre(2, 2)], &cfg());
        assert!(!plan.activate.contains(&CellCoords::new(1, 1)));
        let mut sorted = plan.activate.clone();
        sorted.sort();
        assert_eq!(plan.activate, sorted, "activate is sorted by CellCoords");
    }
}
