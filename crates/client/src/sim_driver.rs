//! The browser's sim, in place of the native tick-generator thread,
//! tokio/netcode thread, and blocking game thread. One [`SimDriverRes`] per
//! joined server (`session::web_flow`): built on Join server, dropped on
//! leaving, which hangs up its WebTransport connection.
//!
//! Two backends: normally the sim and its connection run in a Web Worker
//! ([`crate::web_worker::sim`]) and [`drive_sim`] only moves messages; with
//! workers off (`?workers=0`, or a page that did not publish the module) the
//! sim runs here, on the page's main thread, once per frame.
use crate::connections::ConnEvent;
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use crossbeam::channel::Receiver;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use crate::GameInstanceManager;
use crate::session::{SessionChannels, SessionEnded};
use crate::web_endpoint::WebServer;

/// The inline (main-thread) backend.
pub struct SimDriver {
    manager: GameInstanceManager,
    /// Every connection's events (the login one and those [`crate::netcode_web::PageOpener`]
    /// opens to other hosts), keyed by connection.
    server_recv: Receiver<(u32, ConnEvent)>,
    client_tick_ms: f64,
    /// The session ended (its home connection was lost): nothing runs any more.
    ended: bool,
}

// SyncCell makes the driver Sync (Bevy Resource requires Send + Sync).
// The !Sync culprit is `Box<dyn Controller>` inside Region: the trait object's
// bounds only include Send, and trait objects don't inherit auto-traits from
// their implementors. SyncCell<T> is Sync for any T: Send, which SimDriver is.
#[derive(Resource)]
pub struct SimDriverRes(pub SyncCell<SimBackend>);

/// Where the session's sim runs.
pub enum SimBackend {
    /// In a Web Worker (the default).
    Worker(Box<crate::web_worker::sim::SimWorker>),
    /// Here, on the main thread (workers off).
    Inline(Box<SimDriver>),
}

/// A session's sim: a fresh game manager on `channels`' sim ends (the app's
/// ends are already installed), connected to `server` over WebTransport.
pub fn start_session(
    server: WebServer,
    channels: SessionChannels,
    debug_flags: game::DebugFlags,
    world_scale: crate::world_scale::WorldScale,
    credential: Option<game::Credential>,
) -> SimDriverRes {
    if crate::web_worker::enabled() {
        match crate::web_worker::sim::SimWorker::start(
            server.clone(),
            channels.clone(),
            debug_flags.clone(),
            world_scale,
            credential.clone(),
        ) {
            Ok(worker) => return SimDriverRes(SyncCell::new(SimBackend::Worker(Box::new(worker)))),
            Err(e) => error!(
                "[session] sim worker failed to start ({e:?}): running the sim on the main thread"
            ),
        }
    }
    let SessionChannels {
        game_send,
        game_recv,
        client_send,
        request_recv,
        ..
    } = channels;
    let (server_send, server_recv) = crossbeam::channel::unbounded();
    // The addr is unused on wasm; GameInstanceManager::new keeps one
    // signature on both targets.
    let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
    let mut manager =
        GameInstanceManager::new(game_send, game_recv, client_send, dummy_addr, debug_flags);
    // Same subscription scale as the native game thread (`?world=` here).
    manager.set_world_scale(world_scale);
    manager.set_request_recv(request_recv);
    // One WebTransport session per host: the login one here, the others opened by the
    // manager through the page (`PageOpener`).
    manager.set_conn_opener(Box::new(crate::netcode_web::PageOpener::new(
        server_send.clone(),
    )));
    crate::netcode_web::connect(
        server,
        crate::connections::LOGIN_CONN,
        server_send,
        manager.client_packet_recv(),
        credential,
    );
    manager.start();
    let driver = SimDriver {
        manager,
        server_recv,
        client_tick_ms: 0.0,
        ended: false,
    };
    SimDriverRes(SyncCell::new(SimBackend::Inline(Box::new(driver))))
}

/// Once per frame while a session runs: run due client ticks, then drain
/// both directions of traffic. Mirrors what the native threads do with
/// sleeps and blocking select. A lost home connection ends the session.
pub fn drive_sim(
    time: Res<Time>,
    driver: Option<ResMut<SimDriverRes>>,
    mut ended: MessageWriter<SessionEnded>,
) {
    let Some(mut driver) = driver else { return };
    let driver = match driver.0.get() {
        SimBackend::Worker(worker) => {
            if let Some(reason) = worker.pump() {
                ended.write(SessionEnded {
                    reason: Some(reason),
                });
            }
            return;
        }
        SimBackend::Inline(driver) => driver,
    };
    let SimDriver {
        manager,
        server_recv,
        client_tick_ms,
        ended: over,
    } = &mut **driver;
    if *over {
        return;
    }

    let dt_ms = time.delta_secs_f64() * 1000.0;
    // Predicted client sim at the adaptive rate (SyncClock adjusts it),
    // like the native tick-generator thread; the server ticks remotely.
    let rate = manager.tick_rate_ms().max(1) as f64;
    *client_tick_ms = (*client_tick_ms + dt_ms).min(10.0 * rate);
    while *client_tick_ms >= rate {
        *client_tick_ms -= rate;
        manager.send_tick();
    }

    match manager.pump(server_recv) {
        Ok(true) => {}
        Ok(false) => info!("sim received Quit"),
        // The home connection ended (its connect gave up, the server went, a refusal).
        Err(game::GameError::Disconnected(reason)) => {
            *over = true;
            ended.write(SessionEnded {
                reason: Some(reason),
            });
        }
        Err(e) => panic!("game sim crashed: {e:?}"),
    }
}
