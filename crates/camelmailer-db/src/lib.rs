//! PostgreSQL persistence for CamelMailer.
//!
//! One database for all tenants: configuration tables (organizations,
//! servers, domains, routes, credentials, admin API keys) plus a single
//! `messages` table protected by row-level security. This replaces the Ruby
//! application's MariaDB layout (a main database plus one MariaDB database
//! per mail server).
//!
//! Tenant context is established per transaction with
//! `set_config('camelmailer.server_id', $1, true)`; the RLS policy on
//! `messages` (see `migrations/0002_rls.sql`) filters reads and rejects
//! writes outside that context. `FORCE ROW LEVEL SECURITY` keeps even the
//! table owner subject to the policy.

mod pg_store;
mod queue;

pub use pg_store::{PgMessageSink, PgStore, StoredMessage};
pub use queue::{PgQueue, QueuedMessageRow};

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Connect a pool to the given `postgres://` URL.
pub async fn connect(url: &str, pool_size: u32) -> Result<PgPool, DbError> {
    let pool = PgPoolOptions::new()
        .max_connections(pool_size)
        .connect(url)
        .await?;
    Ok(pool)
}

/// Run all pending migrations.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}
