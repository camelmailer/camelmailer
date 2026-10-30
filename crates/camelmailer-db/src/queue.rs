//! The delivery queue — the port of Postal's main-DB `QueuedMessage` model
//! and the locking logic in `app/lib/message_dequeuer`.
//!
//! The queue itself is cross-tenant (the worker's work list); message
//! *content* is only ever loaded by entering the owning server's RLS tenant
//! context. Locking uses `FOR UPDATE SKIP LOCKED` so multiple workers can
//! dequeue concurrently without stepping on each other.
//!
//! Dequeuing is fair between servers: a worker takes turns between the
//! servers that have ready mail instead of taking the oldest row of the whole
//! queue, so one server's large send cannot hold back everyone else's mail.
//! Within a server, transactional mail goes before bulk mail (broadcast
//! streams, campaigns). A [`QueueFilter`] lets a worker serve only some
//! servers or only one kind of mail, for dedicated workers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use camelmailer_core::Id;
use sqlx::postgres::PgRow;
use sqlx::{Executor, PgPool, Postgres, Row};

/// Which kind of mail a worker delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueKind {
    /// Everything, transactional before bulk.
    #[default]
    All,
    /// Only mail that is not bulk (transactional streams, inbound).
    Transactional,
    /// Only bulk mail (broadcast streams, campaigns).
    Bulk,
}

/// The part of the queue a worker serves. The default serves all of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueFilter {
    /// Serve only these servers; `None` serves every server.
    pub servers: Option<Vec<Id>>,
    /// Never serve these servers.
    pub exclude_servers: Vec<Id>,
    pub kind: QueueKind,
}

impl QueueFilter {
    fn servers_param(&self) -> Option<Vec<i64>> {
        self.servers
            .as_ref()
            .map(|ids| ids.iter().map(|id| *id as i64).collect())
    }

    fn exclude_param(&self) -> Vec<i64> {
        self.exclude_servers.iter().map(|id| *id as i64).collect()
    }

    fn bulk_param(&self) -> Option<bool> {
        match self.kind {
            QueueKind::All => None,
            QueueKind::Transactional => Some(false),
            QueueKind::Bulk => Some(true),
        }
    }
}

/// Look up `organization/server` permalinks for a [`QueueFilter`]. Every
/// entry has to name an existing server, so a typo in the worker config stops
/// the worker at start instead of silently serving nothing.
pub async fn resolve_server_permalinks(
    pool: &PgPool,
    permalinks: &[String],
) -> Result<Vec<Id>, String> {
    let mut ids = Vec::with_capacity(permalinks.len());
    for entry in permalinks {
        let Some((organization, server)) = entry.trim().split_once('/') else {
            return Err(format!(
                "{entry:?} is not organization/server (for example acme/transactional)"
            ));
        };
        let row = sqlx::query(
            "SELECT s.id FROM servers s
             JOIN organizations o ON o.id = s.organization_id
             WHERE o.permalink = $1 AND s.permalink = $2",
        )
        .bind(organization)
        .bind(server)
        .fetch_optional(pool)
        .await
        .map_err(|error| error.to_string())?;
        match row {
            Some(row) => ids.push(row.get::<i64, _>("id") as Id),
            None => return Err(format!("no server {entry:?}")),
        }
    }
    Ok(ids)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedMessageRow {
    pub id: i64,
    pub message_id: i64,
    pub server_id: Id,
    pub domain: String,
    pub attempts: i32,
}

fn queued_from_row(row: &PgRow) -> QueuedMessageRow {
    QueuedMessageRow {
        id: row.get("id"),
        message_id: row.get("message_id"),
        server_id: row.get::<i64, _>("server_id") as Id,
        domain: row.get("domain"),
        attempts: row.get("attempts"),
    }
}

/// Preserve the existing retry schedule: 1 minute initially, up to 1024 minutes.
pub(crate) fn retry_delay_minutes(attempts: i32) -> i32 {
    2_i32.pow(attempts.clamp(0, 10) as u32)
}

/// Queue mutations accept either a pool or the delivery's open transaction.
/// Delivery callers additionally constrain the row to its message and tenant.
pub(crate) async fn complete_message<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    id: i64,
    owner: Option<(i64, Id)>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM queued_messages WHERE id = $1
         AND ($2::bigint IS NULL OR message_id = $2)
         AND ($3::bigint IS NULL OR server_id = $3)",
    )
    .bind(id)
    .bind(owner.map(|(message_id, _)| message_id))
    .bind(owner.map(|(_, server_id)| server_id as i64))
    .execute(executor)
    .await?;
    Ok(())
}

