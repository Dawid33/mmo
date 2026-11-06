//! Where the predicted regions run: inline on the caller's thread (harnesses,
//! wasm, `VOXEL_CLIENT_REGION_THREADS=0`) or one OS thread per region, like
//! the server's region threads. Either way the manager only sends
//! [`RegionCmd`]s and reads [`RegionReport`]s, so a region's results never
//! depend on the host.
use std::collections::BTreeMap;

use game::{ClientUpdateEvent, Region, RegionId, Rollback};

use super::runner::{handle, RegionCmd, RegionReport, RunnerCtx, RunnerState};

/// Build a client region from its snapshot: the render bridge gets its copy
/// (`NewRegion`, sent before any update this region emits), the snapshot
/// itself becomes the region.
pub(crate) fn build_region(ctx: &RunnerCtx, snapshot: Rollback, extent: Option<game::MapExtent>) -> Region {
    let (send, recv) = crossbeam::channel::unbounded();
    let _ = ctx.client_event_send.send(ClientUpdateEvent::NewRegion(ctx.id, (*snapshot.data).clone(), recv));
    let mut region = Region::new(snapshot, Some(send), ctx.id, Some(ctx.client_id));
    region.set_map_extent(extent);
    region
}

pub enum RegionHost {
    Inline(InlineHost),
    #[cfg(not(target_arch = "wasm32"))]
    Threaded(ThreadedHost),
}

/// Every region on the caller's thread, run synchronously in command order.
/// Holds a real `game::World`, so harnesses can read and edit region state.
pub struct InlineHost {
    world: game::World,
    states: BTreeMap<RegionId, (RunnerState, RunnerCtx)>,
    queue: Vec<(RegionId, RegionReport)>,
}

