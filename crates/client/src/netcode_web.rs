//! WebTransport netcode for the browser: the third implementation of the
//! transport seam (after netcode.rs/quinn and local_server.rs). Fetches the
//! dev cert hash the server publishes, opens a WebTransport session, and
//! shuttles one bincode packet per uni-stream in each direction — the same
//! wire format quinn speaks natively.
use crossbeam::channel::{Receiver, Sender};
use game::{ClientPacket, ServerPacket};
use js_sys::{Reflect, Uint8Array};
use log::{error, info, warn};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    ReadableStreamDefaultReader, WebTransport, WebTransportHash, WebTransportOptions,
};

/// Kick off the connection; returns immediately. All I/O runs on the browser
/// event loop via spawn_local, feeding the same channels LocalServer feeds
/// in offline mode.
pub fn connect(server_send: Sender<ServerPacket>, client_recv: Receiver<ClientPacket>) {
    spawn_local(async move {
        if let Err(e) = run(server_send, client_recv).await {
            error!("webtransport connection failed: {e:?}");
            // Spec: dev-grade on-screen signal, not just the console.
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                doc.set_title("CONNECTION LOST — Labour of Love");
            }
        }
    });
}

async fn run(
    server_send: Sender<ServerPacket>,
    client_recv: Receiver<ClientPacket>,
) -> Result<(), JsValue> {
    let window = web_sys::window().expect("no window");

    // 1. Fetch the cert hash + port the server wrote into assets/.
    let resp: web_sys::Response =
        JsFuture::from(window.fetch_with_str("assets/webtransport-cert-hash.json"))
            .await?
            .dyn_into()?;
    let text = JsFuture::from(resp.text()?).await?;
    let text = text.as_string().expect("hash file not text");
    let (hash_hex, port) = parse_hash_json(&text).expect("malformed cert hash file");
    let hash_bytes: Vec<u8> = (0..hash_hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hash_hex[i..i + 2], 16).unwrap())
        .collect();

    // 2. Open the transport, trusting exactly that certificate.
    let hash = WebTransportHash::new();
    hash.set_algorithm("sha-256");
    // `set_value` wants a bare `Object`; the `_u8_array` setter is the safe,
    // copying variant that takes a `Uint8Array` directly (web-sys 0.3.103).
    hash.set_value_u8_array(&Uint8Array::from(hash_bytes.as_slice()));
    let options = WebTransportOptions::new();
    // Takes a slice of hashes directly, not a JS Array wrapper.
    options.set_server_certificate_hashes(&[hash]);
    let transport = WebTransport::new_with_options(&format!("https://127.0.0.1:{port}/"), &options)?;
    JsFuture::from(transport.ready()).await?;
    info!("[client] webtransport connected");

    // 3. Read loop: each incoming uni-stream is one ServerPacket.
    let incoming: ReadableStreamDefaultReader = transport
        .incoming_unidirectional_streams()
        .get_reader()
        .dyn_into()?;
    let read_send = server_send.clone();
    spawn_local(async move {
        loop {
            let next = match JsFuture::from(incoming.read()).await {
                Ok(v) => v,
                Err(e) => return warn!("incoming streams closed: {e:?}"),
            };
            if Reflect::get(&next, &"done".into())
                .map(|d| d.is_truthy())
                .unwrap_or(true)
            {
                return info!("server closed the connection");
            }
            let stream: web_sys::ReadableStream =
                match Reflect::get(&next, &"value".into()).and_then(|v| v.dyn_into()) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("bad incoming stream: {e:?}");
                        continue;
                    }
                };
            let send = read_send.clone();
            spawn_local(async move {
                match read_stream_to_end(stream).await {
                    Ok(bytes) => match bincode::deserialize::<ServerPacket>(&bytes) {
                        Ok(packet) => {
                            let _ = send.send(packet);
                        }
                        Err(e) => warn!("failed deserializing packet {e:?}"),
                    },
                    Err(e) => warn!("stream read failed: {e:?}"),
                }
            });
        }
    });

    // 4. Write loop: drain outgoing ClientPackets. The channel is filled by
    //    the sim on the same (only) thread, so poll it with a task that
    //    yields to the event loop between drains.
    spawn_local(async move {
        loop {
            while let Ok(packet) = client_recv.try_recv() {
                let payload = bincode::serialize(&packet).unwrap();
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
                        let _ = JsFuture::from(writer.close()).await;
                    }
                    Err(e) => warn!("open uni failed: {e:?}"),
                }
            }
            // Yield ~one frame so the sim can enqueue more packets.
            sleep_ms(16).await;
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

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .unwrap();
    });
    let _ = JsFuture::from(promise).await;
}