pub(crate) async fn retry_message<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    id: i64,
    attempts: i32,
    owner: Option<(i64, Id)>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE queued_messages
         SET locked_by = NULL, locked_at = NULL, attempts = attempts + 1,
             retry_after = now() + make_interval(mins => $2::int)
         WHERE id = $1
         AND ($3::bigint IS NULL OR message_id = $3)
         AND ($4::bigint IS NULL OR server_id = $4)",
    )
    .bind(id)
    .bind(retry_delay_minutes(attempts))
    .bind(owner.map(|(message_id, _)| message_id))
    .bind(owner.map(|(_, server_id)| server_id as i64))
    .execute(executor)
    .await?;
    Ok(())
}

/// Default stale-lock window (days) when a caller does not configure one.
/// Matches `camelmailer.queued_message_lock_stale_days`'s default.
const DEFAULT_STALE_LOCK_DAYS: i32 = 1;

/// When this queue last served each server, as a turn number. Shared by the
/// clones of one queue, so the delivery loops of one worker take turns
/// together.
#[derive(Default)]
struct Turns {
    counter: AtomicU64,
    last: Mutex<HashMap<i64, u64>>,
}

impl Turns {
    fn last_served(&self, server_id: i64) -> Option<u64> {
        self.last
            .lock()
            .expect("queue turns lock")
            .get(&server_id)
            .copied()
    }

    fn served(&self, server_id: i64) {
        let turn = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut last = self.last.lock().expect("queue turns lock");
        // Forget servers that left the queue long ago, so the map stays small.
        if last.len() > 10_000 {
            last.clear();
        }
        last.insert(server_id, turn);
    }
}

/// A server with ready mail, as seen by one dequeue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Candidate {
    server_id: i64,
    /// Messages of this server other workers hold right now.
    in_flight: i64,
    /// Its oldest ready message, the tie-breaker between equal servers.
    oldest: i64,
}

/// The order in which one dequeue tries the servers: fewest messages in
/// flight first, so concurrent workers spread over the servers; then the
/// server this queue served longest ago (never served first), so a single
/// worker takes turns; then the oldest waiting mail.
fn turn_order(
    mut candidates: Vec<Candidate>,
    last_served: impl Fn(i64) -> Option<u64>,
) -> Vec<i64> {
    candidates.sort_by_key(|c| {
        (
            c.in_flight,
            last_served(c.server_id).map_or(0, |turn| turn + 1),
            c.oldest,
        )
    });
    candidates.into_iter().map(|c| c.server_id).collect()
}

#[derive(Clone)]
pub struct PgQueue {
    pool: PgPool,
    /// A message locked (`locked_by` set) longer ago than this is treated as
    /// abandoned by a crashed worker and re-dequeued.
    stale_lock_days: i32,
    filter: QueueFilter,
    turns: Arc<Turns>,
}

