//! WebTransport netcode for the browser: the third implementation of the
//! transport seam (after netcode.rs/quinn and local_server.rs). Dials the
//! server the player joined from the main menu (`web_endpoint.rs`: a real
//! hostname is dialled with its real certificate; a local address fetches the
//! cert hash the dev server publishes and pins it), opens a WebTransport session, and
//! shuttles one bincode packet per uni-stream in each direction — the same
//! wire format quinn speaks natively.
//!
//! Always runs on the page. With the sim in a Web Worker (`web_worker::sim`)
//! the page only moves raw frames ([`connect_frames`]) and the worker decodes
//! and encodes them. A worker-hosted transport was tried (2026-11-02): it
//! connected and joined, but in a debug build under load the server's shared
//! writer later wedged on that client's stream credit — unexplained, so the
//! transport stays where the main-thread build has always had it. It only
//! uses what both globals have (`web_worker::fetch` / `sleep_ms`), so moving
//! it into the worker is a call-site change once that is understood.
use crate::connections::ConnEvent;
use crossbeam::channel::{Receiver, Sender, TryRecvError};
use game::{ClientPacket, ServerPacket};
use js_sys::{Reflect, Uint8Array};
use log::{error, info, warn};
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    ReadableStreamDefaultReader, WebTransport, WebTransportHash, WebTransportOptions,
};

use crate::web_endpoint::WebServer;

/// Report a failed or lost connection to the session flow (`session::web_flow`
/// ends the session with it as the reason). A session already left has
/// dropped the receiver: nothing to tell.
fn connection_lost(lost: &Sender<String>, reason: &str) {
    error!("connection lost: {reason}");
    let _ = lost.send(reason.to_string());
}

/// Kick off the connection to `server`; returns immediately. All I/O runs on
/// the browser event loop via spawn_local, feeding the same channels
/// `LocalServer` feeds the native harnesses. `lost` gets one line when the
/// connect gives up or the connection dies; dropping `client_recv`'s sender
/// (the session's game manager) closes the transport.
pub fn connect(
    server: WebServer,
    server_send: Sender<(u32, ConnEvent)>,
    client_recv: Receiver<ClientPacket>,
    lost: Sender<String>,
    credential: Option<game::Credential>,
) {
    // The one (home) connection: the login one. The browser's connections to
    // other hosts are R3 Task 23's.
    let incoming = Incoming::Packets(server_send, crate::connections::LOGIN_CONN);
    let outgoing = Outgoing::Packets(client_recv);
    spawn_local(async move {
        if let Err(e) = run(server, incoming, outgoing, lost.clone(), credential).await {
            connection_lost(&lost, &js_error_text(&e));
        }
    });
}

/// [`connect`] at the frame level: each server packet's frame (as
/// `game::encode_packet` made it) goes to `on_frame` undecoded, and the
/// frames from `outgoing` are sent as they are. Dropping `outgoing`'s sender
/// closes the transport.
pub fn connect_frames(
    server: WebServer,
    on_frame: impl FnMut(Vec<u8>) + 'static,
    outgoing: Receiver<Vec<u8>>,
    lost: Sender<String>,
    credential: Option<game::Credential>,
) {
    let incoming = Incoming::Frames(Box::new(on_frame));
    spawn_local(async move {
        if let Err(e) = run(server, incoming, Outgoing::Frames(outgoing), lost.clone(), credential).await {
            connection_lost(&lost, &js_error_text(&e));
        }
    });
}

/// Where received packets go.
enum Incoming {
    /// Decoded, keyed by the connection.
    Packets(Sender<(u32, ConnEvent)>, u32),
    Frames(Box<dyn FnMut(Vec<u8>)>),
}

impl Incoming {
    fn deliver(&mut self, bytes: Vec<u8>) {
        match self {
            Incoming::Packets(send, key) => match game::decode_packet::<ServerPacket>(&bytes) {
                Ok(packet) => {
                    let _ = send.send((*key, ConnEvent::Packet(packet)));
                }
                Err(e) => warn!("failed deserializing packet {e:?}"),
            },
            Incoming::Frames(on_frame) => on_frame(bytes),
        }
    }
}

