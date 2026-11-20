//! The sim worker: the browser's game thread. It runs the session's
//! `GameInstanceManager` — prediction, rollback, rapier, and decoding /
//! encoding every packet (a region snapshot is megabytes of lz4 + bincode) —
//! in a dedicated Web Worker, so none of it competes with rendering on the
//! page's main thread.
//!
//! The page keeps the app's ends of the [`SessionChannels`], exactly as on
//! native: [`SimWorker`] forwards what the app sends (`GameEventKind`s,
//! `ClientRequest`s, the collider-wireframe flag) as one [`ToSim`] message per
//! frame, and turns the worker's [`FromSim`] batches back into
//! `ClientUpdateEvent`s on the app's channel. The two channel halves that ride
//! inside `ClientUpdateEvent` (a region's update stream, the UI stream) cannot
//! cross: the worker drains them into messages ([`SimOut::Region`],
//! [`SimOut::Ui`]) and the page makes fresh channels for them.
//!
//! The WebTransport connections stay on the page (`netcode_web::connect_frames`,
//! see its module doc for why) and move raw frames: each frame a server sends
//! is posted to the worker as it arrives, tagged with its connection's key
//! (`web_frame::encode_server_frame`; decoded there), each frame the worker
//! encodes ([`SimOut::Packets`], keyed) is queued for its connection's wire as
//! soon as its batch lands. One session per server host (R3): the login one,
//! and one per [`SimOut::Open`] — the manager's `ConnOpener` here is
//! [`WorkerOpener`], whose opens the page carries out; a connection the
//! manager drops is closed by [`SimOut::Close`], and one that ends on the page
//! is told to the manager by [`ToSim::Closed`].
//!
//! The worker ticks on its own timer ([`DRIVE_INTERVAL_MS`]), like the native
//! tick-generator thread — not on the page's frames, so a slow frame no
//! longer delays the sim.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crossbeam::channel::TryRecvError;
use crossbeam::channel::{Receiver, Sender};
use game::{
    ClientPacket, ClientUpdateEvent, Credential, GameDataUpdate, GameEventKind, HostEndpoint,
    RegionId, ServerPacket, UiUpdate,
};
use js_sys::Uint8Array;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};

use super::{LogLine, MainThreadOnly};
use crate::GameInstanceManager;
use crate::connections::{ConnEvent, ConnHandle, ConnOpener, LOGIN_CONN};
use crate::instance::ClientRequest;
use crate::session::SessionChannels;
use crate::web_endpoint::WebServer;
use crate::web_frame::{TAG_SERVER_FRAME, TAG_TO_SIM, decode_server_frame, server_frame_header};
use crate::world_scale::WorldScale;

/// How often the worker wakes to tick the sim and move traffic (ms). Browsers
/// clamp nested timers to 4 ms anyway.
pub const DRIVE_INTERVAL_MS: i32 = 4;

/// [`WorldScale`] on the wire (its ring table is a `&'static` slice there).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaleWire {
    pub near_radius: i32,
    pub lod_table: Vec<(i32, u32)>,
}

impl From<WorldScale> for ScaleWire {
    fn from(scale: WorldScale) -> Self {
        Self {
            near_radius: scale.near_radius,
            lod_table: scale.lod_table.to_vec(),
        }
    }
}

impl ScaleWire {
    /// Back to a [`WorldScale`]; the table is leaked (once per session, a few
    /// dozen bytes) to get its `'static` slice.
    fn into_scale(self) -> WorldScale {
        WorldScale {
            near_radius: self.near_radius,
            lod_table: Box::leak(self.lod_table.into_boxed_slice()),
        }
    }
}

/// Page → sim worker (besides the raw server frames).
#[derive(Debug, Serialize, Deserialize)]
pub enum ToSim {
    /// Build the session's game manager.
    Start {
        scale: ScaleWire,
        collider_wireframes: bool,
    },
    /// One frame's traffic from the app.
    Batch {
        events: Vec<GameEventKind>,
        requests: Vec<ClientRequest>,
        /// Set when the F3 panel's collider-wireframe toggle changed.
        collider_wireframes: Option<bool>,
    },
    /// The session was left: close the worker.
    Stop,
    /// Connection `conn` ended on the page (its connect gave up, the server went).
    Closed { conn: u32, reason: String },
}

