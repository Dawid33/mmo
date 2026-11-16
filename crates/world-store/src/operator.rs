//! The operator's commands (R4, `worldctl`, `ctl.rs`; Postgres only). No coordinator fence —
//! the operator holds none. "Only the coordinator's fenced transactions write `successions`"
//! has three exceptions: the two operator commands below (the rollback also writes
//! `world.generation`), and a total-crash registration (`rows.rs` `register_host`, Elastic 1),
//! which abandons the open successions, unfenced, under the `world` row's lock — see the lock
//! walk in `succession.rs`. The operator's:
//! - [`retire_offline`] (`retire-host` with no host alive) — locks: `world` `FOR NO KEY
//!   UPDATE` first (the declaration's second lock; it holds no `coordinator` lock), then EVERY
//!   `hosts` row `FOR NO KEY UPDATE` in id order, then the retired host's `super_regions`
//!   rows `FOR NO KEY UPDATE` in key order; `lock_timeout` the cut lock timeout. It refuses
//!   (nothing written) while any dirty live row beat within the lease, read under those locks.
//!   Against a host REGISTERING at the same moment: `register_host` takes `world` too, so the
//!   two serialise — a registration first leaves a fresh row (refused: `Running`); a
//!   retirement first leaves no live row, so the next registration is a total crash (Elastic
//!   1, `rows.rs` `register_host`): it abandons the retirement's succession (the retired
//!   host's in-flight rows go to the heir) and takes every dirty row's land and in-flight rows
//!   at `epoch + 1`, raising the generation once more. A registration locks nothing before it
//!   holds `world`, so it closes no cycle with a retirement.
//!   Against a COORDINATOR starting: claiming its row writes only `coordinator`, which the
//!   retirement never touches; its declarations and decisions take `world` after
//!   `coordinator` and serialise on `world` — a declaration first is refused here (an open
//!   succession with survivors), a decision after it must settle the new succession or it is
//!   recorded as an abort (`decide_cut`). Against a STAGED PART: it holds its host's
//!   `super_regions` rows `FOR SHARE` (and nothing on `world` or `hosts`): the retirement
//!   waits for it at most `lock_timeout`, then fails naming the staged gids (`ctl.rs` resolves
//!   staged parts first, as every start does). Against a CLAIM: a claim takes the claimer's
//!   `hosts` row `FOR SHARE` as its LAST statement, after upserting `super_regions` rows; only a
//!   zombie claims here (no host is alive), and only the retired host's own re-claim
//!   (`claim_super_regions_live`: its rows, then its `hosts` row) can close a cycle with the
//!   retirement (which holds that `hosts` row and waits for those rows) — the declaration's
//!   cycle 1: `40P01` aborts one — a failed claim, or a retirement the operator runs again
//!   (`claim_free` locks only free rows: no cycle, see `succession.rs`). A heartbeat waits for the retirement (its short lock
//!   timeout fails that beat).
//! - [`vouch_host`] (`retire-host` while hosts live): ONE `UPDATE` of the retired host's row
//!   (its row lock only), guarded by the lapse — see the function.
//! - `drain_host` (`worldctl drain`, Elastic 1; in `rows.rs`, both engines — SQLite under the
//!   writer lock): ONE `UPDATE` of one dirty live `hosts` row's `drain_at_ms` (`LEAST` of the
//!   old mark and the database clock + the notice: it only moves earlier), guarded in its own
//!   `WHERE` by the row having beaten within the lease (final review A-M7: a lapsed row is
//!   refused — the guard reads only that row, no new lock), then, if it matched nothing, a
//!   plain read of that row to say why; `lock_timeout` the cut lock timeout. UNFENCED
//!   like the heartbeat: one `hosts` row, no ownership changed, nothing else wanted. It holds
//!   that one row and waits for nothing after it, so — like a heartbeat, which also takes one
//!   `hosts` row and nothing after it — it joins neither documented cycle (each needs a
//!   transaction that holds `super_regions` rows and waits for a `hosts` row, or the reverse):
//!   it waits at most its timeout for a declaration, retirement, registration or a claim's
//!   `check_live` holding the row, and nothing waits on it but a beat or another mark of that
//!   row. A heartbeat and a drain mark of one row serialise on it; the beat reads the mark it
//!   finds (`drain_at_ms - seen_ms`, one statement).
//! - [`rollback_world_operator`] (`rollback --yes`): `world` `FOR NO KEY UPDATE`, then
//!   [`crate::succession::rollback_in`]'s updates of `world`, `successions` and `in_flight`. Serialised with
//!   every declaration, registration and decision on `world`. The one row lock it can wait
//!   for: an heir's ADOPTING part staged for the settle cut deleted the predecessor's
//!   in-flight rows and holds them until its cut is decided — the re-key waits at most
//!   `lock_timeout` and fails (nothing written: the coordinator's rollback is retried, the
//!   operator runs it again); once decided, a commit settled the succession (nothing to
//!   abandon) and an abort left the rows as they were. The coordinator's [`crate::succession::rollback_world`]
//!   takes the same path after its fence.
//!
//! An offline retirement also raises `world.generation` (in its transaction, under the `world`
//! lock it holds): "the world is at its last cut as a whole" is then enforced, not assumed —
//! every process of the old generation (a partitioned host that only looked lapsed, a running
//! coordinator) is refused at its next write or hello (`StaleGeneration` /
//! `HigherGeneration`: a host exits 5; the coordinator's next decision is recorded aborted and
//! it re-reads the open successions), whatever the heir's row looks like. (Since Elastic 1 a
//! restarted process is a new host: the first registration after a retirement finds nobody
//! live and is a total crash, see above.)

