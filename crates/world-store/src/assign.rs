//! Elastic 3: the coordinator gives a free (or absent) super-region to a live host —
//! [`game::coordinator::StoreDecision::AssignLand`], run inside its transition's transaction,
//! in a savepoint after the compare-and-set holds the `coordinator` row (`coordinator_tx.rs`).
//! An assignment is an epoch bump in that one transaction, as a claim is: first writer wins,
//! a row another host holds is never touched — and never locked, so it never waits for a
//! staged cut part. Postgres only. The lock walk: `succession.rs`'s module notes.

use game::HOST_LIVE;
use game::coordinator::{AssignLand, Assigned};
use sqlx::Row;

use crate::StoreError;
use crate::rows::i;

/// [`AssignLand`]'s statements inside `tx` (a coordinator transition's; anything but `Done` is
/// rolled back by the caller, to its savepoint). In this order:
/// 1. the assignee's `hosts` row of that boot, live and dirty, `FOR SHARE` (held to the commit:
///    a declaration or a total-crash registration of it waits, then sees the assignment);
///    none → [`Assigned::HostNotLive`], a drain mark → [`Assigned::HostDraining`];
/// 2. a PLAIN read of the row (no lock): owned in the committed snapshot → [`Assigned::Owned`]
///    at once (Elastic 3 Task 5: a staged part that RELEASES the row has UPDATED it, and the
///    unique check of an `INSERT … ON CONFLICT` waits for an uncommitted updater — this read
///    keeps the assignment from waiting for a staged part);
/// 3. a row absent from that read inserted at epoch 1 (`ON CONFLICT DO NOTHING`);
/// 4. else a FREE row taken at `epoch + 1` (`WHERE owner = NO_OWNER`: a held row does not
///    match and is not locked); no row → [`Assigned::Owned`].
pub(crate) async fn assign_land_in(
    tx: &mut sqlx::PgConnection,
    a: &AssignLand,
) -> Result<Assigned, StoreError> {
    let host = sqlx::query(&format!(
        "SELECT drain_at_ms FROM hosts WHERE id = $1 AND boot = $2 AND state = {HOST_LIVE} AND clean = 0 FOR SHARE"
    ))
    .bind(i(a.host.0))
    .bind(i(a.boot))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(host) = host else {
        return Ok(Assigned::HostNotLive);
    };
    if host.try_get::<Option<i64>, _>(0)?.is_some() {
        return Ok(Assigned::HostDraining);
    }
    let (sx, sz) = (i64::from(a.sr.0), i64::from(a.sr.1));
    let owner: Option<i64> =
        sqlx::query_scalar("SELECT owner FROM super_regions WHERE sx = $1 AND sz = $2")
            .bind(sx)
            .bind(sz)
            .fetch_optional(&mut *tx)
            .await?;
    match owner {
        Some(o) if o != crate::NO_OWNER => return Ok(Assigned::Owned),
        Some(_) => {}
        None => {
            let inserted: Option<i64> = sqlx::query_scalar(
                "INSERT INTO super_regions (sx, sz, owner, epoch) VALUES ($1, $2, $3, 1) ON CONFLICT (sx, sz) DO NOTHING RETURNING epoch",
            )
            .bind(sx)
            .bind(sz)
            .bind(i(a.host.0))
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(epoch) = inserted {
                return Ok(Assigned::Done {
                    epoch: epoch as u64,
                });
            }
        }
    }
    let taken: Option<i64> = sqlx::query_scalar(&format!(
        "UPDATE super_regions SET owner = $3, epoch = epoch + 1 WHERE sx = $1 AND sz = $2 AND owner = {} RETURNING epoch",
        crate::NO_OWNER
    ))
    .bind(sx)
    .bind(sz)
    .bind(i(a.host.0))
    .fetch_optional(&mut *tx)
    .await?;
    Ok(match taken {
        Some(epoch) => Assigned::Done {
            epoch: epoch as u64,
        },
        None => Assigned::Owned,
    })
}
