//! Game client
#![deny(missing_docs)]
use crossbeam::channel::{Receiver, Sender};
use game::{GameError, GameEvent, GameEventKind, GameInstance, GameSnapshotUpdate};
use log::{info, trace, warn};
use quinn::rustls::{
    self,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use simple_log::LogConfigBuilder;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use window::{GameEventSource, Window};

mod netcode;
mod window;

/// Wrapper struct for coordinating networking / rollback for the game.
pub struct GameInstanceManager {
    world: GameInstance,
    game_event_send: Sender<GameEventSource>,
    game_event_recv: Receiver<GameEventSource>,
    client_event_send: Sender<ClientEvent>,
    server: SocketAddr,
}

impl GameInstanceManager {
    /// Create new GameInstanceManager
    ///
    /// - Talk to ingress to figure out which regions to load for a given player.
    /// - Start listening game events and log them.
    /// - Download game data
    /// - Re-simulate game based on logged events.
    /// - Send spawn player event and enter main loop
    pub fn new(
        game_event_send: Sender<GameEventSource>,
        game_event_recv: Receiver<GameEventSource>,
        client_event_send: Sender<ClientEvent>,
        server: SocketAddr,
    ) -> Self {
        Self {
            world: GameInstance::new(game::WorldId::StarterArea, 0),
            game_event_recv,
            game_event_send,
            client_event_send,
            server,
        }
    }

    /// Recieve and process game events from the client and network, in that order.
    ///
    /// - If received a client event:
    ///   - Simulate the event and push it on to a buffer.
    ///   - Send it to the players authoritative region.
    ///   - The client buffer should always be larger than the
    ///     network buffer by at least the number of ticks the client is ahead of
    ///     the server.
    /// - If recieved a network event:
    ///   - Check if the network and client event on the front of each buffer match.
    ///   - Keep doing this until the network buffer is empty. Reconcile the region
    ///     each time the comparison succeeds.
    ///   - Any time it doesn't succeed, rollback the client, apply the network
    ///     event and then re-apply the client events that were just rolled back.
    ///
    /// If sync clock event: send client update to window with desired input delay
    /// and update tick time.
    pub fn connect_and_run(&mut self) -> Result<(), GameError> {
        let mut tick = 0;
        let tick_sender = self.game_event_send.clone();
        // Generate ticks
        std::thread::spawn(move || loop {
            // TODO: Sync ticks with server.
            tick_sender
                .send(GameEventSource::Client(GameEvent::new(
                    GameEventKind::Tick(tick),
                )))
                .unwrap();
            tick += 1;
            std::thread::sleep(Duration::from_millis(1000));
        });

        let mut conn = netcode::ServerConnection::new(
            self.game_event_send.clone(),
            self.game_event_recv.clone(),
            self.server,
        );
        std::thread::spawn(move || loop {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async { conn.connect_and_handle().await.unwrap() });
        });

        Ok(while let Ok(source) = self.game_event_recv.recv() {
            match source {
                GameEventSource::Client(game_event) => match game_event.kind {
                    GameEventKind::Quit => return Ok(()),
                    _ => self.world.handle_event(game_event)?,
                },
                GameEventSource::Network(packet) => {
                    info!("[client] RECIEVE SUCCESS");
                }
            }
        })
    }
}

#[derive(Debug)]
enum Origin {
    Client,
    Server,
}

/// Event sent from game to client
#[derive(Debug)]
pub enum ClientEvent {
    /// Update the game client with new sim info.
    Update(GameSnapshotUpdate),
    /// Notify client of game crashing.
    GameCrash(Origin, GameError),
}

/// Event sent from client to game thread.
pub enum Command {
    /// Connect to a server, sync and start running game sim.
    LaunchGame(
        Sender<GameEventSource>,
        Receiver<GameEventSource>,
        Sender<ClientEvent>,
        SocketAddr,
    ),
    /// Quit the game thread. Should only be send when quitting the application.
    Quit,
}

fn main() {
    let config = LogConfigBuilder::builder()
        .size(1 * 100)
        .roll_count(10)
        .time_format("%M:%S")
        .level("debug")
        .unwrap()
        .output_console()
        .build();
    simple_log::new(config).unwrap();

    let (command_send, command_recv) = crossbeam::channel::unbounded();
    std::thread::spawn(move || loop {
        match command_recv.recv() {
            Ok(command) => match command {
                Command::LaunchGame(sender, receiver, client_sender, server) => {
                    let mut manager =
                        GameInstanceManager::new(sender, receiver, client_sender, server);
                    loop {
                        if let Err(e) = manager.connect_and_run() {
                            warn!("Game Crashed: {:?}", e);
                        };
                    }
                }
                Command::Quit => {
                    trace!("Game thread recieved quit command.");
                    break;
                }
            },
            Err(_e) => {
                warn!(
                    "Game thread stoped receiving command events, stopping game thread. Client probably crashed or was closed incorrectly."
                );
                break;
            }
        }
    });

    let mut window = Window::new(command_send);
    window.run();
}
/// Dummy certificate verifier that treats any certificate as valid.
/// NOTE, such verification is vulnerable to MITM attacks, but convenient for testing.
#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