use game::HostId;
use game::{CutId, HOST_DEAD, HOST_LIVE};
use sqlx::{PgPool, Row};

use crate::rows::i;
use crate::succession::{
    DeclareError, PG_NOW_MS, Succession, SuccessionId, SuccessionKind, declare_error, encode_moved, lock_world, rollback_in,
};
use crate::StoreError;

/// What an operator's rollback's lock timeout names.
const OPERATOR_ROLLING_BACK: &str = "worldctl rollback";
/// What an operator's offline retirement's lock timeout names.
const RETIRING: &str = "worldctl retire-host";

/// [`crate::StoreHandle::rollback_world_operator`] (Postgres, `worldctl rollback`): the world
/// row `FOR NO KEY UPDATE` (no coordinator fence: the operator holds none), then
/// [`crate::succession::rollback_in`]. Returns `(generation, successions abandoned)`.
pub(crate) async fn rollback_world_operator(pool: &PgPool, lock_timeout_ms: u64) -> Result<(u64, u64), StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query(&format!("SET LOCAL lock_timeout = '{lock_timeout_ms}ms'")).execute(&mut *tx).await?;
    let rolled = async {
        let stored = lock_world(&mut tx).await?;
        rollback_in(&mut tx, stored).await
    }
    .await;
    let (generation, abandoned) = rolled.map_err(|e| e.into_store(OPERATOR_ROLLING_BACK, lock_timeout_ms))?;
    tx.commit().await.map_err(|e| declare_error(OPERATOR_ROLLING_BACK, e, lock_timeout_ms))?;
    log::warn!(
        "[world-store] the operator rolled the world back to generation {generation}; {abandoned} open successions abandoned"
    );
    Ok((generation, abandoned))
}

/// What [`crate::StoreHandle::retire_offline`] did (`worldctl retire-host` with no host alive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retired {
    /// The succession it opened: no survivors, its base the last committed cut.
    Opened(Succession),
    /// These hosts' dirty live rows beat within the lease: a host runs (vouch instead, or
    /// stop it). Nothing written.
    Running(Vec<HostId>),
    /// No other id has a live `hosts` row to inherit the land. Nothing written.
    NoHeir,
    /// The row cannot be retired (absent, dead, clean, or named by an open succession): why.
    /// Nothing written.
    Refused(String),
}

/// What [`crate::StoreHandle::vouch_host`] did (`worldctl retire-host` while hosts live).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vouched {
    /// The row's `seen_ms` is 0: the coordinator declares it at its next read.
    Vouched,
    /// The row beat within the lease: that host runs. Nothing written.
    Running,
    /// No dirty live row of that id (absent, dead, clean): why. Nothing written.
    Refused(String),
}

/// Why host `retired`'s row (`(clean, state)`, `None`: absent) is no dirty live row, if it is not.
pub(crate) fn not_retirable(retired: HostId, row: Option<(bool, i64)>) -> Option<String> {
    let retired = retired.0;
    match row {
        None => Some(format!("host {retired} has no hosts row: nothing to retire")),
        Some((_, state)) if state != HOST_LIVE => Some(format!(
            "host {retired} was declared dead already: its land is inherited by its succession (worldctl rollback abandons \
             an open one)"
        )),
        Some((true, _)) => Some(format!(
            "host {retired} left cleanly: it holds no unsaved state (under claim its land is given away when wanted); nothing \
             to retire"
        )),
        Some((false, _)) => None,
    }
}

/// Why the open succession `s` refuses retiring host `retired`, if it does: it names that host
/// (predecessor or heir), or it is the coordinator's (it has survivors, all gone now).
pub(crate) fn open_succession_refuses(s: &Succession, retired: HostId) -> Option<String> {
    let (id, predecessor, heir) = (s.id.0, s.predecessor, s.heir);
    if predecessor == retired || heir == retired {
        let (predecessor, heir, retired) = (predecessor.0, heir.0, retired.0);
        return Some(format!(
            "the open succession {id} (host {predecessor} → heir {heir}) names host {retired}: let it settle (its heir, \
             host {heir}, attached and adopted), or worldctl rollback"
        ));
    }
    if !s.survivors.is_empty() {
        return Some(format!(
            "the coordinator's succession {id} (host {predecessor} → heir {heir}, survivors {:?}) is open and its survivors \
             are gone: the world must roll back (worldctl rollback) before a host is retired",
            s.survivors
        ));
    }
    None
}