impl PgQueue {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            stale_lock_days: DEFAULT_STALE_LOCK_DAYS,
            filter: QueueFilter::default(),
            turns: Arc::default(),
        }
    }

    /// Serve only the part of the queue the filter names.
    pub fn with_filter(mut self, filter: QueueFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Like [`PgQueue::new`] but with an explicit stale-lock window (from
    /// `camelmailer.queued_message_lock_stale_days`). A value `<= 0` is
    /// clamped to 1 day so a misconfiguration can never reclaim actively
    /// locked messages.
    pub fn with_stale_lock_days(pool: PgPool, stale_lock_days: i32) -> Self {
        Self {
            pool,
            stale_lock_days: stale_lock_days.max(1),
            filter: QueueFilter::default(),
            turns: Arc::default(),
        }
    }

    pub async fn enqueue(
        &self,
        message_id: i64,
        server_id: Id,
        domain: &str,
    ) -> Result<i64, sqlx::Error> {
        self.enqueue_with_kind(message_id, server_id, domain, false)
            .await
    }

    /// Like [`PgQueue::enqueue`], marking the message as bulk mail or not.
    pub async fn enqueue_with_kind(
        &self,
        message_id: i64,
        server_id: Id,
        domain: &str,
        bulk: bool,
    ) -> Result<i64, sqlx::Error> {
        let row = sqlx::query(
            "INSERT INTO queued_messages (message_id, server_id, domain, bulk)
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(message_id)
        .bind(server_id as i64)
        .bind(domain)
        .bind(bulk)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get("id"))
    }

    /// Lock and return the next ready queued message, if any.
    ///
    /// A row is ready when it is unlocked, *or* when its lock is stale — held
    /// since before `now() - queued_message_lock_stale_days`, i.e. by a worker
    /// that crashed mid-delivery. Without the stale branch such a row would
    /// stay "sending" forever; with it, a surviving worker reclaims it.
    ///
    /// Two steps: list the servers with ready mail (within the filter), then
    /// try them in [`turn_order`] and lock the first ready row of the first
    /// server that still has one. A row another worker locked in between is
    /// skipped, not waited for.
    pub async fn dequeue(&self, worker_id: &str) -> Result<Option<QueuedMessageRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT q.server_id, min(q.id) AS oldest, coalesce(max(f.n), 0) AS in_flight
             FROM queued_messages q
             LEFT JOIN (
                 SELECT server_id, count(*) AS n FROM queued_messages
                 WHERE locked_by IS NOT NULL
                   AND locked_at >= now() - make_interval(days => $1::int)
                 GROUP BY server_id
             ) f ON f.server_id = q.server_id
             WHERE (q.locked_by IS NULL
                    OR q.locked_at < now() - make_interval(days => $1::int))
               AND (q.retry_after IS NULL OR q.retry_after <= now())
               AND ($2::bigint[] IS NULL OR q.server_id = ANY($2))
               AND NOT (q.server_id = ANY($3))
               AND ($4::boolean IS NULL OR q.bulk = $4)
             GROUP BY q.server_id
             ORDER BY in_flight, oldest
             LIMIT 1000",
        )
        .bind(self.stale_lock_days)
        .bind(self.filter.servers_param())
        .bind(self.filter.exclude_param())
        .bind(self.filter.bulk_param())
        .fetch_all(&self.pool)
        .await?;
        let candidates = rows
            .iter()
            .map(|row| Candidate {
                server_id: row.get("server_id"),
                in_flight: row.get("in_flight"),
                oldest: row.get("oldest"),
            })
            .collect();

        for server_id in turn_order(candidates, |id| self.turns.last_served(id)) {
            let row = sqlx::query(
                "UPDATE queued_messages SET locked_by = $1, locked_at = now()
                 WHERE id = (
                     SELECT id FROM queued_messages
                     WHERE server_id = $2
                       AND (locked_by IS NULL
                            OR locked_at < now() - make_interval(days => $3::int))
                       AND (retry_after IS NULL OR retry_after <= now())
                       AND ($4::boolean IS NULL OR bulk = $4)
                     ORDER BY bulk, id
                     LIMIT 1
                     FOR UPDATE SKIP LOCKED
                 )
                 RETURNING *",
            )
            .bind(worker_id)
            .bind(server_id)
            .bind(self.stale_lock_days)
            .bind(self.filter.bulk_param())
            .fetch_optional(&self.pool)
            .await?;
            if let Some(row) = row {
                self.turns.served(server_id);
                return Ok(Some(queued_from_row(&row)));
            }
        }
        Ok(None)
    }

    /// Delivery finished (successfully or terminally) — remove from queue.
    pub async fn complete(&self, id: i64) -> Result<(), sqlx::Error> {
        complete_message(&self.pool, id, None).await
    }

    /// Soft failure — unlock and reschedule with exponential backoff.
    pub async fn retry(&self, id: i64, attempts: i32) -> Result<(), sqlx::Error> {
        retry_message(&self.pool, id, attempts, None).await
    }

    pub async fn queue_size(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query("SELECT count(*) AS c FROM queued_messages")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("c"))
    }

    /// Test helper: make every queued message immediately ready.
    pub async fn clear_backoff(&self) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE queued_messages SET retry_after = NULL, locked_by = NULL")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(server_id: i64, in_flight: i64, oldest: i64) -> Candidate {
        Candidate {
            server_id,
            in_flight,
            oldest,
        }
    }

    #[test]
    fn a_single_worker_takes_turns_between_servers() {
        // Server 1 has the oldest mail, but was served last.
        let candidates = vec![
            candidate(1, 0, 10),
            candidate(2, 0, 500),
            candidate(3, 0, 900),
        ];
        let last = HashMap::from([(1, 7), (2, 3)]);
        let order = turn_order(candidates, |id| last.get(&id).copied());
        // Never served first, then longest ago.
        assert_eq!(order, vec![3, 2, 1]);
    }

    #[test]
    fn concurrent_workers_spread_over_servers() {
        let candidates = vec![candidate(1, 4, 10), candidate(2, 0, 500)];
        let order = turn_order(candidates, |_| None);
        assert_eq!(order, vec![2, 1]);
    }

    #[test]
    fn equal_servers_go_oldest_first() {
        let candidates = vec![candidate(2, 0, 500), candidate(1, 0, 10)];
        assert_eq!(turn_order(candidates, |_| None), vec![1, 2]);
    }

    #[test]
    fn the_filter_maps_to_query_parameters() {
        let filter = QueueFilter {
            servers: Some(vec![4, 5]),
            exclude_servers: vec![6],
            kind: QueueKind::Bulk,
        };
        assert_eq!(filter.servers_param(), Some(vec![4, 5]));
        assert_eq!(filter.exclude_param(), vec![6]);
        assert_eq!(filter.bulk_param(), Some(true));
        assert_eq!(QueueFilter::default().servers_param(), None);
        assert_eq!(QueueFilter::default().bulk_param(), None);
    }
}