/// One item of a sim worker → page batch.
#[derive(Debug, Serialize, Deserialize)]
pub enum SimOut {
    /// Packet frames for the servers (`game::encode_packet`), each with its
    /// connection's key; in order per connection.
    Packets(Vec<(u32, Vec<u8>)>),
    /// A `ClientUpdateEvent` (its channel halves, if any, are `never()` here).
    Client(ClientUpdateEvent),
    /// Updates for one region's render stream, in order.
    Region(RegionId, Vec<GameDataUpdate>),
    /// UI projection updates, in order.
    Ui(Vec<UiUpdate>),
    /// The sim failed.
    Lost(String),
    /// Log lines emitted in the worker (re-emitted on the page).
    Log(Vec<LogLine>),
    /// Open connection `conn` to the host at `endpoint`, presenting `credential` (an
    /// observer ticket); before any of its [`SimOut::Packets`]. (`Credential`'s `Debug`
    /// redacts its secret.)
    Open {
        conn: u32,
        endpoint: HostEndpoint,
        credential: Credential,
    },
    /// The manager dropped connection `conn` (an idle observer connection, a closed one):
    /// hang it up once its queued frames are written; after its last packets.
    Close { conn: u32 },
}

/// Sim worker → page.
pub type FromSim = Vec<SimOut>;

fn tagged(tag: u8, body: &[u8]) -> Uint8Array {
    let message = Uint8Array::new_with_length(body.len() as u32 + 1);
    message.subarray(0, 1).copy_from(&[tag]);
    message.subarray(1, message.length()).copy_from(body);
    message
}

// ---------------------------------------------------------------- worker side

/// A connection the manager opened: its key, host, Hello credential and outgoing packets.
type Opened = (u32, HostEndpoint, Credential, Receiver<ClientPacket>);

/// The worker's [`ConnOpener`]: the transports live on the page, so an open is a
/// [`SimOut::Open`] in the next batch ([`collect`]).
struct WorkerOpener {
    opened: Sender<Opened>,
}

impl ConnOpener for WorkerOpener {
    fn open(&mut self, key: u32, endpoint: &HostEndpoint, credential: Credential) -> ConnHandle {
        let (send, packets) = crossbeam::channel::unbounded();
        let _ = self
            .opened
            .send((key, endpoint.clone(), credential, packets));
        ConnHandle { send }
    }
}

/// The worker's session.
struct WorkerSession {
    manager: GameInstanceManager,
    /// Decoded server packets and connection ends, into the manager, keyed by connection.
    server_send: Sender<(u32, ConnEvent)>,
    server_recv: Receiver<(u32, ConnEvent)>,
    /// Connections the manager opened since the last batch ([`WorkerOpener`]).
    opened: Receiver<Opened>,
    /// Each live connection's outgoing packets, encoded into [`SimOut::Packets`]; a
    /// disconnected one (the manager dropped it) is a [`SimOut::Close`].
    conns: BTreeMap<u32, Receiver<ClientPacket>>,
    /// The app ends of the session channels (the page forwards into them).
    game_send: Sender<GameEventKind>,
    request_send: Sender<ClientRequest>,
    client_recv: Receiver<ClientUpdateEvent>,
    debug_flags: game::DebugFlags,
    /// Each region's update stream, drained into [`SimOut::Region`].
    regions: BTreeMap<RegionId, Receiver<GameDataUpdate>>,
    ui: Option<Receiver<UiUpdate>>,
    client_tick_ms: f64,
    last_drive_ms: f64,
    /// Set once the sim failed: nothing runs any more.
    ended: bool,
}

thread_local! {
    static SESSION: RefCell<Option<WorkerSession>> = const { RefCell::new(None) };
}

fn scope() -> web_sys::DedicatedWorkerGlobalScope {
    js_sys::global().unchecked_into()
}

