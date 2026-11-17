//! Coordinator test helpers shared by `coordinator_rt.rs` and `roles.rs` (each test binary
//! includes this file as a module of its own): what a restart must resume, the row loaded as a
//! restart loads it, a host's `Attach`.
#![allow(dead_code)]

use game::history::CalendarTick;
use world_store::StoreHandle;

/// What a restart must resume exactly: the calendar (history tick and the calendar's anchor),
/// the site markers, the legends' length.
#[derive(Debug, PartialEq)]
pub struct Resumed {
    pub now: CalendarTick,
    pub anchor: ((u64, CalendarTick), u64),
    pub sites: Vec<game::SiteMarker>,
    pub legends: usize,
}

pub fn resumed(c: &game::Coordinator) -> Resumed {
    Resumed { now: c.history().now(), anchor: c.calendar_anchor(), sites: c.history().site_markers(), legends: c.history().log().len() }
}

/// The coordinator state the store holds, loaded as a restart loads it (as replica 0, at store
/// time 0); `None` before its first write.
pub fn stored_core(store: &StoreHandle, map: &game::WorldMap) -> Option<game::Coordinator> {
    let row = store.load_coordinator().unwrap()?;
    let grid = game::history::SocietyGrid::from_world_map(map);
    Some(game::Coordinator::load(&row.core, &row.history, grid, game::coordinator::ReplicaId(0), 0).expect("the projection loads"))
}

/// Host `host`'s `Attach` as boot `boot` at `epoch` and world generation `generation`, `online`
/// online, nothing running or handed, `pending_cut` its last unanswered cut, under `policy`.
pub fn attach_msg(
    host: game::HostId,
    epoch: u64,
    online: Vec<game::OnlinePlayer>,
    generation: u64,
    boot: u64,
    pending_cut: Option<game::CutId>,
    policy: game::CrashPolicy,
) -> game::HostMsg {
    game::HostMsg::Attach {
        host,
        epoch,
        running: Vec::new(),
        online,
        generation,
        boot,
        settled_below: 0,
        handed: Vec::new(),
        pending_cut,
        successions: Vec::new(),
        policy,
    }
}
