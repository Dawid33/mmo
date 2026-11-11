//! Prefabs in a running game: what a client sends (`GameEventKind::Prefab`) and the
//! per-player table of registered prefabs it acts on.
//!
//! The durable library is the player's own files; the world never stores one. To use a
//! prefab in a game the client registers its resolved form; the server validates it
//! again (it is untrusted input) and keeps it in that player's `Client`, where it is
//! ordinary rollback state that travels with the player across seams. Registrations
//! last for the session: a parked record keeps none (`parked_client`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{PrefabKey, ResolvedPrefab, Trust};
use crate::{ClientId, Rollback};

/// Most prefabs one player may have registered at a time.
pub const MAX_REGISTERED_PREFABS: usize = 8;
/// Most elements (`ResolvedPrefab::element_count`) one player's registered prefabs may
/// hold in total: about 100 KB of state at worst.
pub const MAX_REGISTERED_ELEMENTS: usize = 8192;

/// One registered prefab. The definition sits behind a shared pointer with its key
/// computed once, so hashing, undo copies and seam crossings do not walk it: the state
/// hash covers the key alone (the key IS the hash of the definition). On the wire it is
/// the definition alone; the key is recomputed on arrival, never trusted.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisteredPrefab {
    pub key: PrefabKey,
    pub def: Arc<ResolvedPrefab>,
}

impl RegisteredPrefab {
    pub fn new(def: ResolvedPrefab) -> Self {
        Self { key: def.key(), def: Arc::new(def) }
    }
}

impl state_hash::StateHash for RegisteredPrefab {
    fn state_hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.key.state_hash(h);
    }
}

impl Serialize for RegisteredPrefab {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (*self.def).serialize(s)
    }
}

impl<'de> Deserialize<'de> for RegisteredPrefab {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        ResolvedPrefab::deserialize(d).map(Self::new)
    }
}

/// Which prefab a stamp lays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrefabRef {
    /// One the stamping player has registered.
    Registered(PrefabKey),
    /// One of the game's own, by `prefab_name_key` (debug and the editor).
    Builtin(u32),
}

/// What a client asks for; carried by `GameEventKind::Prefab`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum PrefabCommand {
    Register(Box<ResolvedPrefab>),
    Deregister(PrefabKey),
    /// Lay a prefab with its anchor on world cell `anchor` (studs x, plates y, studs z),
    /// turned `turn` quarter turns. Developer-grade, like `Order::CarveStud` /
    /// `FillStud`: no cost, no survival rules.
    Stamp { prefab: PrefabRef, anchor: [i64; 3], turn: u8 },
}

// Compact on purpose: events are logged with `{:?}`, and a definition is thousands of parts.
impl std::fmt::Debug for PrefabCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrefabCommand::Register(p) => write!(f, "Register({:?}, {} elements)", p.name, p.element_count()),
            PrefabCommand::Deregister(k) => write!(f, "Deregister({k:?})"),
            PrefabCommand::Stamp { prefab, anchor, turn } => write!(f, "Stamp({prefab:?} at {anchor:?} turn {turn})"),
        }
    }
}

impl Rollback {
    /// Registers `prefab` for `client`. `false` with nothing changed when the client is
    /// unknown, the prefab is not valid for a player, or a cap would be passed;
    /// registering a prefab that already is changes nothing and is `true`.
    pub fn register_prefab_safe(&mut self, client: ClientId, prefab: ResolvedPrefab) -> bool {
        let Some(c) = self.data.clients.get(&client) else { return false };
        if prefab.validate(Trust::Player).is_err() {
            return false;
        }
        let entry = RegisteredPrefab::new(prefab);
        if c.prefabs.contains_key(&entry.key) {
            return true;
        }
        let used: usize = c.prefabs.values().map(|p| p.def.element_count()).sum();
        if c.prefabs.len() >= MAX_REGISTERED_PREFABS || used + entry.def.element_count() > MAX_REGISTERED_ELEMENTS {
            return false;
        }
        let Some(c) = self.data.clients.get_mut(&client) else { return false };
        c.prefabs.insert(entry.key, entry);
        true
    }

    /// Removes `client`'s registration of `key`; `false` when there was none.
    pub fn deregister_prefab_safe(&mut self, client: ClientId, key: PrefabKey) -> bool {
        if !self.data.clients.get(&client).is_some_and(|c| c.prefabs.contains_key(&key)) {
            return false;
        }
        let Some(c) = self.data.clients.get_mut(&client) else { return false };
        c.prefabs.remove(&key).is_some()
    }
}
