//! Framing: `u32` big-endian length + `game::encode_packet(msg)`, over one bidirectional stream.

use quinn::{RecvStream, SendStream};
use serde::{Serialize, de::DeserializeOwned};

/// Largest frame either side accepts or sends.
pub const LINK_MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum FrameError {
    /// Encoding failed or the frame exceeds [`LINK_MAX_FRAME_BYTES`]; nothing was written.
    Unsendable(String),
    /// The stream broke or ended.
    Io(String),
    /// The peer announced or sent a frame we refuse.
    Protocol(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Unsendable(s) | FrameError::Io(s) | FrameError::Protocol(s) => f.write_str(s),
        }
    }
}

/// Encode one message into a checked packet (sync, so the message is never held across an await).
pub(crate) fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let packet = game::encode_packet(msg).map_err(|e| FrameError::Unsendable(format!("encode: {e}")))?;
    if packet.len() > LINK_MAX_FRAME_BYTES {
        return Err(FrameError::Unsendable(format!(
            "frame of {} bytes exceeds the {LINK_MAX_FRAME_BYTES} byte limit",
            packet.len()
        )));
    }
    Ok(packet)
}

pub(crate) async fn write_packet(send: &mut SendStream, packet: &[u8]) -> Result<(), FrameError> {
    send.write_all(&(packet.len() as u32).to_be_bytes()).await.map_err(|e| FrameError::Io(e.to_string()))?;
    send.write_all(packet).await.map_err(|e| FrameError::Io(e.to_string()))
}

pub(crate) async fn write_frame<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<(), FrameError> {
    write_packet(send, &encode(msg)?).await
}

pub(crate) async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream, max: usize) -> Result<T, FrameError> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.map_err(|e| FrameError::Io(e.to_string()))?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(FrameError::Protocol(format!(
            "peer announced a {len} byte frame, over the {max} byte limit"
        )));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(|e| FrameError::Io(e.to_string()))?;
    game::decode_packet(&body).map_err(|e| FrameError::Protocol(format!("frame does not decode: {e}")))
}