/// A statement error of an operator command: a lock timeout names the staged cut parts of
/// this world that may hold the rows (`gids`).
async fn operator_error(pool: &PgPool, what: &str, e: DeclareError, lock_timeout_ms: u64) -> StoreError {
    match e {
        DeclareError::Store(e) => e,
        DeclareError::Sql(e) => {
            let gids: Vec<String> =
                sqlx::query_scalar(crate::open::PREPARED_GIDS_SQL).fetch_all(pool).await.unwrap_or_default();
            match StoreError::lock_conflict_of(&e, |text| {
                format!(
                    "{what} waited longer than its lock_timeout ({lock_timeout_ms} ms) for a row lock (staged cut parts of \
                     this world: {gids:?} — start the coordinator or a host once, which resolves them, or run ROLLBACK \
                     PREPARED '<gid>' for each); nothing was written ({text})"
                )
            }) {
                Some(lost) => lost,
                None => e.into(),
            }
        }
    }
}

/// [`crate::StoreHandle::retire_offline`] (Postgres): see the module docs for the locks.
pub(crate) async fn retire_offline(pool: &PgPool, retired: HostId, lock_timeout_ms: u64, lease_ms: u64) -> Result<Retired, StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query(&format!("SET LOCAL lock_timeout = '{lock_timeout_ms}ms'")).execute(&mut *tx).await?;
    let retired = retire_in(&mut tx, retired, lease_ms).await;
    match retired {
        Ok(Retired::Opened(s)) => {
            tx.commit().await.map_err(|e| declare_error(RETIRING, e, lock_timeout_ms))?;
            log::warn!(
                "[world-store] succession {} opened by the operator: host {} (boot {}) retired while no host ran → heir {}, \
                 {} super-regions, base cut {}",
                s.id.0,
                s.predecessor,
                s.predecessor_boot,
                s.heir,
                s.moved.len(),
                s.base_cut.0
            );
            Ok(Retired::Opened(s))
        }
        Ok(other) => {
            tx.rollback().await?;
            Ok(other)
        }
        Err(e) => {
            drop(tx);
            Err(operator_error(pool, RETIRING, e, lock_timeout_ms).await)
        }
    }
}