/// The worker's half: install the message handler and the drive timer.
pub fn worker_start() {
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|event: web_sys::MessageEvent| {
            let Some(bytes) = super::message_bytes(&event) else {
                return;
            };
            match bytes.split_first() {
                Some((&TAG_TO_SIM, body)) => match bincode::deserialize::<ToSim>(body) {
                    Ok(message) => handle(message),
                    Err(e) => log::error!("[sim-worker] bad message: {e}"),
                },
                Some((&TAG_SERVER_FRAME, _)) => match decode_server_frame(&bytes) {
                    Some((conn, frame)) => match game::decode_packet::<ServerPacket>(frame) {
                        Ok(packet) => SESSION.with(|s| {
                            if let Some(session) = s.borrow_mut().as_mut() {
                                let _ = session.server_send.send((conn, ConnEvent::Packet(packet)));
                            }
                        }),
                        Err(e) => log::warn!("failed deserializing packet {e:?}"),
                    },
                    None => log::warn!(
                        "[sim-worker] a server frame of {} bytes, short of its header",
                        bytes.len()
                    ),
                },
                _ => log::error!("[sim-worker] untagged message"),
            }
        });
    scope().set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let drive_closure = Closure::<dyn FnMut()>::new(drive);
    let _ = scope().set_interval_with_callback_and_timeout_and_arguments_0(
        drive_closure.as_ref().unchecked_ref(),
        DRIVE_INTERVAL_MS,
    );
    drive_closure.forget();
}

fn handle(message: ToSim) {
    match message {
        ToSim::Start {
            scale,
            collider_wireframes,
        } => {
            let channels = SessionChannels::new();
            let SessionChannels {
                game_send,
                game_recv,
                client_send,
                client_recv,
                request_send,
                request_recv,
            } = channels;
            let debug_flags = game::DebugFlags::default();
            debug_flags.set_collider_wireframes(collider_wireframes);
            let (server_send, server_recv) = crossbeam::channel::unbounded();
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0)); // unused on wasm
            let mut manager = GameInstanceManager::new(
                game_send.clone(),
                game_recv,
                client_send,
                addr,
                debug_flags.clone(),
            );
            manager.set_world_scale(scale.into_scale());
            manager.set_request_recv(request_recv);
            let (opened_send, opened) = crossbeam::channel::unbounded();
            manager.set_conn_opener(Box::new(WorkerOpener {
                opened: opened_send,
            }));
            let conns = BTreeMap::from([(LOGIN_CONN, manager.client_packet_recv())]);
            manager.start();
            log::info!("[sim-worker] session started");
            SESSION.with(|s| {
                *s.borrow_mut() = Some(WorkerSession {
                    manager,
                    server_send,
                    server_recv,
                    opened,
                    conns,
                    game_send,
                    request_send,
                    client_recv,
                    debug_flags,
                    regions: BTreeMap::new(),
                    ui: None,
                    client_tick_ms: 0.0,
                    last_drive_ms: js_sys::Date::now(),
                    ended: false,
                })
            });
        }
        ToSim::Batch {
            events,
            requests,
            collider_wireframes,
        } => SESSION.with(|s| {
            let s = s.borrow();
            let Some(session) = s.as_ref() else { return };
            for event in events {
                let _ = session.game_send.send(event);
            }
            for request in requests {
                let _ = session.request_send.send(request);
            }
            if let Some(on) = collider_wireframes {
                session.debug_flags.set_collider_wireframes(on);
            }
        }),
        ToSim::Stop => {
            SESSION.with(|s| s.borrow_mut().take());
            scope().close();
        }
        ToSim::Closed { conn, reason } => SESSION.with(|s| {
            if let Some(session) = s.borrow().as_ref() {
                let _ = session.server_send.send((conn, ConnEvent::Closed(reason)));
            }
        }),
    }
}

