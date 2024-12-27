use std::collections::BTreeMap;

pub type Tick = usize;
pub type EventId = usize;
pub type EntityId = usize;

pub enum ServerPacket {
    GameEvent(GameEvent),
}

pub enum ClientPacket {
    GameEvent(GameEvent),
}

pub struct GameEvent {
    id: EventId,
    tick: Tick,
    kind: GameEventKind,
}

pub enum GameEventKind {
    Tick,
}

pub enum RegionPacket {
    /// Give control over entity to another region.
    GrantAuthority,
    /// Create a shadow entity in another region.
    CreateEntity,
    /// Update a shadow entity in another region.
    UpdateEntity,
    /// Delete a shadow entity in another region.
    DeleteEntity,
    /// Acknowledge creation of shadow entity and return the shadow entities ID.
    AckEntity,
}

pub struct Region {
    data: GameData,
    shadows: BTreeMap<EntityId, Entity>,
    region_packet_buf: Vec<RegionPacket>,
}

struct Entity;

pub struct GameData {}

impl Region {
    pub fn from_file() {}
    pub fn new() {}
    pub fn reconcile(&mut self, event_id: EventId) -> Result<(), String> {
        Ok(())
    }
    pub fn rollback(&mut self, tick: Tick) -> Result<(), String> {
        Ok(())
    }
    pub fn event(&mut self, event: GameEvent) -> Option<&[RegionPacket]> {
        None
    }
}