/// [`retire_offline`]'s statements inside `tx`; anything but `Opened` is rolled back.
async fn retire_in(tx: &mut sqlx::PgConnection, retired: HostId, lease_ms: u64) -> Result<Retired, DeclareError> {
    // 1. The world row: serialised with every declaration, registration, rollback, decision.
    let stored = lock_world(&mut *tx).await?;
    // 2. Every `hosts` row, in id order (the declaration's order): a beat waits for this
    // transaction, so none lands between the lapse check and the commit.
    let rows = sqlx::query(&format!(
        "SELECT id, boot, clean, state, seen_ms <> 0 AND {PG_NOW_MS} - seen_ms <= $1 FROM hosts \
         ORDER BY id FOR NO KEY UPDATE"
    ))
    .bind(i(lease_ms))
    .fetch_all(&mut *tx)
    .await?;
    let mut hosts = Vec::with_capacity(rows.len());
    for r in rows {
        let (o, boot, clean, state, fresh): (i64, i64, i64, i64, bool) =
            (r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?, r.try_get(4)?);
        let o = u64::try_from(o).map(HostId).map_err(|_| StoreError::Corrupt(format!("hosts.id {o}")))?;
        hosts.push((o, boot as u64, clean != 0, state, fresh));
    }
    let running: Vec<HostId> =
        hosts.iter().filter(|(_, _, clean, state, fresh)| !clean && *state == HOST_LIVE && *fresh).map(|h| h.0).collect();
    if !running.is_empty() {
        return Ok(Retired::Running(running));
    }
    let mine = hosts.iter().find(|h| h.0 == retired);
    if let Some(why) = not_retirable(retired, mine.map(|h| (h.2, h.3))) {
        return Ok(Retired::Refused(why));
    }
    let boot = mine.expect("checked above").1;
    // 3. The open successions (read under the world row's lock, which every writer of them takes).
    let open = sqlx::query("SELECT id, predecessor, heir, survivors FROM successions WHERE settled_cut IS NULL AND abandoned = 0 ORDER BY id")
        .fetch_all(&mut *tx)
        .await?;
    for r in open {
        let (id, predecessor, heir, survivors): (i64, i64, i64, String) = (r.try_get(0)?, r.try_get(1)?, r.try_get(2)?, r.try_get(3)?);
        let s = Succession {
            id: SuccessionId(id as u64),
            predecessor: u64::try_from(predecessor).map_or(HostId(u64::MAX), HostId),
            predecessor_boot: 0,
            heir: u64::try_from(heir).map_or(HostId(u64::MAX), HostId),
            kind: SuccessionKind::Crash,
            base_cut: CutId(0),
            moved: Vec::new(),
            survivors: crate::succession::decode_survivors(id as u64, &survivors)?,
        };
        if let Some(why) = open_succession_refuses(&s, retired) {
            return Ok(Retired::Refused(why));
        }
    }
    // 4. The heir: the lowest other live-row id (dirty or clean). Elastic 1: a restarted process
    // is a new host, so that id may never attach again; the next registration then finds
    // nobody alive and its total-crash rule abandons this succession and takes the land.
    let Some(heir) = hosts.iter().filter(|h| h.0 != retired && h.3 == HOST_LIVE).map(|h| h.0).min() else {
        return Ok(Retired::NoHeir);
    };
    let base_cut: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(cut_id), 0) FROM cut_decisions WHERE committed = 1")
        .fetch_one(&mut *tx)
        .await?;
    // 5. The world restarts from cut K as a whole: every process of the old generation is
    // refused from now on (see the module docs).
    sqlx::query("UPDATE world SET generation = $1 WHERE id = 1").bind(i(stored + 1)).execute(&mut *tx).await?;
    // 6. As a declaration: the row dead, the land moved (key order, then the claim itself).
    sqlx::query(&format!("UPDATE hosts SET state = {HOST_DEAD} WHERE id = $1 AND boot = $2"))
        .bind(i(retired.0))
        .bind(i(boot))
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT 1 FROM super_regions WHERE owner = $1 ORDER BY sx, sz FOR NO KEY UPDATE")
        .bind(i(retired.0))
        .execute(&mut *tx)
        .await?;
    let rows = sqlx::query("UPDATE super_regions SET owner = $1, epoch = epoch + 1 WHERE owner = $2 RETURNING sx, sz, epoch")
        .bind(i(heir.0))
        .bind(i(retired.0))
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
    let id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) + 1 FROM successions").fetch_one(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO successions (id, predecessor, predecessor_boot, heir, kind, base_cut, settled_cut, abandoned, \
         declared_utc, survivors, moved) VALUES ($1, $2, $3, $4, $5, $6, NULL, 0, $7, '', $8)",
    )
    .bind(id)
    .bind(i(retired.0))
    .bind(i(boot))
    .bind(i(heir.0))
    .bind(SuccessionKind::Crash.code())
    .bind(base_cut)
    .bind(crate::open::utc_now())
    .bind(encode_moved(&moved))
    .execute(&mut *tx)
    .await?;
    Ok(Retired::Opened(Succession {
        id: SuccessionId(id as u64),
        predecessor: retired,
        predecessor_boot: boot,
        heir,
        kind: SuccessionKind::Crash,
        base_cut: CutId(base_cut as u64),
        moved,
        survivors: Vec::new(),
    }))
}

/// [`crate::StoreHandle::vouch_host`] (Postgres): ONE statement over host `retired`'s row
/// (its row lock; a heartbeat either commits first — then the row is fresh and nothing is
/// written — or overwrites the vouch after, which the declaration's lapse re-check sees).
pub(crate) async fn vouch_host(pool: &PgPool, retired: HostId, lease_ms: u64, lock_timeout_ms: u64) -> Result<Vouched, StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query(&format!("SET LOCAL lock_timeout = '{lock_timeout_ms}ms'")).execute(&mut *tx).await?;
    let done = sqlx::query(&format!(
        "UPDATE hosts SET seen_ms = 0 WHERE id = $1 AND clean = 0 AND state = {HOST_LIVE} \
         AND (seen_ms = 0 OR {PG_NOW_MS} - seen_ms > $2)"
    ))
    .bind(i(retired.0))
    .bind(i(lease_ms))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if done == 1 {
        tx.commit().await?;
        log::warn!("[world-store] the operator vouched for host {retired}'s death (seen_ms = 0)");
        return Ok(Vouched::Vouched);
    }
    let row = sqlx::query("SELECT clean, state FROM hosts WHERE id = $1")
        .bind(i(retired.0))
        .fetch_optional(&mut *tx)
        .await?;
    tx.rollback().await?;
    let row = match row {
        Some(r) => Some((r.try_get::<i64, _>(0)? != 0, r.try_get::<i64, _>(1)?)),
        None => None,
    };
    Ok(match not_retirable(retired, row) {
        Some(why) => Vouched::Refused(why),
        None => Vouched::Running,
    })
}

