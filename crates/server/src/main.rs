//! Server
// #![deny(missing_docs)]
use std::{fmt::Debug, sync::Arc, thread, time::Duration};

use crossbeam::{
    channel::{Receiver, Sender},
    epoch::Pointable,
};
use dashmap::{mapref::entry, DashMap, Entry};
use game::{ClientPacket, GameEvent, GameEventKind, ServerPacket, World};
use log::{debug, error, info, LevelFilter};
use quinn::{
    crypto::rustls::QuicServerConfig,
    rustls::{
        self,
        pki_types::{CertificateDer, PrivatePkcs8KeyDer},
        ConfigBuilder,
    },
    ClientConfig, ConnectError, Connecting, Connection, ConnectionError, Endpoint,
};
use simplelog::{
    format_description, ColorChoice, Config, FormatItem, SimpleLogger, TermLogger, TerminalMode,
};

/// Wrapper around RegionGroup with additional bookeeping / networking
/// to make it work as a server. Acts only as a dumb router of game event packets.
///
/// ## Initial Setup
///
/// - Load world from file or generate from scene.
/// - Enter main loop
///
/// ## Game Loop
/// - Order incoming client packets by game tick.
/// - Check if client packets are for the current game tick and execute them
///   if so.
/// - Execute game tick.
/// - If the loop didn't take a full TICK_TIME, wait until full TICK_TIME has passed.
pub struct WorldIngress {}

impl WorldIngress {
    pub fn new() -> Self {
        Self {}
    }

    /// ## Network Loop
    /// - Process incoming client packets and send them to the game loop
    /// - Receive server packets from game loop and send them out to connected
    ///   clients. Maintain a buffer of events for each client and drop the client
    ///   if the buffer grows too large.
    /// - Recieve region packets from game loop and send them to their assorted
    ///   regions.
    pub async fn listen(&mut self, send: Sender<ServerEvent>, server_recv: Receiver<ServerPacket>) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_der = CertificateDer::from(cert.cert);
        let priv_key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], priv_key.into())
            .unwrap();

        let endpoint = Endpoint::server(
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(crypto).unwrap())),
            "127.0.0.1:6466".parse().unwrap(),
        )
        .unwrap();

        let connections: Arc<DashMap<usize, Connection>> = Arc::new(DashMap::new());
        let conns = connections.clone();
        tokio::spawn(async move {
            while let Ok(event) = server_recv.recv() {
                let packet = bincode::serialize(&event).unwrap();
                for entry in conns.iter() {
                    let mut stream = entry.value().open_uni().await.unwrap();
                    stream.write_all(&packet).await.unwrap();
                    stream.finish().unwrap();
                    stream.stopped().await.unwrap();
                }
            }
        });

        while let Some(conn) = endpoint.accept().await {
            let send = send.clone();
            let conns = connections.clone();
            tokio::spawn(async move {
                info!("accepting connection");
                let connection = conn.await.unwrap();
                conns.insert(connection.stable_id(), connection.clone());

                let fut = handle_connection(connection, send);
                tokio::spawn(async move {
                    if let Err(e) = fut.await {
                        error!("connection failed: {reason}", reason = e.to_string())
                    }
                });
            });
        }
    }
}

#[derive(Debug)]
/// Internal event that is handled by the server.
pub enum ServerEvent {
    /// Recieved a packet from a client that needs to be handled.
    ClientPacket(ClientPacket),
    /// Internal timer for generated game ticks.
    ServerTickTimer,
}

const FORMAT: &'static [FormatItem] = &[FormatItem::Literal("server".as_bytes())];

fn main() {
    let config = simplelog::ConfigBuilder::new()
        .set_time_format_custom(FORMAT)
        .build();
    SimpleLogger::init(LevelFilter::Info, config).unwrap();

    let (client_packet_send, client_packet_recv) = crossbeam::channel::unbounded();
    let (server_send, server_recv) = crossbeam::channel::unbounded();

    let mut rgi = WorldIngress::new();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let cps = client_packet_send.clone();
    std::thread::spawn(move || rt.block_on(async move { rgi.listen(cps, server_recv).await }));

    // Handle game loop
    std::thread::spawn(move || loop {
        // TODO: Sync ticks with server.
        client_packet_send
            .send(ServerEvent::ServerTickTimer)
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
    });

    let mut world = World::editor();
    // TODO: send region once to all new connections.
    // TODO: send region on request to a connection.

    // handle game events on server and send successfull events to connected clients.
    while let Ok(event) = client_packet_recv.recv() {
        match event {
            ServerEvent::ClientPacket(client_packet) => match client_packet {
                ClientPacket::RequestRegion => {
                    server_send
                        .send(world.build_region_server_packet(0))
                        .unwrap();
                }
                ClientPacket::SyncClock => todo!(),
                ClientPacket::GameEvent(game_event) => match game_event.kind {
                    game::GameEventKind::Quit | game::GameEventKind::Tick => (),
                    _ => {
                        let event = match world.handle_event(game_event.kind, game_event._region_id)
                        {
                            Ok(e) => e,
                            Err(e) => panic!("Server crashed {:?}", e),
                        };
                        server_send.send(ServerPacket::GameEvent(event)).unwrap();
                    }
                },
            },
            ServerEvent::ServerTickTimer => {
                let event = match world.handle_event(GameEventKind::Tick, 0) {
                    Ok(e) => e,
                    Err(e) => panic!("Server crashed {:?}", e),
                };
                server_send.send(ServerPacket::GameEvent(event)).unwrap();
            }
        }
    }
}

async fn handle_connection(
    connection: quinn::Connection,
    send: Sender<ServerEvent>,
) -> Result<(), ConnectionError> {
    loop {
        let send = send.clone();
        let stream = connection.accept_uni().await;

        let mut stream = match stream {
            Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
                info!("connection closed");
                return Ok(());
            }
            Err(e) => {
                return Err(e);
            }
            Ok(s) => s,
        };

        tokio::spawn(async move {
            let req = stream.read_to_end(usize::MAX).await.unwrap();
            let packet = bincode::deserialize_from(&req[..]);
            let packet: ClientPacket = match packet {
                Ok(e) => e,
                Err(e) => {
                    info!("Failed deserializing packet {:?}", e);
                    return ();
                }
            };
            send.send(ServerEvent::ClientPacket(packet)).unwrap();
        });
    }
}
