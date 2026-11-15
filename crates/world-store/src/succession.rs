//! Successions (R4): when a host of a multi-host world dies (its lease lapsed, or its row
//! was vouched dead by its next boot) or left clean, the coordinator declares it in ONE
//! fenced transaction ([`declare`]) that marks a crashed host's row dead, inserts the
//! `successions` row and moves every super-region of the predecessor to one heir with
//! `epoch + 1` — that update is the claim; the heir only reads its new fence. Nothing else
//! is written until the next committed world cut, whose decision settles the open
//! successions in its own transaction (`two_phase::decide_cut`'s `settles`). A world
//! rollback ([`rollback_world`]) abandons the open ones; the land stays with the heirs.
//! Postgres only: several hosts need it (SQLite answers `Unsupported`).
//!
//! A `Leave` declaration needs the predecessor's row clean and does not mark it dead; a claim
//! by that boot is refused anyway — a claim's last statement (`rows.rs` `check_live`) needs
//! the claimer's row live AND dirty, so a cleanly-left boot claims nothing from its leave
//! cut's commit on, and its next boot registers dirty and claims normally.
//!
//! Locks (the declaration): `coordinator` `FOR SHARE`, then `world` `FOR NO KEY UPDATE`
//! (it serialises with `decide_cut`, `register_host` and `rollback_world`, which take the
//! same lock in the same order, and never waits for a claim's `FOR KEY SHARE`), then the
//! predecessors' `hosts` rows `FOR NO KEY UPDATE` in ordinal order (a heartbeat waits for
//! the declaration, so a beat between the lapse check and the update cannot be lost), then
//! each predecessor's `super_regions` rows `FOR NO KEY UPDATE` in key order. A staged cut
//! part of the predecessor holds those rows `FOR SHARE` until its cut is decided: the
//! declaration waits for it, at most `lock_timeout` (the caller declares only between
//! cuts; a timeout is a retry). It never locks a row a staged part could be waiting for.
//!
//! Two deadlock cycles with a CLAIM are possible, and both are safe (Postgres aborts one
//! transaction with `40P01`, which writes nothing; each side treats that as a retry or a
//! fence, never as a start-up error):
//! 1. **A boot's re-claim of its own rows vs the declaration of that boot**
//!    (`claim_super_regions_live`, `rows.rs`): the re-claim upserts the boot's
//!    `super_regions` rows in key order, then takes its `hosts` row `FOR SHARE` (its last
//!    statement); the declaration holds that `hosts` row `FOR NO KEY UPDATE` and waits for
//!    the same `super_regions` rows. If the re-claim is aborted, the server re-reads its row
//!    (`HostClaims::reclaim_own`): no longer its live boot (the declaration won) → the
//!    fenced exit (3); still live → it re-claims once more. If the declaration is aborted,
//!    the coordinator declares again after a fresh lease read (the re-claimed rows then move
//!    to the heir, or the row is no longer lapsed).
//! 2. **A zombie's `claim_free` vs a declaration of several predecessors**: the zombie B's
//!    upsert `ON CONFLICT` locks a row of another predecessor A (the row stays A's: its
//!    `WHERE owner = NO_OWNER` fails, but the conflict lock is taken), then waits for B's
//!    `hosts` row `FOR SHARE`; the declaration holds A's and B's `hosts` rows and waits for
//!    A's `super_regions` rows. An aborted claim is a failed claim (`DirectoryKeeper`: a
//!    retry after `CLAIM_RETRY_MS`, then refused `Fenced` once B is dead); an aborted
//!    declaration is retried as above.
//!
//! **A registration's lease refresh** (`rows.rs` `register_host`, the "nobody alive" branch
//! under `Succession`): one `UPDATE hosts SET seen_ms = <now> WHERE ordinal <> $me AND clean = 0
//! AND state = live` — row locks on SEVERAL other hosts' rows, in no fixed order. Safe: it runs
//! under the `world` row's `FOR NO KEY UPDATE`, which every other statement that locks more
//! than one `hosts` row takes first (the declaration, the settle in `decide_cut`,
//! `retire_offline`, another registration), so two of them never interleave. The one-row
//! lockers cannot close a cycle: a heartbeat holds only its own row (and fails after its 1 s
//! lock timeout: "no beat this time"); `vouch_host` is one guarded `UPDATE`; a claim takes its
//! own `hosts` row `FOR SHARE` as its LAST statement and commits — the registration waits for
//! that commit, or the claim waits for the registration's, and neither then wants anything the
//! other holds (the registration's fence check came first, and a claim blocked on one of those
//! `super_regions` rows has not locked its `hosts` row yet). A cut part locks no `hosts` row.
//!
//! **After a world rollback** (the coordinator's [`rollback_world`] or the operator's
//! [`rollback_world_operator`], both through [`rollback_in`]): every host restarts from the
//! last committed cut K — no cut commits between a declaration and its settle, so that is the
//! abandoned succession's base. The land stays with the heir, and so do the predecessor's
//! in-flight rows of K: they are re-keyed to the heir (`in_flight.host`) in the same
//! transaction. Left under the predecessor's ordinal they would have no reader: a host reads
//! the rows of its own ordinal at boot (`load_in_flight(Some(own))`) and an heir reads a
//! predecessor's only while an open succession names it (`load_in_flight_of`), so they would
//! be stranded for ever if that ordinal never boots again, and restored by a boot that owns
//! none of their land if it does. Re-keyed, the heir restores each exactly once at its boot
//! (forwarding a row whose target another host owns, as any boot does), and its first part
//! replaces them (it deletes every row of its ordinal); the predecessor's next boot, owning
//! nothing, reads none.
//!
//! The operator's commands (`worldctl retire-host`, `worldctl rollback`) are in `operator.rs`.

