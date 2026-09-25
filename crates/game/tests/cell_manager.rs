//! Cell mode through the WorldManager (phase 1b of
//! docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md).
use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, InlineLodBuilder,
    InlineSpawner, LodBuilder, RegionCoords, RegionOutput, ServerEvent, ServerPacket, SubChunk,
    WorldManager, REGION_CHUNKS, SPAWN_REGION, UNLOAD_GRACE_MS,
};

pub const CC: usize = 8;

pub struct Harness {
    pub manager: WorldManager<InlineSpawner>,
    region_out: Receiver<(RegionCoords, RegionOutput)>,
    packets: Receiver<(Option<ClientId>, ServerPacket)>,
}

/// A flat region: one dense floor sub-chunk in every column.
pub fn harness(cell_chunks: usize) -> Harness {
    let (out_send, packets) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let generator = Box::new(|_rc: RegionCoords| -> Vec<(ColumnCoords, Column)> {
        let mut cols = Vec::new();
        for x in 0..REGION_CHUNKS {
            for z in 0..REGION_CHUNKS {
                let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
                let air = Block::new(BlockId::AIR);
                cols.push((ColumnCoords::new(x, z), Column::new(0, air, air, vec![sub])));
            }
        }
        cols
    });
    let (lod_result_send, lod_result_recv) = unbounded();
    let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
        coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
        out: lod_result_send,
    });
    Harness {
        manager: WorldManager::new(
            InlineSpawner::with_cells(cell_chunks),
            generator,
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        ),
        region_out,
        packets,
    }
}

impl Harness {
    pub fn settle(&mut self, now_ms: u64) {
        for _ in 0..2 {
            self.manager.spawner_mut().pump();
            while let Ok((rc, out)) = self.region_out.try_recv() {
                self.manager.handle_region_output(rc, out, now_ms);
            }
            self.manager.pump_lod_results();
        }
    }
    pub fn event(&mut self, ev: ServerEvent, now_ms: u64) {
        self.manager.handle_server_event(ev, now_ms);
        self.settle(now_ms);
    }
    pub fn tick(&mut self, now_ms: u64) {
        self.manager.spawner_mut().tick_all();
        self.settle(now_ms);
    }
    pub fn drain(&mut self) -> Vec<(Option<ClientId>, ServerPacket)> {
        self.packets.try_iter().collect()
    }
    pub fn active_cells(&mut self, rc: RegionCoords) -> usize {
        let mut n = 0;
        self.manager.spawner_mut().with_region(rc, |r| n = r.data().cells.active.len());
        n
    }
}

#[test]
fn a_joining_player_gets_a_cell_mode_home_with_its_cells_active() {
    let mut h = harness(CC);
    h.event(ServerEvent::ClientConnected(0), 0);
    let mut cell_chunks = 0;
    h.manager.spawner_mut().with_region(SPAWN_REGION, |r| cell_chunks = r.data().cells.cell_chunks);
    assert_eq!(cell_chunks, CC);
    assert_eq!(h.active_cells(SPAWN_REGION), 9, "the centre spawn cell's full 3×3");
}

#[test]
fn the_join_snapshot_holds_the_active_cells() {
    let mut h = harness(CC);
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
    let snap = h.drain().into_iter().find_map(|(_, p)| match p {
        ServerPacket::Region(rc, rb) if rc == SPAWN_REGION => Some(rb),
        _ => None,
    });
    let rb = snap.expect("snapshot delivered");
    assert_eq!(rb.data.cells.active.len(), 9);
    assert!(rb.data.ecs.chunk.iter().filter(|(_, c)| c.is_some()).count() > 0);
}

fn release_after_grace(cell_chunks: usize) -> bool {
    let mut h = harness(cell_chunks);
    let rc = RegionCoords::new(1, 0);
    h.event(ServerEvent::ClientConnected(0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 0), 0);
    h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), 0), 0);
    h.manager.maintain(UNLOAD_GRACE_MS + 1);
    h.settle(UNLOAD_GRACE_MS + 1);
    h.manager.running_regions().contains(&rc)
}

#[test]
fn a_cell_mode_region_never_parks() {
    assert!(release_after_grace(CC), "cell regions keep their store: no parking");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_legacy_spawner_still_parks() {
    assert!(!release_after_grace(0));
}

mod convergence {
    use super::*;
    use game::{GameEvent, GameEventKind, GameDataUpdate, InputEvent, Key, Region};

    struct Client {
        region: Option<Region>,
        send: crossbeam::channel::Sender<GameDataUpdate>,
        _recv: Receiver<GameDataUpdate>,
    }

    /// Route every server packet for the home region into the client replica.
    fn feed(h: &mut Harness, c: &mut Client) {
        for (_, p) in h.drain() {
            match p {
                ServerPacket::Region(rc, rb) if rc == SPAWN_REGION => {
                    c.region = Some(Region::new(rb, Some(c.send.clone()), rc, Some(0)));
                }
                ServerPacket::GameEvent(ev) if ev.region_id == SPAWN_REGION => {
                    if let Some(r) = c.region.as_mut() {
                        r.reconcile(ev).unwrap();
                    }
                }
                _ => {}
            }
        }
    }

    fn predict(c: &mut Client, lead: usize) {
        let r = c.region.as_mut().expect("joined");
        while r.pending_event_ids().len() < lead {
            r.handle_event(GameEventKind::Tick).unwrap();
        }
    }

    fn input(h: &mut Harness, c: &mut Client, ev: InputEvent, now: u64) {
        c.region.as_mut().unwrap().handle_event(GameEventKind::PlayerInput(0, ev.clone())).unwrap();
        let gev = GameEvent::new(GameEventKind::PlayerInput(0, ev), 0, SPAWN_REGION);
        h.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(gev), 0), now);
        feed(h, c);
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn a_client_joined_through_the_manager_converges_while_cells_load() {
        game::set_hash_mode(game::HashMode::Full);
        let mut h = harness(CC);
        let (send, recv) = unbounded();
        let mut c = Client { region: None, send, _recv: recv };
        h.event(ServerEvent::ClientConnected(0), 0);
        h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
        feed(&mut h, &mut c);
        assert!(c.region.is_some(), "snapshot delivered");
        let mut now = 0u64;
        input(&mut h, &mut c, InputEvent::FpsCam(true), now);
        input(&mut h, &mut c, InputEvent::Key { key: Key::KeyW, pressed: true }, now);
        // −Z from the centre spawn: ~21 m, inside the region, across cell edges.
        for i in 0..100usize {
            predict(&mut c, 1 + i % 3);
            now += 50;
            h.tick(now);
            feed(&mut h, &mut c);
        }
        for _ in 0..8 {
            if c.region.as_ref().unwrap().pending_event_ids().is_empty() {
                break;
            }
            now += 50;
            h.tick(now);
            feed(&mut h, &mut c);
        }
        let r = c.region.as_ref().unwrap();
        assert!(r.pending_event_ids().is_empty(), "client still ahead");
        let mut server_hash = 0;
        h.manager.spawner_mut().with_region(SPAWN_REGION, |s| server_hash = game::state_hash(s.data()));
        assert_eq!(server_hash, game::state_hash(r.data()), "client diverged from the server");
        assert!(r.data().cells.active.len() > 9, "cells loaded around the walking player");
    }
}
