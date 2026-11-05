//! Player identity. A `ClientId` is one connection (a per-process counter the
//! server restarts at 0); a [`PlayerId`] is the person behind it, stable across
//! reconnects and server restarts. The client proves who it is with a
//! [`PlayerToken`] — a bearer secret it keeps on disk (native) or in
//! `localStorage` (browser) — sent as the first packet of every connection
//! (`ClientPacket::Hello`). The server resolves it at its network edge through
//! a `PlayerStore` (`crates/server/src/player_store.rs`), so the `WorldManager`
//! only ever sees the resolved id and the [`PlayerRecord`] saved for it.
//!
//! No authentication yet: whoever holds the token is the player.

use serde::{Deserialize, Serialize};

use crate::history::SiteId;
use crate::{Client, Pose, RegionCoords, StateHash};

/// The stable identity of a player (never reused for another token).
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize, StateHash)]
pub struct PlayerId(pub u64);

/// Random bytes in a token: 128 bits.
pub const PLAYER_TOKEN_BYTES: usize = 16;
/// Characters of a token's lowercase-hex form.
pub const PLAYER_TOKEN_LEN: usize = PLAYER_TOKEN_BYTES * 2;
/// How many leading characters `Debug` shows (the rest is the secret).
pub const PLAYER_TOKEN_DEBUG_CHARS: usize = 6;

/// The secret a client holds to be the same player again: 32 lowercase hex
/// characters. `Debug` prints only its first [`PLAYER_TOKEN_DEBUG_CHARS`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PlayerToken(pub String);

impl PlayerToken {
    /// The token of 16 random bytes (the caller supplies the randomness: the
    /// sim crate has no OS RNG).
    pub fn generate(rng_bytes: [u8; PLAYER_TOKEN_BYTES]) -> Self {
        Self(rng_bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// Exactly [`PLAYER_TOKEN_LEN`] lowercase hex characters.
    pub fn is_valid(&self) -> bool {
        self.0.len() == PLAYER_TOKEN_LEN && self.0.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

impl std::fmt::Debug for PlayerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown: String = self.0.chars().take(PLAYER_TOKEN_DEBUG_CHARS).collect();
        write!(f, "PlayerToken({shown}…)")
    }
}

/// What the server keeps of a player who is not online: enough to put the
/// body back where it stood, with its state and claims.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, StateHash)]
pub struct PlayerRecord {
    /// The player's sim state (inventory, mode, …); its input is reset.
    pub client: Client,
    /// The body's pose, region-local to `region`.
    pub pose: Pose,
    /// The region the body stood in.
    pub region: RegionCoords,
    /// The fortress the player held.
    pub fortress: Option<SiteId>,
    /// The site the player's adventure started at.
    pub adventure: Option<SiteId>,
}
