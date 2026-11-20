//! Deterministic, rollback-safe tree regrowth.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Block, BlockId, RegionTrees, Rollback, TreeLife, TreeLifeSeed, TreeState, WorldBlockPos,
};

/// Six inclusive growth shapes, from sapling to the mature target.
pub const TREE_GROW_AGES: [u16; 6] = [0, 20, 40, 60, 80, 100];
pub const TREE_GROW_STAGE_DAYS: u64 = 4;
pub const TREE_GROW_STAGES_PER_TICK: usize = 1;

pub fn tree_stage_ticks() -> u64 {
    TREE_GROW_STAGE_DAYS * crate::history::TICKS_PER_DAY
}

impl Rollback {
    /// Raw genesis/restore insertion. The root and due indexes are rebuilt and
    /// never trusted from the source.
    pub fn seed_trees_genesis(&mut self, trees: impl IntoIterator<Item = TreeLifeSeed>) {
        let mut lives = RegionTrees::new();
        for seed in trees {
            let mut life = crate::standing_life(seed);
            life.original_roots.sort_unstable();
            life.original_roots.dedup();
            life.surviving_roots.sort_unstable();
            life.surviving_roots.dedup();
            lives.insert(life.genome.id, life);
        }
        self.restore_tree_indexes(lives);
    }

    /// Raw restore insertion, with the nullable legacy store represented by an
    /// empty map.
    pub(crate) fn seed_trees_restore(&mut self, trees: RegionTrees) {
        self.restore_tree_indexes(trees);
    }

    fn restore_tree_indexes(&mut self, trees: RegionTrees) {
        for (id, life) in trees {
            for &root in &life.surviving_roots {
                self.data.tree_roots.insert_raw(root, id);
            }
            if let TreeState::Regrowing { next_tick, .. } = life.state {
                self.data.tree_due.insert_raw((next_tick, id), ());
            }
            self.data.tree_lives.insert_raw(id, life);
        }
    }

    pub fn is_tree_root(&self, pos: [i32; 3]) -> bool {
        self.data.tree_roots.contains_key(&pos)
    }

    fn growth_interval(&self, life: &TreeLife) -> u64 {
        let remaining = life.surviving_roots.len().max(1) as u64;
        let original = life.original_roots.len().max(1) as u64;
        let slowdown = original.div_ceil(remaining).clamp(1, 3);
        tree_stage_ticks().saturating_mul(slowdown)
    }

    fn remove_due_safe(&mut self, id: u32, state: &TreeState) {
        if let TreeState::Regrowing { next_tick, .. } = state {
            self.data.tree_due.remove(&(*next_tick, id));
        }
    }

    fn write_life_safe(&mut self, id: u32, old: &TreeState, life: TreeLife) {
        self.remove_due_safe(id, old);
        if let TreeState::Regrowing { next_tick, .. } = life.state {
            self.data.tree_due.insert((next_tick, id), ());
        }
        self.data.tree_lives.insert(id, life);
    }

    /// A visible felling starts at the sapling, while branch-only loss asks for
    /// repair of the mature/current target.
    pub(crate) fn tree_felled_safe(&mut self, id: u32, trunk_cut: bool) {
        let Some(life) = self.data.tree_lives.get(&id).cloned() else {
            return;
        };
        if life.surviving_roots.is_empty() {
            return;
        }
        let old = life.state.clone();
        let stage = if trunk_cut {
            0
        } else {
            (TREE_GROW_AGES.len() - 1) as u8
        };
        let next_tick = (*self.data.tick).saturating_add(self.growth_interval(&life));
        let mut life = life;
        life.state = TreeState::Regrowing {
            next_stage: stage,
            next_tick,
            repair_age: (!trunk_cut).then_some(life.genome.initial_age),
        };
        self.write_life_safe(id, &old, life);
    }

    /// Observe a normal terrain edit. Growth writes bypass this path.
    pub(crate) fn tree_block_changed_safe(&mut self, pos: [i32; 3], old: Block, new: Block) {
        if old == new || old.tree_id() == 0 {
            return;
        }
        let id = old.tree_id();
        if self.data.tree_roots.get(&pos) == Some(&id) {
            self.data.tree_roots.remove(&pos);
            let Some(mut life) = self.data.tree_lives.get(&id).cloned() else {
                return;
            };
            let old_state = life.state.clone();
            life.surviving_roots.retain(|&root| root != pos);
            if life.surviving_roots.is_empty() {
                self.remove_due_safe(id, &old_state);
                self.data.tree_lives.remove(&id);
                return;
            }
            if let TreeState::Regrowing {
                next_stage,
                repair_age,
                ..
            } = life.state
            {
                life.state = TreeState::Regrowing {
                    next_stage,
                    next_tick: (*self.data.tick).saturating_add(self.growth_interval(&life)),
                    repair_age,
                };
                self.write_life_safe(id, &old_state, life);
            } else {
                self.data.tree_lives.insert(id, life);
            }
            return;
        }
        let Some(life) = self.data.tree_lives.get(&id).cloned() else {
            return;
        };
        if matches!(life.state, TreeState::Standing) && !life.surviving_roots.is_empty() {
            let old_state = life.state.clone();
            let mut life = life;
            life.state = TreeState::Regrowing {
                next_stage: (TREE_GROW_AGES.len() - 1) as u8,
                next_tick: (*self.data.tick).saturating_add(self.growth_interval(&life)),
                repair_age: Some(life.genome.initial_age),
            };
            self.write_life_safe(id, &old_state, life);
        }
    }