/// Timer tick: run due client ticks, pump traffic, ship the outputs.
fn drive() {
    let mut out: FromSim = Vec::new();
    SESSION.with(|s| {
        let mut s = s.borrow_mut();
        let Some(session) = s.as_mut() else { return };
        if session.ended {
            return;
        }
        let now = js_sys::Date::now();
        let dt_ms = (now - session.last_drive_ms).max(0.0);
        session.last_drive_ms = now;
        // Predicted client sim at the adaptive rate (SyncClock adjusts it),
        // like the native tick-generator thread.
        let rate = session.manager.tick_rate_ms().max(1) as f64;
        session.client_tick_ms = (session.client_tick_ms + dt_ms).min(10.0 * rate);
        while session.client_tick_ms >= rate {
            session.client_tick_ms -= rate;
            session.manager.send_tick();
        }
        match session.manager.pump(&session.server_recv) {
            Ok(true) => {}
            Ok(false) => log::info!("sim received Quit"),
            // The home connection ended (its connect gave up, the server went, a refusal).
            Err(game::GameError::Disconnected(reason)) => {
                session.ended = true;
                log::info!("[sim-worker] the session ended: {reason}");
                out.push(SimOut::Lost(reason));
            }
            Err(e) => {
                session.ended = true;
                log::error!("[sim-worker] game sim crashed: {e:?}");
                out.push(SimOut::Lost(format!("The game sim crashed: {e:?}")));
            }
        }
        collect(session, &mut out);
    });
    let lines = super::take_log_lines();
    if !lines.is_empty() {
        out.push(SimOut::Log(lines));
    }
    if !out.is_empty() {
        let _ = super::post_bytes(&scope(), &super::encode(&out));
    }
}

/// Drain the session's outgoing channels into `out`: connections opened first, then
/// packets, then connections closed, then each stream in order, a region's earlier
/// updates before its replacement.
fn collect(session: &mut WorkerSession, out: &mut FromSim) {
    fn flush(id: RegionId, rx: &Receiver<GameDataUpdate>, out: &mut FromSim) {
        let updates: Vec<_> = rx.try_iter().collect();
        if !updates.is_empty() {
            out.push(SimOut::Region(id, updates));
        }
    }
    for (conn, endpoint, credential, packets) in session.opened.try_iter() {
        session.conns.insert(conn, packets);
        out.push(SimOut::Open {
            conn,
            endpoint,
            credential,
        });
    }
    let mut packets = Vec::new();
    let mut closed = Vec::new();
    for (&conn, rx) in &session.conns {
        loop {
            match rx.try_recv() {
                Ok(p) => packets.push((
                    conn,
                    game::encode_packet(&p).expect("client packet encodes"),
                )),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    closed.push(conn);
                    break;
                }
            }
        }
    }
    if !packets.is_empty() {
        out.push(SimOut::Packets(packets));
    }
    for conn in closed {
        session.conns.remove(&conn);
        out.push(SimOut::Close { conn });
    }
    while let Ok(event) = session.client_recv.try_recv() {
        match event {
            ClientUpdateEvent::NewRegion(id, data, rx) => {
                if let Some(old) = session.regions.remove(&id) {
                    flush(id, &old, out);
                }
                session.regions.insert(id, rx);
                out.push(SimOut::Client(ClientUpdateEvent::NewRegion(
                    id,
                    data,
                    crossbeam::channel::never(),
                )));
            }
            ClientUpdateEvent::RemoveRegion(id) => {
                if let Some(old) = session.regions.remove(&id) {
                    flush(id, &old, out);
                }
                out.push(SimOut::Client(ClientUpdateEvent::RemoveRegion(id)));
            }
            ClientUpdateEvent::UiChannel(rx) => session.ui = Some(rx),
            other => out.push(SimOut::Client(other)),
        }
    }
    for (&id, rx) in &session.regions {
        flush(id, rx, out);
    }
    if let Some(ui) = &session.ui {
        let updates: Vec<_> = ui.try_iter().collect();
        if !updates.is_empty() {
            out.push(SimOut::Ui(updates));
        }
    }
}

// ------------------------------------------------------------------ page side

/// What the page's handlers collected since the last frame.
#[derive(Default)]
struct Inbox {
    items: Vec<SimOut>,
    error: Option<String>,
}

/// The page's handle on a session's sim worker. Dropping it ends the session:
/// every connection hangs up (once the frames already queued are written) and
/// the worker closes.
pub struct SimWorker {
    worker: MainThreadOnly<web_sys::Worker>,
    inbox: Arc<Mutex<Inbox>>,
    /// The message handler owns the session's connections (each one's frame queue):
    /// dropping it hangs them all up.
    _handlers: MainThreadOnly<(
        Closure<dyn FnMut(web_sys::MessageEvent)>,
        Closure<dyn FnMut(JsValue)>,
    )>,
    /// The sim ends of the session channels: what the app sends is forwarded.
    game_recv: Receiver<GameEventKind>,
    request_recv: Receiver<ClientRequest>,
    client_send: Sender<ClientUpdateEvent>,
    regions: BTreeMap<RegionId, Sender<GameDataUpdate>>,
    ui_send: Sender<UiUpdate>,
    debug_flags: game::DebugFlags,
    sent_wireframes: bool,
    /// Terminated after a crash; nothing more to send.
    dead: bool,
}