/// Where packets to send come from.
enum Outgoing {
    Packets(Receiver<ClientPacket>),
    Frames(Receiver<Vec<u8>>),
}

impl Outgoing {
    /// The next frame to send; `Err` once the session was left.
    fn next(&self) -> Result<Option<Vec<u8>>, ()> {
        let take = |r: Result<Vec<u8>, TryRecvError>| match r {
            Ok(frame) => Ok(Some(frame)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(()),
        };
        match self {
            Outgoing::Packets(recv) => take(recv.try_recv().map(|p| game::encode_packet(&p).unwrap())),
            Outgoing::Frames(recv) => take(recv.try_recv()),
        }
    }
}

/// A readable line for a JS error value (`Error.message` when it has one).
fn js_error_text(e: &JsValue) -> String {
    Reflect::get(e, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .filter(|m| !m.is_empty())
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}

/// Pause between connect attempts and the total budget before giving up —
/// the same policy as the native client (`CONNECT_RETRY_INTERVAL` /
/// `CONNECT_DEADLINE` in `netcode.rs`, which is not compiled on wasm), so the
/// page can be opened before the server is up.
pub const CONNECT_RETRY_INTERVAL_MS: i32 = 500;
pub const CONNECT_DEADLINE_MS: f64 = 120_000.0;
/// Log every Nth failed attempt (and the first): the browser console is the
/// only log here, and two minutes of 500 ms retries would bury it.
const CONNECT_RETRY_LOG_EVERY: u32 = 10;
/// How often the write loop looks for outgoing packets (ms): an input waits
/// at most this long for the wire.
const WRITE_POLL_MS: i32 = 4;

async fn run(
    server: WebServer,
    mut incoming_to: Incoming,
    outgoing: Outgoing,
    lost: Sender<String>,
    credential: Option<game::Credential>,
) -> Result<(), JsValue> {
    let started = js_sys::Date::now();
    let mut attempt = 1u32;
    let transport = loop {
        let failure = match open_transport(&server).await {
            Ok(transport) => break transport,
            Err(failure) => failure,
        };
        if js_sys::Date::now() - started >= CONNECT_DEADLINE_MS {
            error!("[client] giving up after {attempt} connect attempts");
            return Err(JsValue::from_str(&format!(
                "Could not reach the server after {attempt} attempts ({}).",
                js_error_text(&failure)
            )));
        }
        if attempt == 1 || attempt % CONNECT_RETRY_LOG_EVERY == 0 {
            warn!(
                "[client] webtransport connect attempt {attempt} failed ({failure:?}); \
                 is the server running? retrying"
            );
        }
        attempt += 1;
        sleep_ms(CONNECT_RETRY_INTERVAL_MS).await;
    };
    info!("[client] webtransport connected");
    game::perf_event!(
        "connected",
        "attempts={attempt} connect_ms={:.0}",
        js_sys::Date::now() - started
    );

    // Shared death flag: the read loop is the one that observes the server
    // going away; the write loop checks it so it stops polling instead of
    // spinning against a dead transport every 16ms forever.
    let dead = Rc::new(Cell::new(false));

    // 3. Read loop: each incoming uni-stream is one ServerPacket. One stream
    //    at a time, fully read before the next is accepted: preserves
    //    server-send order exactly like the native quinn loop in netcode.rs
    //    (the rollback reconciler relies on server-open order, not
    //    stream-completion order).
    let incoming: ReadableStreamDefaultReader = transport
        .incoming_unidirectional_streams()
        .get_reader()
        .dyn_into()?;
    let read_dead = dead.clone();
    let read_lost = lost.clone();
    spawn_local(async move {
        loop {
            let next = match JsFuture::from(incoming.read()).await {
                Ok(v) => v,
                Err(e) => {
                    if read_dead.replace(true) {
                        return; // closed by the write loop: the session was left
                    }
                    return connection_lost(&read_lost, &format!("Lost the server: {}", js_error_text(&e)));
                }
            };
            if Reflect::get(&next, &"done".into())
                .map(|d| d.is_truthy())
                .unwrap_or(true)
            {
                if read_dead.replace(true) {
                    return;
                }
                return connection_lost(&read_lost, "The server closed the connection.");
            }
            let stream: web_sys::ReadableStream =
                match Reflect::get(&next, &"value".into()).and_then(|v| v.dyn_into()) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("bad incoming stream: {e:?}");
                        continue;
                    }
                };
            match read_stream_to_end(stream).await {
                Ok(bytes) => incoming_to.deliver(bytes),
                Err(e) => warn!("stream read failed: {e:?}"),
            }
        }
    });

