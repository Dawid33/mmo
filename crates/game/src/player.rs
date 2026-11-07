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
use rapier3d::math::Vector;

use crate::{Client, ClientId, Pose, RegionCoords, Rollback, StateHash, TileSource};

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

/// `client` as a parked record keeps it: what belongs to the connection —
/// held input, the first-person flag, an open inventory — is reset, the
/// cursor stack goes back into the inventory (what does not fit stays on the
/// cursor), an open chest closes, and what belongs to the region it stood
/// in (mining, the break cooldown: [`Client::arrive`]) is cleared, so a
/// restored player starts like a fresh session in everything but its state.
pub fn parked_client(client: &Client) -> Client {
    let mut c = client.clone();
    c.input = Default::default();
    c.fps_cam_mode = false;
    c.inventory_open = false;
    c.open_container = None;
    if let Some(rest) = c.stow_cursor() {
        c.cursor = Some(rest);
    }
    c.arrive()
}

/// Every player body in `rollback` as a record (claims empty — the
/// `WorldManager` holds those), keyed by session. The world autosave extracts
/// online players from each region's `Saved` state with this.
pub fn players_in(rollback: &Rollback, region: RegionCoords) -> Vec<(ClientId, PlayerRecord)> {
    rollback
        .player_entites
        .keys()
        .filter_map(|&c| Some((c, rollback.player_record(c, region)?)))
        .collect()
}

impl Rollback {
    /// `client`'s record as it stands in this region (`region`), if it has a
    /// body here. Claims are empty.
    pub fn player_record(&self, client: ClientId, region: RegionCoords) -> Option<PlayerRecord> {
        let key = *self.data.player_entites.get(&client)?;
        let handle = (*self.data.ecs.rigidbody.try_get(key))?;
        let pose = *self.data.physics.bodies.get(handle)?.position();
        let c = self.data.clients.get(&client)?;
        Some(PlayerRecord { client: parked_client(c), pose, region, fortress: None, adventure: None })
    }

    /// Puts a returning player back, undo-safely: its `Client` (session state
    /// reset) and a body at the record's position — or, when that is now inside
    /// terrain (someone built there), on the nearest standable tile of its
    /// column ([`restore_translation`]). A client that already has a body here
    /// is left alone.
    pub fn restore_player_safe(&mut self, client: ClientId, record: &PlayerRecord) {
        if self.data.player_entites.contains_key(&client) {
            log::warn!("RestorePlayer({client}): already has a body here");
            return;
        }
        let at = restore_translation(self, record.pose.translation);
        self.data.clients.insert(client, parked_client(&record.client));
        self.create_player_body_safe(client, at);
    }
}

/// Where a restored body goes: `t` (a body centre, region-local voxel units)
/// when its feet tile and the tile above are clear of solid blocks and inside
/// the region; else onto the standable tile nearest its feet level in its
/// column (clamped into the region); else over the region's spawn column.
/// Never inside terrain.
pub fn restore_translation(rollback: &Rollback, t: Vector) -> Vector {
    let feet = crate::feet_tile(crate::feet_of(t));
    let n = crate::REGION_BLOCKS as i32;
    let inside = (0..n).contains(&feet[0]) && (0..n).contains(&feet[2]);
    let clear = |p: crate::TilePos| {
        let b = rollback.block(p);
        b == crate::BlockId::AIR || crate::block_props().is_liquid(b) || !crate::block_props().is_solid(b)
    };
    if inside && clear(feet) && clear([feet[0], feet[1] + 1, feet[2]]) {
        return t;
    }
    let column = [feet[0].clamp(0, n - 1), feet[2].clamp(0, n - 1)];
    match crate::nearest_standable(rollback, column, Some(feet[1])) {
        Some(tile) => crate::body_on_tile(tile),
        None => {
            let xz = rollback.spawn_xz();
            Vector::new(xz.0, rollback.player_spawn_y(xz), xz.1)
        }
    }
}