impl SimWorker {
    /// Start a sim worker for a session on `channels` (the app's ends are
    /// already installed) and connect to `server` from the page (the login
    /// connection; the worker asks for the others with [`SimOut::Open`]).
    pub fn start(
        server: WebServer,
        channels: SessionChannels,
        debug_flags: game::DebugFlags,
        world_scale: WorldScale,
        credential: Option<game::Credential>,
    ) -> Result<Self, JsValue> {
        let worker = super::spawn("sim", "orion-sim")?;
        let wireframes = debug_flags.collider_wireframes();
        let start = ToSim::Start {
            scale: world_scale.into(),
            collider_wireframes: wireframes,
        };
        let start = bincode::serialize(&start).expect("start message encodes");
        super::post_bytes(&worker, &tagged(TAG_TO_SIM, &start))?;

        // The session's connections, by key: each one's frame queue. The worker's
        // encoded packets go straight to their connection's queue as their batch
        // lands, and its opens and closes are carried out then too; the rest waits
        // for the frame's `pump`.
        let mut conns: BTreeMap<u32, Sender<Vec<u8>>> = BTreeMap::new();
        open_connection(&worker, &mut conns, LOGIN_CONN, server, credential);
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let on_message = {
            let inbox = inbox.clone();
            let worker = worker.clone();
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                let Some(bytes) = super::message_bytes(&event) else {
                    return;
                };
                let batch: FromSim = match bincode::deserialize(&bytes) {
                    Ok(batch) => batch,
                    Err(e) => {
                        log::error!("[session] bad sim worker message: {e}");
                        return;
                    }
                };
                let mut inbox = inbox.lock().unwrap();
                for item in batch {
                    match item {
                        SimOut::Packets(frames) => {
                            for (conn, frame) in frames {
                                match conns.get(&conn) {
                                    Some(queue) => {
                                        let _ = queue.send(frame);
                                    }
                                    None => log::debug!(
                                        "[session] a frame for connection {conn}, closed: dropped"
                                    ),
                                }
                            }
                        }
                        SimOut::Open {
                            conn,
                            endpoint,
                            credential,
                        } => match crate::web_endpoint::web_server_for_host(&endpoint) {
                            Ok(server) => {
                                log::info!(
                                    "[session] connection {conn}: to host {} ({server:?})",
                                    endpoint.id
                                );
                                open_connection(
                                    &worker,
                                    &mut conns,
                                    conn,
                                    server,
                                    Some(credential),
                                );
                            }
                            Err(reason) => post_to_sim(&worker, &ToSim::Closed { conn, reason }),
                        },
                        // Dropping its queue hangs it up once what is queued is written.
                        SimOut::Close { conn } => {
                            conns.remove(&conn);
                        }
                        other => inbox.items.push(other),
                    }
                }
            })
        };
        let on_error = {
            let inbox = inbox.clone();
            Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
                inbox
                    .lock()
                    .unwrap()
                    .error
                    .get_or_insert_with(|| super::error_text(&event));
            })
        };
        worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let SessionChannels {
            game_recv,
            client_send,
            request_recv,
            ..
        } = channels;
        // The manager announces its UI stream first thing (`GameInstanceManager::new`);
        // here the stream is the page's own channel, fed from `SimOut::Ui`.
        let (ui_send, ui_recv) = crossbeam::channel::unbounded();
        let _ = client_send.send(ClientUpdateEvent::UiChannel(ui_recv));
        log::info!("[session] sim runs in a Web Worker");
        Ok(Self {
            worker: MainThreadOnly(worker),
            inbox,
            _handlers: MainThreadOnly((on_message, on_error)),
            game_recv,
            request_recv,
            client_send,
            regions: BTreeMap::new(),
            ui_send,
            debug_flags,
            sent_wireframes: wireframes,
            dead: false,
        })
    }

    fn post(&self, message: &ToSim) {
        post_to_sim(&self.worker.0, message);
    }

    fn send_inputs(&mut self) {
        if self.dead {
            return;
        }
        let events: Vec<_> = self.game_recv.try_iter().collect();
        let requests: Vec<_> = self.request_recv.try_iter().collect();
        let wireframes = self.debug_flags.collider_wireframes();
        let collider_wireframes = (wireframes != self.sent_wireframes).then_some(wireframes);
        self.sent_wireframes = wireframes;
        if events.is_empty() && requests.is_empty() && collider_wireframes.is_none() {
            return;
        }
        self.post(&ToSim::Batch {
            events,
            requests,
            collider_wireframes,
        });
    }

    /// Once per frame: forward the app's traffic to the worker and the
    /// worker's back to the app. `Some(reason)` = the session is over.
    pub fn pump(&mut self) -> Option<String> {
        self.send_inputs();
        let (items, error) = {
            let mut inbox = self.inbox.lock().unwrap();
            (std::mem::take(&mut inbox.items), inbox.error.take())
        };
        let mut lost = None;
        for item in items {
            match item {
                SimOut::Client(ClientUpdateEvent::NewRegion(id, data, _)) => {
                    let (send, recv) = crossbeam::channel::unbounded();
                    self.regions.insert(id, send);
                    let _ = self
                        .client_send
                        .send(ClientUpdateEvent::NewRegion(id, data, recv));
                }
                SimOut::Client(event) => {
                    if let ClientUpdateEvent::RemoveRegion(id) = &event {
                        self.regions.remove(id);
                    }
                    let _ = self.client_send.send(event);
                }
                SimOut::Region(id, updates) => {
                    if let Some(send) = self.regions.get(&id) {
                        for update in updates {
                            let _ = send.send(update);
                        }
                    }
                }
                SimOut::Ui(updates) => {
                    for update in updates {
                        let _ = self.ui_send.send(update);
                    }
                }
                SimOut::Lost(reason) => lost = lost.or(Some(reason)),
                SimOut::Log(lines) => lines.iter().for_each(LogLine::emit),
                // Carried out on arrival.
                SimOut::Packets(_) | SimOut::Open { .. } | SimOut::Close { .. } => {}
            }
        }
        if let Some(error) = error {
            log::error!("[session] sim worker failed: {error}");
            self.worker.0.terminate();
            self.dead = true;
            lost = lost.or(Some(format!("The game sim crashed: {error}")));
        }
        lost
    }
}