use std::collections::BTreeSet;

use game::HostId;
use game::{CutId, HOST_DEAD, HOST_LIVE};
use sqlx::{PgPool, Row};

use crate::rows::i;
use crate::{CoordinatorFence, StoreError, SuperRegion};

pub use game::{Declared, Succession, SuccessionId, SuccessionKind};

/// `successions.survivors`: ordinals, comma-separated.
pub(crate) fn encode_survivors(survivors: &[HostId]) -> String {
    survivors.iter().map(|h| h.0.to_string()).collect::<Vec<_>>().join(",")
}

pub(crate) fn decode_survivors(id: u64, s: &str) -> Result<Vec<HostId>, StoreError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    s.split(',')
        .map(|o| o.parse().map(HostId).map_err(|_| StoreError::Corrupt(format!("successions {id}: survivors {s:?}"))))
        .collect()
}

/// `successions.moved`: `sx:sz:epoch`, comma-separated.
pub(crate) fn encode_moved(moved: &[(SuperRegion, u64)]) -> String {
    moved.iter().map(|((sx, sz), e)| format!("{sx}:{sz}:{e}")).collect::<Vec<_>>().join(",")
}

pub(crate) fn decode_moved(id: u64, s: &str) -> Result<Vec<(SuperRegion, u64)>, StoreError> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let bad = || StoreError::Corrupt(format!("successions {id}: moved {s:?}"));
    s.split(',')
        .map(|m| {
            let mut p = m.split(':');
            match (p.next(), p.next(), p.next(), p.next()) {
                (Some(sx), Some(sz), Some(e), None) => Ok((
                    (sx.parse().map_err(|_| bad())?, sz.parse().map_err(|_| bad())?),
                    e.parse().map_err(|_| bad())?,
                )),
                _ => Err(bad()),
            }
        })
        .collect()
}

/// What every store that cannot declare a succession answers.
pub(crate) const SUCCESSION_UNSUPPORTED: &str = "successions need several hosts, which need Postgres";

