//! The operator's commands (R4, `worldctl`, `ctl.rs`; Postgres only). No coordinator fence —
//! the operator holds none. "Only the coordinator's fenced transactions write `successions`"
//! has two exceptions: the operator's rollback below (it also writes `world.generation`), and
//! a total-crash registration (`rows.rs` `register_host`, Elastic 1),
//! which abandons the open successions, unfenced, under the `world` row's lock — see the lock
//! walk in `succession.rs`. The operator's:
//! - `retire-host` with NO host alive is refused (Elastic 2a): the first registration takes
//!   the land (a total crash, `rows.rs` `register_host`); an offline retirement that opened a
//!   succession and raised the generation is gone.
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
//!   it waits at most its timeout for a declaration, an end of a landless host, a registration or a claim's
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

use game::HostId;
use game::HOST_LIVE;
use sqlx::{PgPool, Row};

use crate::rows::i;
use crate::succession::{PG_NOW_MS, declare_error, lock_world, rollback_in};
use crate::StoreError;

/// What an operator's rollback's lock timeout names.
const OPERATOR_ROLLING_BACK: &str = "worldctl rollback";

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

