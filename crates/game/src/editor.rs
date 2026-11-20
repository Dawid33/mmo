//! Authoritative editor switching and inventory actions.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crate::{ClientId, IntentMsg, ItemStack, MAX_STACK, Motion, Rollback, TilePos, UiAction};

static EDITOR_REFUSALS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EditorObservability {
    pub refusals: u64,
}

/// Process-local counters for server telemetry. Simulation never reads them.
pub fn editor_observability() -> EditorObservability {
    EditorObservability {
        refusals: EDITOR_REFUSALS.load(Relaxed),
    }
}

fn refused() -> bool {
    EDITOR_REFUSALS.fetch_add(1, Relaxed);
    false
}

impl Rollback {
    /// Switches the authoritative editor capability for `client`. Entering
    /// may place an owned body at `at`; exit closes the editor screen state,
    /// preserves its cursor stack and clears flight.
    pub fn set_editor_safe(&mut self, client: ClientId, on: bool, at: Option<TilePos>) -> bool {
        let Some(current) = self.data.clients.get(&client).filter(|c| c.editor_allowed) else {
            return refused();
        };
        let at = match at {
            Some(at) => {
                if !self.data.player_entites.contains_key(&client) {
                    return refused();
                }
                let Some(at) = crate::adventure::clamp_tile(self, at) else {
                    return refused();
                };
                Some(at)
            }
            None => None,
        };
        if current.editor == on {
            return true;
        }

        let mut rest = None;
        if let Some(c) = self.data.clients.get_mut(&client) {
            c.editor = on;
            if on {
                c.mining = None;
                c.damaged.clear();
            } else {
                rest = c.leave_survival();
            }
        }
        if let Some(rest) = rest {
            self.drop_at_feet_safe(client, rest);
        }
        if on {
            if let Some(at) = at {
                self.place_body_safe(client, at);
            }
        } else if let Some(&key) = self.data.player_entites.get(&client) {
            if let Some(motion) = *self.data.ecs.motion.try_get(key) {
                if motion.flying || motion.last_jump_tap.is_some() {
                    let motion = Motion {
                        flying: false,
                        last_jump_tap: None,
                        ..motion
                    };
                    self.data.ecs.motion.set_safe(key, Some(motion));
                }
            }
        }
        true
    }

    /// Applies one authoritative editor palette action.
    pub(crate) fn editor_action_safe(&mut self, client: ClientId, action: UiAction) -> bool {
        let Some(c) = self.data.clients.get(&client) else {
            return refused();
        };
        if !c.editor_allowed || !c.editor {
            return refused();
        }
        let (requires_inventory, take, stack) = match action {
            UiAction::EditorTake { item, count } => (true, true, Some((item, count))),
            UiAction::EditorGive { item, count } => (false, false, Some((item, count))),
            UiAction::EditorTrash => (true, false, None),
            _ => return refused(),
        };
        if requires_inventory && !c.inventory_open {
            return refused();
        }
        let Some((item, count)) = stack else {
            if c.cursor.is_some() {
                if let Some(c) = self.data.clients.get_mut(&client) {
                    c.cursor = None;
                }
            }
            return true;
        };
        if !crate::item_exists(item) || !(1..=MAX_STACK).contains(&count) {
            return refused();
        }
        let stack = ItemStack { item, count };
        let mut rest = None;
        let mut old_cursor = None;
        if take {
            if let Some(c) = self.data.clients.get_mut(&client) {
                old_cursor = c.stow_cursor();
                c.cursor = Some(stack);
            }
        } else if let Some(c) = self.data.clients.get_mut(&client) {
            let count = c.inventory.add(stack);
            rest = (count > 0).then_some(ItemStack { count, ..stack });
        }
        for stack in old_cursor.into_iter().chain(rest) {
            self.drop_at_feet_safe(client, stack);
        }
        true
    }

    /// Places `client`'s selected hotbar block without consuming it.
    pub(crate) fn editor_place_safe(&mut self, client: ClientId, out: &mut Vec<IntentMsg>) -> bool {
        self.place_selected_safe(client, false, out)
    }
}