/// The `lock_timeout` of a declaration ran out: a staged cut part (or another transaction)
/// holds a predecessor's row. Nothing was written; the caller declares again later.
pub(crate) fn declare_error(what: &str, e: sqlx::Error, lock_timeout_ms: u64) -> StoreError {
    match StoreError::lock_conflict_of(&e, |text| {
        format!(
            "{what} waited longer than its lock_timeout ({lock_timeout_ms} ms) for a row a staged cut part or \
             another transaction holds, or was chosen in a deadlock; nothing was written — retry once the cut is \
             decided ({text})"
        )
    }) {
        Some(lost) => lost,
        None => e.into(),
    }
}

/// The coordinator row `FOR SHARE` (`Fenced { sr: None }` unless `fence` holds it), then
/// the world row `FOR NO KEY UPDATE` (`StaleGeneration` unless at `generation`, when given).
/// Returns the stored generation.
async fn lock_coordinator_and_world(
    tx: &mut sqlx::PgConnection,
    fence: CoordinatorFence,
    generation: Option<u64>,
) -> Result<u64, DeclareError> {
    let epoch: Option<i64> = sqlx::query_scalar("SELECT epoch FROM coordinator WHERE id = 1 FOR SHARE")
        .fetch_optional(&mut *tx)
        .await?;
    if epoch.map(|e| e as u64) != Some(fence.epoch) {
        return Err(StoreError::Fenced { sr: None }.into());
    }
    let stored: i64 = sqlx::query_scalar("SELECT generation FROM world WHERE id = 1 FOR NO KEY UPDATE")
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
    if let Some(mine) = generation {
        if stored as u64 != mine {
            return Err(StoreError::StaleGeneration { stored: stored as u64, mine }.into());
        }
    }
    Ok(stored as u64)
}

/// The database clock in ms (as `rows`' `NOW_MS` for Postgres: `clock_timestamp()`, not the
/// transaction's start — this one may have waited for locks).
pub(crate) const PG_NOW_MS: &str = "(extract(epoch from clock_timestamp()) * 1000)::bigint";

/// What a declaration's lock timeout names.
const DECLARING: &str = "a succession's declaration";
/// What a rollback's lock timeout names.
const ROLLING_BACK: &str = "a world rollback";

/// [`crate::StoreHandle::declare_succession`] (Postgres): see the module docs for the locks.
pub(crate) async fn declare(
    pool: &PgPool,
    dead: Vec<(HostId, u64)>,
    heirs: Vec<HostId>,
    kind: SuccessionKind,
    fence: CoordinatorFence,
    generation: u64,
    lock_timeout_ms: u64,
    lease_ms: u64,
) -> Result<Declared, StoreError> {
    let predecessors: BTreeSet<HostId> = dead.iter().map(|(o, _)| *o).collect();
    if dead.is_empty() || heirs.len() != dead.len() || predecessors.len() != dead.len() {
        return Err(StoreError::Unsupported(format!(
            "a succession needs one heir per predecessor and each predecessor once (dead {dead:?}, heirs {heirs:?})"
        )));
    }
    if let Some(h) = heirs.iter().find(|h| predecessors.contains(h)) {
        return Err(StoreError::Unsupported(format!("host {h} cannot inherit while it is declared dead itself")));
    }
    let mut tx = pool.begin().await?;
    sqlx::query(&format!("SET LOCAL lock_timeout = '{lock_timeout_ms}ms'")).execute(&mut *tx).await?;
    let declared = declare_in(&mut tx, &dead, &heirs, &predecessors, kind, fence, generation, lease_ms).await;
    match declared {
        Ok(Declared::Done(s)) => {
            tx.commit().await.map_err(|e| declare_error(DECLARING, e, lock_timeout_ms))?;
            Ok(Declared::Done(s))
        }
        Ok(other) => {
            tx.rollback().await?;
            Ok(other)
        }
        Err(e) => Err(e.into_store(DECLARING, lock_timeout_ms)),
    }
}

/// A declaration's statement error, kept raw so a lock timeout is named as one.
pub(crate) enum DeclareError {
    Store(StoreError),
    Sql(sqlx::Error),
}

impl DeclareError {
    pub(crate) fn into_store(self, what: &str, lock_timeout_ms: u64) -> StoreError {
        match self {
            DeclareError::Store(e) => e,
            DeclareError::Sql(e) => declare_error(what, e, lock_timeout_ms),
        }
    }
}