impl InlineHost {
    fn send(&mut self, id: RegionId, cmd: RegionCmd) -> bool {
        let (Some(region), Some((st, ctx))) = (self.world.regions.get_mut(&id), self.states.get_mut(&id)) else {
            return false;
        };
        let mut out = Vec::new();
        let running = handle(region, st, ctx, cmd, &mut out);
        self.queue.extend(out.into_iter().map(|r| (id, r)));
        if !running {
            self.world.remove_region(&id);
            self.states.remove(&id);
        }
        true
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use threaded::ThreadedHost;

#[cfg(not(target_arch = "wasm32"))]
mod threaded {
    use super::*;
    use crossbeam::channel::{Receiver, Sender};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// To a region thread: a command, or (tests) "stop and hand the region back".
    pub(super) enum ToRegion {
        Cmd(RegionCmd),
        #[cfg_attr(not(test), allow(dead_code))]
        Take,
    }

    /// A report and the generation of the region thread that sent it: a
    /// region stopped and spawned again under the same id is a new
    /// generation, and the old one's late reports are dropped.
    type Tagged = (RegionId, u64, RegionReport);

    struct Live {
        send: Sender<ToRegion>,
        generation: u64,
        handle: JoinHandle<Option<Region>>,
    }

    /// One OS thread per region, named `region-<x>,<z>`. Commands queue on
    /// the region's channel and never wait for another region; reports come
    /// back on one shared channel.
    pub struct ThreadedHost {
        regions: BTreeMap<RegionId, Live>,
        /// Stopped threads still finishing their queue; joined once done
        /// (never blocking the caller) or on drop.
        retired: Vec<(RegionId, JoinHandle<Option<Region>>)>,
        report_send: Sender<Tagged>,
        report_recv: Receiver<Tagged>,
        /// Reports already read off the channel (by `settle` or the caller's
        /// `select!`), still to be handed out by `drain`.
        early: Vec<Tagged>,
        next_generation: u64,
        next_ping: u64,
        /// Region threads alive right now (test hook).
        live: Arc<AtomicUsize>,
    }

    /// How long `settle` waits between checks that no region thread died.
    const SETTLE_POLL: Duration = Duration::from_millis(50);

    impl ThreadedHost {
        pub(super) fn new() -> Self {
            let (report_send, report_recv) = crossbeam::channel::unbounded();
            Self {
                regions: BTreeMap::new(),
                retired: Vec::new(),
                report_send,
                report_recv,
                early: Vec::new(),
                next_generation: 0,
                next_ping: 0,
                live: Arc::new(AtomicUsize::new(0)),
            }
        }

        pub(super) fn spawn(&mut self, ctx: RunnerCtx, snapshot: Rollback, extent: Option<game::MapExtent>) {
            let id = ctx.id;
            self.stop(id);
            // Earlier threads of this id finish first (on the new thread, so
            // the caller never waits): their `RemoveRegion` reaches the
            // bridge before this one's `NewRegion`.
            let (previous, others): (Vec<_>, Vec<_>) = self.retired.drain(..).partition(|(r, _)| *r == id);
            self.retired = others;
            let previous: Vec<_> = previous.into_iter().map(|(_, h)| h).collect();
            let generation = self.next_generation;
            self.next_generation += 1;
            let (send, recv) = crossbeam::channel::unbounded();
            let out = self.report_send.clone();
            let live = self.live.clone();
            live.fetch_add(1, Ordering::SeqCst);
            let handle = std::thread::Builder::new()
                .name(format!("region-{},{}", id.x, id.z))
                .spawn(move || {
                    for handle in previous {
                        let _ = handle.join();
                    }
                    let region = region_thread(ctx, snapshot, extent, recv, out, generation);
                    live.fetch_sub(1, Ordering::SeqCst);
                    region
                })
                .expect("spawn region thread");
            self.regions.insert(id, Live { send, generation, handle });
        }

        pub(super) fn send(&mut self, id: RegionId, cmd: RegionCmd) -> bool {
            match self.regions.get(&id) {
                Some(live) => live.send.send(ToRegion::Cmd(cmd)).is_ok(),
                None => false,
            }
        }

        pub(super) fn stop(&mut self, id: RegionId) -> bool {
            let Some(live) = self.regions.remove(&id) else {
                return false;
            };
            // The thread drops its region itself, off the caller's thread,
            // and then takes it off the bridge.
            let _ = live.send.send(ToRegion::Cmd(RegionCmd::Stop));
            self.retired.push((id, live.handle));
            true
        }

        pub(super) fn contains(&self, id: &RegionId) -> bool {
            self.regions.contains_key(id)
        }

        pub(super) fn ids(&self) -> Vec<RegionId> {
            self.regions.keys().copied().collect()
        }

        /// The shared report channel, for the caller's `select!`; hand what
        /// it reads back through [`Self::accept`].
        pub(super) fn report_recv(&self) -> Receiver<Tagged> {
            self.report_recv.clone()
        }

        pub(super) fn accept(&mut self, tagged: Tagged) {
            self.early.push(tagged);
        }

        pub(super) fn drain(&mut self) -> Vec<(RegionId, RegionReport)> {
            self.reap();
            let mut all = std::mem::take(&mut self.early);
            all.extend(self.report_recv.try_iter());
            let mut out: Vec<(RegionId, RegionReport)> = all
                .into_iter()
                .filter(|(id, generation, _)| self.regions.get(id).is_some_and(|l| l.generation == *generation))
                .map(|(id, _, report)| (id, report))
                .collect();
            // A region thread that ended without being stopped panicked:
            // report it, so the session ends instead of waiting on it.
            let dead: Vec<RegionId> =
                self.regions.iter().filter(|(_, l)| l.handle.is_finished()).map(|(id, _)| *id).collect();
            for id in dead {
                let live = self.regions.remove(&id).expect("listed above");
                let why = match live.handle.join() {
                    Err(_) => format!("region {id:?} thread panicked"),
                    Ok(_) => format!("region {id:?} thread ended unexpectedly"),
                };
                log::error!("{why}");
                out.push((id, RegionReport::Failed(game::GameError::Disconnected(why))));
            }
            out
        }

        /// Block until every region has run every command sent so far.
        pub(super) fn settle(&mut self) {
            let ping = self.next_ping;
            self.next_ping += 1;
            let mut waiting: std::collections::BTreeSet<RegionId> = BTreeMap::keys(&self.regions).copied().collect();
            for id in waiting.clone() {
                if !self.send(id, RegionCmd::Ping(ping)) {
                    waiting.remove(&id);
                }
            }
            while !waiting.is_empty() {
                match self.report_recv.recv_timeout(SETTLE_POLL) {
                    Ok((id, generation, report)) => {
                        let current = self.regions.get(&id).is_some_and(|l| l.generation == generation);
                        if current && matches!(report, RegionReport::Pong(p) if p == ping) {
                            waiting.remove(&id);
                        }
                        self.early.push((id, generation, report));
                    }
                    Err(_) => {
                        // A dead thread never pongs; `drain` reports it.
                        waiting.retain(|id| self.regions.get(id).is_some_and(|l| !l.handle.is_finished()));
                    }
                }
            }
        }

        /// Join the stopped threads that have finished.
        fn reap(&mut self) {
            let (done, running): (Vec<_>, Vec<_>) = self.retired.drain(..).partition(|(_, h)| h.is_finished());
            self.retired = running;
            for (_, handle) in done {
                let _ = handle.join();
            }
        }

        #[cfg(test)]
        pub(super) fn stop_and_take(&mut self, id: RegionId) -> Option<Region> {
            let live = self.regions.remove(&id)?;
            live.send.send(ToRegion::Take).ok()?;
            live.handle.join().ok().flatten()
        }

        #[cfg(test)]
        pub(super) fn live_threads(&self) -> Arc<AtomicUsize> {
            self.live.clone()
        }
    }

    impl Drop for ThreadedHost {
        fn drop(&mut self) {
            for id in self.ids() {
                self.stop(id);
            }
            for (_, handle) in self.retired.drain(..) {
                let _ = handle.join();
            }
        }
    }

    /// A region thread: build the region here, then run commands until
    /// `Stop` (region dropped here) or `Take` (region returned).
    fn region_thread(
        ctx: RunnerCtx,
        snapshot: Rollback,
        extent: Option<game::MapExtent>,
        recv: Receiver<ToRegion>,
        out: Sender<Tagged>,
        generation: u64,
    ) -> Option<Region> {
        let mut region = build_region(&ctx, snapshot, extent);
        let mut st = RunnerState::default();
        let mut reports = Vec::new();
        while let Ok(msg) = recv.recv() {
            let cmd = match msg {
                ToRegion::Cmd(cmd) => cmd,
                ToRegion::Take => return Some(region),
            };
            let running = handle(&mut region, &mut st, &ctx, cmd, &mut reports);
            for report in reports.drain(..) {
                let _ = out.send((ctx.id, generation, report));
            }
            if !running {
                // Stopped: off the bridge, after everything it sent there.
                drop(region);
                let _ = ctx.client_event_send.send(ClientUpdateEvent::RemoveRegion(ctx.id));
                return None;
            }
        }
        None
    }
}

impl RegionHost {
    pub fn inline() -> Self {
        RegionHost::Inline(InlineHost { world: game::World::new(), states: BTreeMap::new(), queue: Vec::new() })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn threaded() -> Self {
        RegionHost::Threaded(ThreadedHost::new())
    }

    pub fn is_threaded(&self) -> bool {
        !matches!(self, RegionHost::Inline(_))
    }

    /// Start region `ctx.id` from `snapshot`, replacing one already held.
    pub fn spawn(&mut self, ctx: RunnerCtx, snapshot: Rollback, extent: Option<game::MapExtent>) {
        match self {
            RegionHost::Inline(h) => {
                let id = ctx.id;
                let region = build_region(&ctx, snapshot, extent);
                h.world.load(&id, region);
                h.states.insert(id, (RunnerState::default(), ctx));
            }
            #[cfg(not(target_arch = "wasm32"))]
            RegionHost::Threaded(h) => h.spawn(ctx, snapshot, extent),
        }
    }

    /// Send `cmd` to region `id`; false if it is not held.
    pub fn send(&mut self, id: RegionId, cmd: RegionCmd) -> bool {
        match self {
            RegionHost::Inline(h) => h.send(id, cmd),
            #[cfg(not(target_arch = "wasm32"))]
            RegionHost::Threaded(h) => h.send(id, cmd),
        }
    }

    /// Send a command to every region, in id order.
    pub fn broadcast(&mut self, cmd: impl Fn() -> RegionCmd) {
        for id in self.ids() {
            self.send(id, cmd());
        }
    }

    /// Stop region `id` and take it off the render bridge
    /// (`RemoveRegion`, ordered after everything the region sent it); true
    /// if it was held.
    pub fn stop(&mut self, id: RegionId) -> bool {
        match self {
            RegionHost::Inline(h) => {
                let state = h.states.remove(&id);
                let held = h.world.remove_region(&id).is_some();
                if let (true, Some((_, ctx))) = (held, state) {
                    let _ = ctx.client_event_send.send(ClientUpdateEvent::RemoveRegion(id));
                }
                held
            }
            #[cfg(not(target_arch = "wasm32"))]
            RegionHost::Threaded(h) => h.stop(id),
        }
    }

    pub fn contains(&self, id: &RegionId) -> bool {
        match self {
            RegionHost::Inline(h) => h.world.region_exists(id),
            #[cfg(not(target_arch = "wasm32"))]
            RegionHost::Threaded(h) => h.contains(id),
        }
    }

    pub fn ids(&self) -> Vec<RegionId> {
        match self {
            RegionHost::Inline(h) => h.world.loaded_regions(),
            #[cfg(not(target_arch = "wasm32"))]
            RegionHost::Threaded(h) => h.ids(),
        }
    }

    /// Every report produced so far, in order; never blocks.
    pub fn drain(&mut self) -> Vec<(RegionId, RegionReport)> {
        match self {
            RegionHost::Inline(h) => std::mem::take(&mut h.queue),
            #[cfg(not(target_arch = "wasm32"))]
            RegionHost::Threaded(h) => h.drain(),
        }
    }

    /// Wait until every region has run every command sent so far (inline:
    /// always true already).
    pub fn settle(&mut self) {
        match self {
            RegionHost::Inline(_) => {}
            #[cfg(not(target_arch = "wasm32"))]
            RegionHost::Threaded(h) => h.settle(),
        }
    }

    /// Wakes the caller's `select!` when a region thread reports (`never()`
    /// inline); hand each message it reads to [`Self::accept`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn report_recv(&self) -> crossbeam::channel::Receiver<(RegionId, u64, RegionReport)> {
        match self {
            RegionHost::Inline(_) => crossbeam::channel::never(),
            RegionHost::Threaded(h) => h.report_recv(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn accept(&mut self, tagged: (RegionId, u64, RegionReport)) {
        if let RegionHost::Threaded(h) = self {
            h.accept(tagged);
        }
    }

    /// The regions' state, on the inline host only (harness hook).
    pub fn world(&self) -> Option<&game::World> {
        match self {
            RegionHost::Inline(h) => Some(&h.world),
            #[cfg(not(target_arch = "wasm32"))]
            RegionHost::Threaded(_) => None,
        }
    }

    pub fn world_mut(&mut self) -> Option<&mut game::World> {
        match self {
            RegionHost::Inline(h) => Some(&mut h.world),
            #[cfg(not(target_arch = "wasm32"))]
            RegionHost::Threaded(_) => None,
        }
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn stop_and_take(&mut self, id: RegionId) -> Option<Region> {
        match self {
            RegionHost::Threaded(h) => h.stop_and_take(id),
            RegionHost::Inline(_) => None,
        }
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn live_threads(&self) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        match self {
            RegionHost::Threaded(h) => h.live_threads(),
            RegionHost::Inline(_) => unreachable!("threaded host only"),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::regions::runner::RegionReport;
    use game::{ClientId, GameEventKind, RegionCoords};

    const CLIENT: ClientId = 0;
    const RC: RegionCoords = RegionCoords { x: 0, z: 0 };

    /// A snapshot holding `CLIENT`, as the server would send it.
    fn snapshot() -> Rollback {
        let mut server = Region::from_chunks(RC, Vec::new());
        server.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
        server.commit_server_event();
        server.data().clone()
    }

    fn ctx() -> RunnerCtx {
        let (server_send, _) = crossbeam::channel::unbounded();
        let (client_event_send, _) = crossbeam::channel::unbounded();
        RunnerCtx { id: RC, client_id: CLIENT, server_send, client_event_send, debug_flags: Default::default() }
    }

    /// `n` ticks of the clock from world tick 0 (every snapshot here starts
    /// at 0).
    fn ticks(host: &mut RegionHost, n: usize) {
        for t in 1..=n {
            host.broadcast(|| RegionCmd::Tick(t as game::Tick));
        }
    }

    #[test]
    fn threaded_tick_matches_inline() {
        let mut inline = RegionHost::inline();
        let mut threaded = RegionHost::threaded();
        inline.spawn(ctx(), snapshot(), None);
        threaded.spawn(ctx(), snapshot(), None);
        ticks(&mut inline, 50);
        ticks(&mut threaded, 50);
        threaded.settle();
        let region = threaded.stop_and_take(RC).expect("threaded region");
        assert_eq!(region.current_tick(), 50);
        assert_eq!(
            game::state_hash(region.data()),
            game::state_hash(inline.world().unwrap().data(&RC)),
            "the same commands give the same region on a thread"
        );
    }

    #[test]
    fn stop_then_respawn_same_region() {
        let mut host = RegionHost::threaded();
        host.spawn(ctx(), snapshot(), None);
        ticks(&mut host, 20);
        assert!(host.stop(RC));
        assert!(!host.contains(&RC));
        host.spawn(ctx(), snapshot(), None);
        ticks(&mut host, 3);
        host.settle();
        assert_eq!(host.ids(), vec![RC]);
        let ticked = host
            .drain()
            .into_iter()
            .filter(|(_, r)| matches!(r, RegionReport::Ticked { .. }))
            .count();
        assert_eq!(ticked, 3, "reports of the stopped region never reach the new one");
    }

    /// A region released or re-received while its thread is still
    /// building: the bridge still sees NewRegion → RemoveRegion → NewRegion,
    /// never a removal overtaken by the old generation's NewRegion.
    #[test]
    fn respawn_keeps_bridge_order() {
        let (client_event_send, bridge) = crossbeam::channel::unbounded();
        let ctx = RunnerCtx { client_event_send, ..ctx() };
        let mut host = RegionHost::threaded();
        host.spawn(ctx.clone(), snapshot(), None);
        host.stop(RC);
        host.spawn(ctx, snapshot(), None);
        host.settle();
        drop(host);
        let order: Vec<&str> = bridge
            .try_iter()
            .filter_map(|e| match e {
                ClientUpdateEvent::NewRegion(..) => Some("new"),
                ClientUpdateEvent::RemoveRegion(_) => Some("remove"),
                _ => None,
            })
            .collect();
        assert_eq!(&order[..3], ["new", "remove", "new"], "{order:?}");
    }

    #[test]
    fn drop_joins_all_threads() {
        let mut host = RegionHost::threaded();
        for x in 0..3 {
            let mut c = ctx();
            c.id = RegionCoords::new(x, 0);
            let mut server = Region::from_chunks(c.id, Vec::new());
            server.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
            server.commit_server_event();
            host.spawn(c, server.data().clone(), None);
        }
        ticks(&mut host, 5);
        let live = host.live_threads();
        assert_eq!(live.load(std::sync::atomic::Ordering::SeqCst), 3);
        drop(host);
        assert_eq!(live.load(std::sync::atomic::Ordering::SeqCst), 0, "dropping the host joins every region thread");
    }
}
