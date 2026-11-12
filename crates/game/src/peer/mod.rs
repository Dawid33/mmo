//! Host ↔ host: the messages two region hosts of one world exchange ([`PeerMsg`]), the
//! per-peer session that makes some of them reliable — numbered, acknowledged, resent
//! after a reconnect — and lets the others drop ([`PeerSession`]), and an in-process
//! link with a seeded delay for tests ([`InlinePeerLink`]). IO-free like the coordinator:
//! a real link (QUIC) implements [`PeerLink`] at the server's networking edge, and time
//! arrives as `now_ms`.

mod link;
mod msg;
mod session;

pub use link::*;
pub use msg::*;
pub use session::*;
