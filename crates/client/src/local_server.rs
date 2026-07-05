//! Embedded single-player "server" for the wasm build: mirrors the dumb-router
//! event loop in crates/server/src/main.rs against a local World, behind the
//! same channel interface netcode::ServerConnection provides on native. A
//! future WebTransport/WebSocket transport replaces this without touching
//! GameInstanceManager.
use std::collections::BTreeMap;
use std::time::Duration;

use crossbeam::channel::{Receiver, Sender};
use game::{
    ClientId, ClientPacket, GameError, GameEvent, GameEventKind, RegionCoords, RegionId,
    ServerPacket, World, TICK_RATE,
};

/// The only client in an offline world.
pub const LOCAL_CLIENT_ID: ClientId = 0;

pub struct LocalServer {
    world: World,
    recv: Receiver<ClientPacket>,
    send: Sender<ServerPacket>,
    results_buffer: BTreeMap<RegionId, Result<GameEvent, GameError>>,
}

impl LocalServer {
    pub fn new(
        recv: Receiver<ClientPacket>,
        send: Sender<ServerPacket>,
    ) -> Result<Self, GameError> {
        let mut world = World::basic();
        // Server-authoritative player creation, as on ClientConnected.
        let region_id = RegionCoords::new(0, 0);
        let event = world.handle_region_event(GameEventKind::CreateClient(LOCAL_CLIENT_ID), region_id)?;
        world.forget_last_event(&region_id);
        send.send(ServerPacket::GameEvent(event)).unwrap();
        Ok(Self {
            world,
            recv,
            send,
            results_buffer: BTreeMap::new(),
        })
    }

    /// Drain pending client packets without blocking.
    pub fn pump(&mut self) -> Result<(), GameError> {
        while let Ok(packet) = self.recv.try_recv() {
            match packet {
                ClientPacket::RequestPlayerRegion => {
                    let id = self.world.find_player(&LOCAL_CLIENT_ID);
                    self.send
                        .send(ServerPacket::PlayerRegion(id, LOCAL_CLIENT_ID))
                        .unwrap();
                }
                ClientPacket::RequestRegionConnection(id) => {
                    self.send
                        .send(self.world.build_region_server_packet(&id))
                        .unwrap();
                }
                ClientPacket::GameEvent(game_event) => match game_event.kind {
                    GameEventKind::Tick => (),
                    // Unlike the real server (which breaks its event loop), Quit is a no-op:
                    // there is no process to terminate in the embedded loopback.
                    GameEventKind::Quit => (),
                    kind => {
                        let event = self
                            .world
                            .handle_region_event(kind, game_event.region_id)?;
                        self.world.forget_last_event(&game_event.region_id);
                        self.send.send(ServerPacket::GameEvent(event)).unwrap();
                    }
                },
            }
        }
        Ok(())
    }

    /// Advance the authoritative sim one tick and broadcast results,
    /// mirroring ServerEvent::ServerTickTimer handling on the real server
    /// (including the every-10-ticks SyncClock, with zero RTT).
    pub fn tick(&mut self) {
        self.world.progress_world_one_tick(&mut self.results_buffer);
        for (id, result) in &self.results_buffer {
            self.send
                .send(ServerPacket::GameEvent(result.as_ref().unwrap().clone()))
                .unwrap();
            if self.world.current_tick(id) % 10 == 0 {
                self.send
                    .send(ServerPacket::SyncClock(
                        *id,
                        TICK_RATE,
                        self.world.current_tick(id),
                        Duration::ZERO,
                    ))
                    .unwrap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GameInstanceManager;
    use game::{ClientUpdateEvent, ServerPacket};
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

    /// Full offline handshake: manager + LocalServer wired over channels,
    /// pumped in lockstep like the wasm frame driver will.
    #[test]
    fn offline_handshake_loads_region() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded::<ServerPacket>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager =
            GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);

        let mut server = LocalServer::new(manager.client_packet_recv(), server_send).unwrap();
        manager.start();

        // A few frames of the wasm drive loop: server pump -> client pump.
        for _ in 0..4 {
            server.pump().unwrap();
            assert!(manager.pump(&server_recv).unwrap());
        }

        // Client received region + player identity. NewRegion carries the
        // render bridge's Receiver; keep it alive so the region's Sender
        // doesn't hit a closed channel when later ticks emit render updates.
        let mut saw_region = false;
        let mut saw_player = false;
        let mut bridge_recv = None;
        while let Ok(ev) = client_recv.try_recv() {
            match ev {
                ClientUpdateEvent::NewRegion(_, _, recv) => {
                    saw_region = true;
                    bridge_recv = Some(recv);
                }
                ClientUpdateEvent::SetPlayer(id) => {
                    saw_player = true;
                    assert_eq!(id, LOCAL_CLIENT_ID);
                }
                _ => {}
            }
        }
        assert!(saw_region, "client never received the region snapshot");
        assert!(saw_player, "client never learned its ClientId");

        // Server ticks advance the authoritative world and reach the client.
        server.tick();
        manager.pump(&server_recv).unwrap();

        // Client ticks advance the local prediction.
        manager.send_tick();
        assert!(manager.pump(&server_recv).unwrap());
        drop(bridge_recv);
    }
}