fn post_to_sim(worker: &web_sys::Worker, message: &ToSim) {
    let body = bincode::serialize(message).expect("sim message encodes");
    let _ = super::post_bytes(worker, &tagged(TAG_TO_SIM, &body));
}

/// Open connection `conn` to `server` from the page: its frames go to the worker
/// undecoded, tagged with `conn` (`web_frame`), as they arrive; its end is a
/// [`ToSim::Closed`]; what the worker sends on it is queued in `conns`.
fn open_connection(
    worker: &web_sys::Worker,
    conns: &mut BTreeMap<u32, Sender<Vec<u8>>>,
    conn: u32,
    server: WebServer,
    credential: Option<Credential>,
) {
    let (queue, outgoing) = crossbeam::channel::unbounded::<Vec<u8>>();
    conns.insert(conn, queue);
    let (frame_target, closed_target) = (worker.clone(), worker.clone());
    crate::netcode_web::connect_frames(
        server,
        conn,
        move |frame| {
            let header = server_frame_header(conn);
            let message = Uint8Array::new_with_length((header.len() + frame.len()) as u32);
            message.subarray(0, header.len() as u32).copy_from(&header);
            message
                .subarray(header.len() as u32, message.length())
                .copy_from(&frame);
            let _ = super::post_bytes(&frame_target, &message);
        },
        move |reason| post_to_sim(&closed_target, &ToSim::Closed { conn, reason }),
        outgoing,
        credential,
    );
}

impl Drop for SimWorker {
    fn drop(&mut self) {
        // Before the closures drop: they back the JS functions the worker
        // object still points at. Every connection's frame queue goes with the
        // message handler: once the frames already queued are written, each
        // write loop hangs up and every server sees the session close at once.
        self.worker.0.set_onmessage(None);
        self.worker.0.set_onerror(None);
        if !self.dead {
            self.post(&ToSim::Stop);
        }
    }
}
