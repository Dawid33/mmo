//! Player identity. A `ClientId` is one session, 64 bits on every target: for a
//! login session [`session_client_id`] of its player and ticket epoch (the same on every
//! host of a world), for a local session a per-process counter from 0; a [`PlayerId`] is the person behind it, stable across
//! reconnects and server restarts. The client says who it is with a
//! [`Credential`], sent as the first packet of every connection
//! (`ClientPacket::Hello`):
//! - on a local server (single player, LAN, dev — Factorio's model) a freely
//!   chosen [`PlayerName`]: no secret, whoever sends a name is that player;
//! - on an online server a join JWT from the login service, whose ticket
//!   carries the account's [`PlayerToken`].
//!
//! The server resolves it at its network edge through a `PlayerStore`
//! (`crates/server/src/player_store.rs`), so the `WorldManager` only ever sees
//! the resolved id and the [`PlayerRecord`] saved for it.

use serde::{Deserialize, Serialize};

use crate::history::SiteId;
use rapier3d::math::Vector;

use crate::{Client, ClientId, Pose, RegionCoords, Rollback, StateHash};

/// The stable identity of a player (never reused for another name or token).
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize, StateHash)]
pub struct PlayerId(pub u64);

/// Low bits of a [`ClientId`] that hold the login ticket's `session_epoch`.
pub const SESSION_EPOCH_BITS: u32 = 24;

/// The world-unique session id of `player`'s login session `session_epoch`: the player id
/// above [`SESSION_EPOCH_BITS`] bits of the epoch (only its low bits: the epoch wraps after 16 million logins of one player). The same on every host of a world.
pub fn session_client_id(player: PlayerId, session_epoch: u64) -> ClientId {
    debug_assert!(player.0 < 1 << (64 - SESSION_EPOCH_BITS), "player id {} does not fit a session id", player.0);
    (player.0 << SESSION_EPOCH_BITS) | (session_epoch & ((1 << SESSION_EPOCH_BITS) - 1))
}

/// Random bytes in a token: 128 bits.
pub const PLAYER_TOKEN_BYTES: usize = 16;
/// Characters of a token's lowercase-hex form.
pub const PLAYER_TOKEN_LEN: usize = PLAYER_TOKEN_BYTES * 2;
/// How many leading characters `Debug` shows (the rest is the secret).
pub const PLAYER_TOKEN_DEBUG_CHARS: usize = 6;

/// An online account's player key, minted by the login service and carried in
/// its join tickets: 32 lowercase hex characters. `Debug` prints only its
/// first [`PLAYER_TOKEN_DEBUG_CHARS`].
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

/// Longest player name, in characters.
pub const PLAYER_NAME_MAX_CHARS: usize = 32;

/// The name a player goes by on a local (single-player / LAN / dev) server:
/// trimmed, 1..=[`PLAYER_NAME_MAX_CHARS`] characters, no control characters;
/// otherwise free (any script). Not a secret. Build it with
/// [`PlayerName::parse`]; a server re-validates what it receives.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PlayerName(pub String);

impl PlayerName {
    /// `raw` trimmed, or why it is not a name.
    pub fn parse(raw: &str) -> Result<PlayerName, &'static str> {
        let name = raw.trim();
        if name.is_empty() {
            return Err("the player name is empty");
        }
        if name.chars().count() > PLAYER_NAME_MAX_CHARS {
            return Err("the player name is longer than 32 characters");
        }
        if name.chars().any(char::is_control) {
            return Err("the player name contains a control character");
        }
        Ok(PlayerName(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PlayerName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Longest login JWT a `Hello` may carry, in bytes.
pub const MAX_JWT_BYTES: usize = 2048;

/// What a connection presents in its `Hello`: a player name (local servers —
/// single player, LAN, dev) or a join JWT signed by the login service
/// (online servers).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Credential {
    Local(PlayerName),
    Jwt(String),
    /// A session to a host that does not hold the client's body (an observer): the
    /// [`crate::ObserverTicket`] its home host handed out (`ServerPacket::RegionElsewhere`).
    /// `client` is the session id the home host knows the client by.
    Observer { client: crate::ClientId, nonce: u64 },
}

impl std::fmt::Debug for Credential {
    /// Never prints a secret: a JWT shows only its size (a name is public), an observer
    /// ticket only its client.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Local(name) => write!(f, "Local({:?})", name.0),
            Credential::Jwt(j) => write!(f, "Jwt(<{} bytes>)", j.len()),
            Credential::Observer { client, .. } => write!(f, "Observer {{ client: {client}, nonce: <secret> }}"),
        }
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

/// [`PlayerRecord`] as stored before registered prefabs (world-store player record
/// format 1): the same fields around a [`crate::ClientV1`]. Decode-only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerRecordV1 {
    pub client: crate::ClientV1,
    pub pose: Pose,
    pub region: RegionCoords,
    pub fortress: Option<SiteId>,
    pub adventure: Option<SiteId>,
}

impl From<PlayerRecordV1> for PlayerRecord {
    fn from(r: PlayerRecordV1) -> Self {
        let PlayerRecordV1 { client, pose, region, fortress, adventure } = r;
        PlayerRecord { client: client.into(), pose, region, fortress, adventure }
    }
}

/// `client` as a parked record keeps it: what belongs to the connection —
/// held input, the first-person flag, an open inventory, registered prefabs — is reset, the
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
    // Registered prefabs last for the session; the client registers again.
    c.prefabs.clear();
    if let Some(rest) = c.stow_cursor() {
        c.cursor = Some(rest);
    }
    c.arrive()
}

/// Every player body in `rollback` as a record (claims empty — the
/// `WorldManager` holds those), keyed by session. A save cut takes the
/// online players from each region's snapshot with this.
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
/// when it is clear of terrain there (`body_fits`: on voxel terrain its feet
/// tile and the tile above are clear of solid blocks) and inside
/// the region — water is clear, so a swimmer is restored in place; else onto
/// the standable tile nearest its feet level in its column (clamped into the
/// region); else over the region's spawn column (on its water's top where
/// it has any, `player_spawn_y`). Never inside terrain.
pub fn restore_translation(rollback: &Rollback, t: Vector) -> Vector {
    let src = rollback.tiles();
    let feet_at = rollback.feet_of(t);
    let feet = crate::feet_tile(feet_at);
    let n = crate::REGION_BLOCKS as i32;
    let inside = (0..n).contains(&feet[0]) && (0..n).contains(&feet[2]);
    if inside && crate::body_fits(&src, feet_at) {
        return t;
    }
    let column = [feet[0].clamp(0, n - 1), feet[2].clamp(0, n - 1)];
    match crate::nearest_standable(&src, column, Some(feet[1])) {
        Some(tile) => rollback.body_centre_on(tile),
        None => {
            let xz = rollback.spawn_xz();
            Vector::new(xz.0, rollback.player_spawn_y(xz), xz.1)
        }
    }
}
