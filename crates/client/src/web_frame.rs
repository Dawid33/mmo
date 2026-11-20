//! The page → sim worker message prefix (`web_worker::sim`): every message starts with a
//! tag byte. A server frame names the connection it came on, since the browser holds one
//! WebTransport session per server host (R3): [`TAG_SERVER_FRAME`], the connection's
//! key (`connections`: one per open, never reused) as a little-endian `u32`, then the
//! frame as `game::encode_packet` made it. Pure bytes, compiled on every target so the
//! codec is the same on both.

/// First byte of a page → worker message: a bincode `ToSim` follows.
pub const TAG_TO_SIM: u8 = 0;
/// First byte of a page → worker message: a connection key and a server frame follow.
pub const TAG_SERVER_FRAME: u8 = 1;
/// The bytes before a server frame's own: the tag and the connection key.
pub const SERVER_FRAME_HEADER_LEN: usize = 1 + 4;

/// The header [`encode_server_frame`] puts before the frame (the page writes it straight
/// into the message it posts, sparing a copy of a megabyte snapshot).
pub fn server_frame_header(conn: u32) -> [u8; SERVER_FRAME_HEADER_LEN] {
    let key = conn.to_le_bytes();
    [TAG_SERVER_FRAME, key[0], key[1], key[2], key[3]]
}

/// A server frame from connection `conn`, as the page posts it to the sim worker.
pub fn encode_server_frame(conn: u32, frame: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SERVER_FRAME_HEADER_LEN + frame.len());
    bytes.extend_from_slice(&server_frame_header(conn));
    bytes.extend_from_slice(frame);
    bytes
}

/// The connection and frame of an [`encode_server_frame`] message; `None` for anything
/// else: another tag, or too short to hold the header.
pub fn decode_server_frame(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let (header, frame) = bytes.split_at_checked(SERVER_FRAME_HEADER_LEN)?;
    let (&tag, key) = header.split_first()?;
    if tag != TAG_SERVER_FRAME {
        return None;
    }
    Some((u32::from_le_bytes(key.try_into().ok()?), frame))
}