    // 4. Write loop: drain outgoing packets. The channel is filled on this
    //    same thread (the sim, or the sim worker's messages), so poll it with
    //    a task that yields to the event loop between drains.
    // The handshake goes first: the server starts the session once this
    // `Hello` resolves, and refuses a connection that sends anything else.
    // `credential` is a join JWT from the login service; `None` = this
    // browser's local player name (`player_name`).
    let credential = credential.unwrap_or_else(|| game::Credential::Local(crate::player_name::player_name()));
    let mut hello = Some(
        game::encode_packet(&ClientPacket::Hello { credential })
            .map_err(|e| JsValue::from_str(&format!("cannot encode Hello: {e}")))?,
    );
    spawn_local(async move {
        loop {
            if dead.get() {
                return;
            }
            loop {
                let next = match hello.take() {
                    Some(hello) => Ok(Some(hello)),
                    None => outgoing.next(),
                };
                let payload = match next {
                    Ok(Some(payload)) => payload,
                    Ok(None) => break,
                    Err(()) => {
                        // The session was left: hang up, so the server sees
                        // this client go at once.
                        dead.set(true);
                        transport.close();
                        info!("[client] session left: webtransport closed");
                        return;
                    }
                };
                // `create_unidirectional_stream` resolves to a typed
                // `WebTransportSendStream`, which `extends WritableStream` —
                // the dyn_into upcast below always succeeds.
                let stream_fut = transport.create_unidirectional_stream();
                match JsFuture::from(stream_fut).await {
                    Ok(stream) => {
                        let stream: web_sys::WritableStream = match stream.dyn_into() {
                            Ok(s) => s,
                            Err(e) => {
                                warn!("bad outgoing stream: {e:?}");
                                continue;
                            }
                        };
                        let writer = stream.get_writer().expect("stream locked");
                        let chunk = Uint8Array::from(payload.as_slice());
                        if let Err(e) = JsFuture::from(writer.write_with_chunk(&chunk)).await {
                            warn!("write failed: {e:?}");
                        }
                        // Not awaited: `close()` resolves only once the server
                        // has acknowledged the stream's end, and waiting for
                        // that put every packet behind the previous one's
                        // round trip. The stream is already opened and written
                        // in order, which is the order the server reads them in.
                        let closing = writer.close();
                        spawn_local(async move {
                            if let Err(e) = JsFuture::from(closing).await {
                                warn!("closing a stream failed: {e:?}");
                            }
                        });
                    }
                    Err(e) => {
                        // Can't open streams: the session is gone. Fatal, not
                        // retry-every-16ms.
                        if dead.replace(true) {
                            return;
                        }
                        return connection_lost(&lost, &format!("Lost the server: {}", js_error_text(&e)));
                    }
                }
            }
            // Yield so the sim can enqueue more packets: about its drive
            // interval, so an input waits at most that long for the wire.
            sleep_ms(WRITE_POLL_MS).await;
        }
    });

    Ok(())
}

async fn read_stream_to_end(stream: web_sys::ReadableStream) -> Result<Vec<u8>, JsValue> {
    let reader: ReadableStreamDefaultReader = stream.get_reader().dyn_into()?;
    let mut bytes = Vec::new();
    loop {
        let next = JsFuture::from(reader.read()).await?;
        if Reflect::get(&next, &"done".into())?.is_truthy() {
            return Ok(bytes);
        }
        let chunk: Uint8Array = Reflect::get(&next, &"value".into())?.dyn_into()?;
        bytes.extend(chunk.to_vec());
    }
}

