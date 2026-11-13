//! The page → sim worker server-frame codec (`client::web_frame`) on wasm32, where the
//! browser runs it (node via wasm-bindgen-test-runner: `just client-wasm`). The same
//! cases run natively as `web_frame::tests`. Empty on native targets.
#![cfg(target_arch = "wasm32")]

use client::web_frame::{decode_server_frame, encode_server_frame, SERVER_FRAME_HEADER_LEN, TAG_SERVER_FRAME, TAG_TO_SIM};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn server_frame_round_trips_with_its_connection() {
    let frame = game::encode_packet(&game::ServerPacket::HomeHop(7)).unwrap();
    for conn in [0, 1, 2, 0x0102_0304, u32::MAX] {
        let bytes = encode_server_frame(conn, &frame);
        assert_eq!(bytes[0], TAG_SERVER_FRAME);
        assert_eq!(&bytes[1..5], &conn.to_le_bytes(), "the key is little-endian");
        let (got, body) = decode_server_frame(&bytes).expect("decodes");
        assert_eq!((got, body), (conn, frame.as_slice()));
        assert!(matches!(game::decode_packet(body), Ok(game::ServerPacket::HomeHop(7))));
    }
}

#[wasm_bindgen_test]
fn short_frame_is_rejected() {
    for len in 0..SERVER_FRAME_HEADER_LEN {
        let bytes = &encode_server_frame(3, &[])[..len];
        assert_eq!(decode_server_frame(bytes), None, "{len} bytes");
    }
    let mut other = encode_server_frame(3, b"frame");
    other[0] = TAG_TO_SIM;
    assert_eq!(decode_server_frame(&other), None, "another tag");
}
