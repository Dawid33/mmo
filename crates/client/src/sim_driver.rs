//! Drives the sim from the Bevy schedule on wasm, replacing the native
//! tick-generator thread, tokio/netcode thread, and blocking game thread.
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use crossbeam::channel::{Receiver, Sender};
use game::{ClientUpdateEvent, GameEventKind, ServerPacket, TICK_RATE};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use crate::local_server::LocalServer;
use crate::GameInstanceManager;

pub struct SimDriver {
    manager: GameInstanceManager,
    server_recv: Receiver<ServerPacket>,
    local_server: LocalServer,
    client_tick_ms: f64,
    server_tick_ms: f64,
}

/// The sim state is `Send` but not `Sync` (interior mutability inside
/// `GameData`'s undo wrappers), so it can't be a `Resource` directly.
/// `SyncCell` only hands out `&mut` access, which makes it unconditionally
/// `Sync` — enough for the `Resource` bound.
#[derive(Resource)]
pub struct SimDriverRes(pub SyncCell<SimDriver>);

pub fn start_wasm_sim() -> (
    SimDriverRes,
    Sender<GameEventKind>,
    Receiver<ClientUpdateEvent>,
) {
    let (game_send, game_recv) = crossbeam::channel::unbounded();
    let (client_send, client_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();
    // The addr is unused on wasm; GameInstanceManager::new keeps one
    // signature on both targets.
    let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
    let mut manager =
        GameInstanceManager::new(game_send.clone(), game_recv, client_send, dummy_addr);
    let local_server = LocalServer::new(manager.client_packet_recv(), server_send)
        .expect("failed to build offline world");
    manager.start();
    (
        SimDriverRes(SyncCell::new(SimDriver {
            manager,
            server_recv,
            local_server,
            client_tick_ms: 0.0,
            server_tick_ms: 0.0,
        })),
        game_send,
        client_recv,
    )
}

/// Once per frame: run due server ticks, run due client ticks, then drain
/// both directions of traffic. Mirrors what the native threads do with
/// sleeps and blocking select.
pub fn drive_sim(time: Res<Time>, mut driver: ResMut<SimDriverRes>) {
    let SimDriver {
        manager,
        server_recv,
        local_server,
        client_tick_ms,
        server_tick_ms,
    } = driver.0.get();

    let dt_ms = time.delta_secs_f64() * 1000.0;

    // Authoritative sim at fixed TICK_RATE (ms per tick), like the server's
    // tick thread. Cap catch-up to avoid a spiral after a background tab.
    *server_tick_ms = (*server_tick_ms + dt_ms).min(10.0 * TICK_RATE as f64);
    while *server_tick_ms >= TICK_RATE as f64 {
        *server_tick_ms -= TICK_RATE as f64;
        local_server.tick();
    }

    // Predicted client sim at the adaptive rate (SyncClock adjusts it),
    // like the native tick-generator thread.
    let rate = manager.tick_rate_ms().max(1) as f64;
    *client_tick_ms = (*client_tick_ms + dt_ms).min(10.0 * rate);
    while *client_tick_ms >= rate {
        *client_tick_ms -= rate;
        manager.send_tick();
    }

    local_server.pump().expect("offline server crashed");
    if !manager.pump(server_recv).expect("game sim crashed") {
        info!("sim received Quit");
    }
}