    fn growth_block_allowed(&self, pos: [i32; 3], id: u32) -> bool {
        if crate::edit::locate(pos).is_none() {
            return false;
        }
        let block = self.block_at(pos).unwrap_or(Block::new(BlockId::AIR));
        if block.id != BlockId::AIR && block.tree_id() != id {
            return false;
        }
        if let Some((coords, (bx, by, bz))) = crate::edit::locate(pos) {
            if self
                .chunk_key_at(coords)
                .and_then(|e| self.data.ecs.chunk.try_get(e).as_ref())
                .is_some_and(|chunk| {
                    chunk
                        .chisel
                        .contains_key(&crate::BlockIndex::from_xyz(bx, by, bz))
                })
            {
                return false;
            }
        }
        if self.block_overlaps_body(pos) {
            return false;
        }
        let world = WorldBlockPos::from_local(
            *self.data.region,
            crate::BlockPos::new(pos[0], pos[1], pos[2]),
        );
        self.water_units_at(world) == 0
            && !self.data.item_piles.contains_key(&world)
            && !self.data.containers.contains_key(&world)
    }

    /// Build the one writable, face-connected target component rooted at the
    /// surviving trunk base. Obstructions are omitted, never overwritten.
    fn tree_stage_writes(
        &self,
        life: &TreeLife,
        stage: u8,
        repair_age: Option<u16>,
    ) -> (Vec<([i32; 3], Block)>, bool) {
        let Some(&species) = crate::SPECIES.get(life.genome.species as usize) else {
            return (Vec::new(), true);
        };
        let age = repair_age.unwrap_or(TREE_GROW_AGES[stage as usize]);
        let raster = crate::rasterize(&crate::skeleton(&species, life.genome.seed, age));
        let target: BTreeMap<[i32; 3], Block> = raster
            .into_iter()
            .filter(|b| b.offset[1] >= 0)
            .map(|b| {
                let id = match b.part {
                    crate::TreePart::Log => life.genome.log,
                    crate::TreePart::Branch => life.genome.branch,
                    crate::TreePart::Leaves => life.genome.leaves,
                };
                (
                    [
                        life.genome.base[0] + b.offset[0],
                        life.genome.base[1] + b.offset[1],
                        life.genome.base[2] + b.offset[2],
                    ],
                    Block::with_tree(id, life.genome.id, b.limb),
                )
            })
            .collect();
        let mut reachable = BTreeSet::new();
        let mut stack = vec![life.genome.base];
        while let Some(pos) = stack.pop() {
            if !target.contains_key(&pos)
                || !self.growth_block_allowed(pos, life.genome.id)
                || !reachable.insert(pos)
            {
                continue;
            }
            for d in [
                [1, 0, 0],
                [-1, 0, 0],
                [0, 1, 0],
                [0, -1, 0],
                [0, 0, 1],
                [0, 0, -1],
            ] {
                stack.push([pos[0] + d[0], pos[1] + d[1], pos[2] + d[2]]);
            }
        }
        let blocked = target
            .iter()
            .any(|(pos, block)| self.block_at(*pos) != Some(*block) && !reachable.contains(pos));
        let writes = target
            .into_iter()
            .filter(|(pos, block)| reachable.contains(pos) && self.block_at(*pos) != Some(*block))
            .collect();
        (writes, blocked)
    }

    /// Advance at most one due tree, preserving absolute deadlines after
    /// restart and deferring blocked final stages at their normal interval.
    pub(crate) fn tick_tree_growth_safe(&mut self) {
        for _ in 0..TREE_GROW_STAGES_PER_TICK {
            let now = *self.data.tick;
            let Some((due, id)) = self
                .data
                .tree_due
                .iter()
                .find(|((due_tick, _), _)| *due_tick <= now)
                .map(|(key, _)| *key)
            else {
                break;
            };
            let key = (due, id);
            self.data.tree_due.remove(&key);
            let Some(mut life) = self.data.tree_lives.get(&id).cloned() else {
                continue;
            };
            let TreeState::Regrowing {
                next_stage,
                next_tick,
                repair_age,
            } = life.state
            else {
                continue;
            };
            debug_assert_eq!(due, next_tick);
            let (writes, blocked) = self.tree_stage_writes(&life, next_stage, repair_age);
            self.write_tree_blocks_safe(&writes);
            if next_stage + 1 < TREE_GROW_AGES.len() as u8 || blocked {
                life.state = TreeState::Regrowing {
                    next_stage: next_stage
                        .saturating_add(1)
                        .min((TREE_GROW_AGES.len() - 1) as u8),
                    next_tick: due.saturating_add(self.growth_interval(&life)),
                    repair_age,
                };
                self.data.tree_due.insert(
                    match life.state {
                        TreeState::Regrowing { next_tick, .. } => (next_tick, id),
                        TreeState::Standing => unreachable!(),
                    },
                    (),
                );
            } else {
                life.state = TreeState::Standing;
            }
            self.data.tree_lives.insert(id, life);
        }
    }
}