impl From<sqlx::Error> for DeclareError {
    fn from(e: sqlx::Error) -> Self {
        DeclareError::Sql(e)
    }
}

impl From<StoreError> for DeclareError {
    fn from(e: StoreError) -> Self {
        DeclareError::Store(e)
    }
}

/// The declaration's statements inside `tx`; anything but `Done` is rolled back by the caller.
/// `lease_ms`: a row lapsed past it ([`game::HOST_LEASE_MS`] unless a test shortened it).
#[allow(clippy::too_many_arguments)]
async fn declare_in(
    tx: &mut sqlx::PgConnection,
    dead: &[(HostId, u64)],
    heirs: &[HostId],
    predecessors: &BTreeSet<HostId>,
    kind: SuccessionKind,
    fence: CoordinatorFence,
    generation: u64,
    lease_ms: u64,
) -> Result<Declared, DeclareError> {
    lock_coordinator_and_world(&mut *tx, fence, Some(generation)).await?;
    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM successions WHERE settled_cut IS NULL AND abandoned = 0")
        .fetch_one(&mut *tx)
        .await?;
    if open > 0 {
        return Ok(Declared::SuccessionOpen);
    }
    // The predecessors' rows, locked in ordinal order: a heartbeat waits for this transaction.
    let mut sorted: Vec<(HostId, u64)> = dead.to_vec();
    sorted.sort();
    let (mut gone, mut not_lapsed) = (Vec::new(), Vec::new());
    for (ordinal, boot) in &sorted {
        let row = sqlx::query(&format!(
            "SELECT boot, clean, state, seen_ms = 0 OR {PG_NOW_MS} - seen_ms > $2, \
             EXISTS (SELECT 1 FROM successions WHERE predecessor = $1 AND predecessor_boot = hosts.boot AND abandoned = 0) \
             FROM hosts WHERE ordinal = $1 FOR NO KEY UPDATE"
        ))
        .bind(i((*ordinal).0))
        .bind(i(lease_ms))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            gone.push(*ordinal);
            continue;
        };
        let (stored_boot, clean, state, lapsed, succeeded): (i64, i64, i64, bool, bool) =
            (row.try_get(0)?, row.try_get(1)?, row.try_get(2)?, row.try_get(3)?, row.try_get(4)?);
        if stored_boot as u64 != *boot || state != HOST_LIVE || succeeded {
            gone.push(*ordinal);
            continue;
        }
        let ready = match kind {
            SuccessionKind::Crash => clean == 0 && lapsed,
            SuccessionKind::Leave => clean != 0,
        };
        if !ready {
            not_lapsed.push(*ordinal);
        }
    }
    // Each heir: live, dirty and beating (a vouched row, `seen_ms = 0`, is not fresh).
    for heir in heirs {
        let fresh: Option<bool> = sqlx::query_scalar(&format!(
            "SELECT seen_ms <> 0 AND {PG_NOW_MS} - seen_ms <= $2 FROM hosts \
             WHERE ordinal = $1 AND clean = 0 AND state = {HOST_LIVE}"
        ))
        .bind(i(heir.0))
        .bind(i(lease_ms))
        .fetch_optional(&mut *tx)
        .await?;
        if fresh != Some(true) && !gone.contains(heir) {
            gone.push(*heir);
        }
    }
    if !gone.is_empty() {
        gone.sort();
        return Ok(Declared::Gone(gone));
    }
    if !not_lapsed.is_empty() {
        return Ok(Declared::NotLapsed(not_lapsed));
    }
    // Read under the world row's lock, which `register_host` takes: a host registering
    // later never met the dead boot; a live one the coordinator is not attached to is listed.
    let live: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT ordinal FROM hosts WHERE clean = 0 AND state = {HOST_LIVE} ORDER BY ordinal"
    ))
    .fetch_all(&mut *tx)
    .await?;
    let mut survivors = Vec::with_capacity(live.len());
    for o in live {
        let o = u64::try_from(o).map(HostId).map_err(|_| StoreError::Corrupt(format!("hosts.ordinal {o}")))?;
        if !predecessors.contains(&o) {
            survivors.push(o);
        }
    }
    let base_cut: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cut_id), 0) FROM cut_decisions WHERE committed = 1")
        .fetch_one(&mut *tx)
        .await?;
    let mut out = Vec::with_capacity(dead.len());
    for ((predecessor, boot), heir) in dead.iter().zip(heirs) {
        if kind == SuccessionKind::Crash {
            sqlx::query(&format!("UPDATE hosts SET state = {HOST_DEAD} WHERE ordinal = $1 AND boot = $2"))
                .bind(i(predecessor.0))
                .bind(i(*boot))
                .execute(&mut *tx)
                .await?;
        }
        // Key order first (a claim upserts in key order too), then the claim itself.
        sqlx::query("SELECT 1 FROM super_regions WHERE owner = $1 ORDER BY sx, sz FOR NO KEY UPDATE")
            .bind(i(predecessor.0))
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(
            "UPDATE super_regions SET owner = $1, epoch = epoch + 1 WHERE owner = $2 RETURNING sx, sz, epoch",
        )
        .bind(i(heir.0))
        .bind(i(predecessor.0))
        .fetch_all(&mut *tx)
        .await?;
        let mut moved = Vec::with_capacity(rows.len());
        for r in rows {
            let (sx, sz, epoch): (i64, i64, i64) = (r.try_get(0)?, r.try_get(1)?, r.try_get(2)?);
            let sr = (
                i32::try_from(sx).map_err(|_| StoreError::Corrupt(format!("super_regions.sx {sx}")))?,
                i32::try_from(sz).map_err(|_| StoreError::Corrupt(format!("super_regions.sz {sz}")))?,
            );
            moved.push((sr, epoch as u64));
        }
        moved.sort();
        let id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) + 1 FROM successions")
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO successions (id, predecessor, predecessor_boot, heir, kind, base_cut, settled_cut, abandoned, \
             declared_utc, survivors, moved) VALUES ($1, $2, $3, $4, $5, $6, NULL, 0, $7, $8, $9)",
        )
        .bind(id)
        .bind(i(predecessor.0))
        .bind(i(*boot))
        .bind(i(heir.0))
        .bind(kind.code())
        .bind(base_cut)
        .bind(crate::open::utc_now())
        .bind(encode_survivors(&survivors))
        .bind(encode_moved(&moved))
        .execute(&mut *tx)
        .await?;
        log::info!(
            "[world-store] succession {id}: host {predecessor} (boot {boot}, {kind:?}) → heir {heir}, {} super-regions, \
             base cut {base_cut}, survivors {survivors:?}",
            moved.len()
        );
        out.push(Succession {
            id: SuccessionId(id as u64),
            predecessor: *predecessor,
            predecessor_boot: *boot,
            heir: *heir,
            kind,
            base_cut: CutId(base_cut as u64),
            moved,
            survivors: survivors.clone(),
        });
    }
    Ok(Declared::Done(out))
}