/// One connect attempt to `server` (`web_endpoint`).
async fn open_transport(server: &WebServer) -> Result<WebTransport, JsValue> {
    match server {
        WebServer::Trusted { url } => {
            let transport = WebTransport::new(url)?;
            JsFuture::from(transport.ready()).await?;
            Ok(transport)
        }
        WebServer::PinnedLocal => open_pinned_local().await,
        WebServer::Pinned { url, cert_hash } => open_pinned(url, cert_hash).await,
    }
}

/// Dev: fetch the cert hash the local server published, then open a session
/// trusting exactly that certificate.
async fn open_pinned_local() -> Result<WebTransport, JsValue> {
    // 1. Fetch the cert hash + port the server wrote into assets/. Malformed
    //    contents are Err returns, not panics: a panic inside this promise
    //    would poison the whole wasm instance instead of just flagging the
    //    connection as lost. Re-fetched on every attempt, past the HTTP cache:
    //    the server mints a new certificate (and rewrites this file) each
    //    start, so a hash read before it came up is stale.
    // Resolved against the page: in the sim worker a bare relative URL would
    // resolve against the worker's Blob URL.
    let hash_url = crate::web_worker::page_url(&format!(
        "assets/webtransport-cert-hash.json?t={}",
        js_sys::Date::now()
    ));
    let resp: web_sys::Response = JsFuture::from(crate::web_worker::fetch(&hash_url))
        .await?
        .dyn_into()?;
    let text = JsFuture::from(resp.text()?).await?;
    let text = text
        .as_string()
        .ok_or_else(|| JsValue::from_str("hash file not text"))?;
    let (hash_hex, port) =
        parse_hash_json(&text).ok_or_else(|| JsValue::from_str("malformed cert hash file"))?;
    open_pinned(&format!("https://127.0.0.1:{port}/"), &hash_hex).await
}

/// Open a session to `url` trusting exactly the certificate whose SHA-256 is
/// `hash_hex` (the dev server's hash file, or a login ticket's `cert_hash`).
async fn open_pinned(url: &str, hash_hex: &str) -> Result<WebTransport, JsValue> {
    // Guard the slicing too: odd-length or non-ASCII hex would panic on
    // `&hash_hex[i..i + 2]` before from_str_radix ever saw it.
    if hash_hex.len() % 2 != 0 || !hash_hex.is_ascii() {
        return Err(JsValue::from_str("bad hex in cert hash"));
    }
    let hash_bytes: Vec<u8> = (0..hash_hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hash_hex[i..i + 2], 16)
                .map_err(|_| JsValue::from_str("bad hex in cert hash"))
        })
        .collect::<Result<_, _>>()?;

    // 2. Open the transport, trusting exactly that certificate.
    let hash = WebTransportHash::new();
    hash.set_algorithm("sha-256");
    // `set_value` wants a bare `Object`; the `_u8_array` setter is the safe,
    // copying variant that takes a `Uint8Array` directly (web-sys 0.3.103).
    hash.set_value_u8_array(&Uint8Array::from(hash_bytes.as_slice()));
    let options = WebTransportOptions::new();
    // Takes a slice of hashes directly, not a JS Array wrapper.
    options.set_server_certificate_hashes(&[hash]);
    let transport = WebTransport::new_with_options(url, &options)?;
    JsFuture::from(transport.ready()).await?;
    Ok(transport)
}

fn parse_hash_json(text: &str) -> Option<(String, u16)> {
    // {"sha256_hex":"…","port":6467} — hand-rolled to avoid a serde_json
    // dependency in the client for one file.
    let hex = text.split("\"sha256_hex\":\"").nth(1)?.split('"').next()?;
    let port = text
        .split("\"port\":")
        .nth(1)?
        .trim_end_matches(['}', ' ', '\n'])
        .parse()
        .ok()?;
    Some((hex.to_string(), port))
}

use crate::web_worker::sleep_ms;