/// [`crate::StoreHandle::rollback_world`] (Postgres): under the coordinator fence and the
/// world row's lock (the declaration's order), [`rollback_in`]. Returns the new generation.
pub(crate) async fn rollback_world(pool: &PgPool, fence: CoordinatorFence, lock_timeout_ms: u64) -> Result<u64, StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query(&format!("SET LOCAL lock_timeout = '{lock_timeout_ms}ms'")).execute(&mut *tx).await?;
    let rolled = async {
        let stored = lock_coordinator_and_world(&mut tx, fence, None).await?;
        rollback_in(&mut tx, stored).await
    }
    .await;
    let (generation, abandoned) = rolled.map_err(|e| e.into_store(ROLLING_BACK, lock_timeout_ms))?;
    tx.commit().await.map_err(|e| declare_error(ROLLING_BACK, e, lock_timeout_ms))?;
    log::warn!("[world-store] world rolled back to generation {generation}; {abandoned} open successions abandoned");
    Ok(generation)
}

/// The world row `FOR NO KEY UPDATE` (the lock every declaration, rollback, registration and
/// decision takes); its generation.
pub(crate) async fn lock_world(tx: &mut sqlx::PgConnection) -> Result<u64, DeclareError> {
    let stored: i64 = sqlx::query_scalar("SELECT generation FROM world WHERE id = 1 FOR NO KEY UPDATE")
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::Corrupt("the world row is missing".into()))?;
    Ok(stored as u64)
}

/// A world rollback inside `tx` (the world row locked, at `stored`): `world.generation + 1`,
/// every open succession abandoned, and each abandoned one's predecessor in-flight rows
/// re-keyed to its heir — the land stays with the heir, which restarts from the last
/// committed cut owning it, so the rows of that cut go with it (the module docs). Returns
/// `(generation, successions abandoned)`.
pub(crate) async fn rollback_in(tx: &mut sqlx::PgConnection, stored: u64) -> Result<(u64, u64), DeclareError> {
    let generation = stored + 1;
    sqlx::query("UPDATE world SET generation = $1 WHERE id = 1").bind(i(generation)).execute(&mut *tx).await?;
    // Ordered by id: a predecessor's rows end with the heir of its LAST open succession (one
    // predecessor has at most one open succession: its row is dead, or clean, after it).
    let abandoned: Vec<(i64, i64, i64)> = sqlx::query_as(
        "UPDATE successions SET abandoned = 1 WHERE settled_cut IS NULL AND abandoned = 0 \
         RETURNING id, predecessor, heir",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut abandoned = abandoned;
    abandoned.sort();
    for (id, predecessor, heir) in &abandoned {
        // `seq` keeps its range (the predecessor's): the key is `seq` alone, never shared.
        let moved = sqlx::query("UPDATE in_flight SET host = $1 WHERE host = $2")
            .bind(heir)
            .bind(predecessor)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if moved > 0 {
            log::warn!(
                "[world-store] succession {id} abandoned: host {predecessor}'s {moved} in-flight row(s) go to its heir {heir} \
                 with its land"
            );
        }
    }
    Ok((generation, abandoned.len() as u64))
}

/// The ids of the open successions, in `tx` (no lock: `decide_cut` reads them under the
/// world row's lock, which every declaration and rollback takes first).
pub(crate) async fn open_ids(tx: &mut sqlx::PgConnection) -> Result<BTreeSet<u64>, StoreError> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM successions WHERE settled_cut IS NULL AND abandoned = 0")
        .fetch_all(&mut *tx)
        .await?;
    Ok(ids.into_iter().map(|i| i as u64).collect())
}

/// Settles `settles` with `cut` (inside the committing decision's transaction): each
/// gets `settled_cut`, and each predecessor's row — of the boot that died — turns clean.
pub(crate) async fn settle(tx: &mut sqlx::PgConnection, cut: CutId, settles: &[SuccessionId]) -> Result<(), StoreError> {
    if settles.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = settles.iter().map(|s| s.0 as i64).collect();
    sqlx::query("UPDATE successions SET settled_cut = $1 WHERE id = ANY($2) AND settled_cut IS NULL AND abandoned = 0")
        .bind(cut.0 as i64)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE hosts SET clean = 1 FROM successions s \
         WHERE s.id = ANY($1) AND hosts.ordinal = s.predecessor AND hosts.boot = s.predecessor_boot",
    )
    .bind(&ids)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_lists_round_trip() {
        let moved = vec![((-3, 4), 7), ((0, -1), 1)];
        assert_eq!(decode_moved(1, &encode_moved(&moved)).unwrap(), moved);
        assert_eq!(decode_moved(1, "").unwrap(), vec![]);
        assert!(decode_moved(1, "1:2").is_err());
        assert_eq!(decode_survivors(1, &encode_survivors(&[HostId(0), HostId(2)])).unwrap(), vec![HostId(0), HostId(2)]);
        assert_eq!(decode_survivors(1, "").unwrap(), Vec::<HostId>::new());
        assert!(decode_survivors(1, "x").is_err());
    }
}
