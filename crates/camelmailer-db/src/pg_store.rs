//! The PostgreSQL implementations of the storage traits.
//!
//! [`PgStore`] implements both the async [`AdminStore`] (Admin API) and the
//! sync [`Store`] used by the SMTP session state machine. The sync methods
//! bridge onto the async pool via `block_in_place`, so they must run on a
//! multi-threaded tokio runtime (which the SMTP server does).

use async_trait::async_trait;
use camelmailer_core::{
    store, token, AdminStore, Credential, CredentialType, Domain, DomainOwner, Id, MessageScope,
    MessageSink, NewOrganization, NewServer, Organization, QueuedMessage, ResolvedRoute, Route,
    RouteMode, Server, ServerMode, Store, StoreError,
};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use std::future::Future;
use std::net::IpAddr;

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
    handle: tokio::runtime::Handle,
}

impl PgStore {
    /// Must be created from within a tokio runtime.
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            handle: tokio::runtime::Handle::current(),
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Bridge for the sync [`Store`] trait. Uses `block_in_place` when
    /// called from a runtime thread (requires the multi-thread flavor).
    fn wait<F: Future>(&self, future: F) -> F::Output {
        match tokio::runtime::Handle::try_current() {
            Ok(_) => tokio::task::block_in_place(|| self.handle.block_on(future)),
            Err(_) => self.handle.block_on(future),
        }
    }

    fn sqlx_error(error: sqlx::Error) -> StoreError {
        if let sqlx::Error::Database(db_error) = &error {
            if db_error.code().as_deref() == Some("23505") {
                return StoreError::Conflict("Permalink has already been taken".into());
            }
        }
        StoreError::Other(error.to_string())
    }
}

fn organization_from_row(row: &PgRow) -> Organization {
    Organization {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        name: row.get("name"),
        permalink: row.get("permalink"),
    }
}

fn server_from_row(row: &PgRow) -> Server {
    Server {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        organization_id: row.get::<i64, _>("organization_id") as Id,
        name: row.get("name"),
        permalink: row.get("permalink"),
        token: row.get("token"),
        mode: match row.get::<String, _>("mode").as_str() {
            "Development" => ServerMode::Development,
            _ => ServerMode::Live,
        },
        suspended: row.get("suspended"),
        suspension_reason: row.get("suspension_reason"),
        privacy_mode: row.get("privacy_mode"),
        log_smtp_data: row.get("log_smtp_data"),
        allow_sender: row.get("allow_sender"),
    }
}

fn credential_from_row(row: &PgRow) -> Credential {
    Credential {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        server_id: row.get::<i64, _>("server_id") as Id,
        credential_type: match row.get::<String, _>("type").as_str() {
            "API" => CredentialType::Api,
            "SMTP-IP" => CredentialType::SmtpIp,
            _ => CredentialType::Smtp,
        },
        name: row.get("name"),
        key: row.get("key"),
        hold: row.get("hold"),
    }
}

fn route_from_row(row: &PgRow) -> Route {
    Route {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        server_id: row.get::<i64, _>("server_id") as Id,
        domain_id: row.get::<Option<i64>, _>("domain_id").map(|id| id as Id),
        name: row.get("name"),
        token: row.get("token"),
        mode: route_mode_from_str(&row.get::<String, _>("mode")),
    }
}

fn route_mode_from_str(mode: &str) -> RouteMode {
    match mode {
        "Accept" => RouteMode::Accept,
        "Hold" => RouteMode::Hold,
        "Bounce" => RouteMode::Bounce,
        "Reject" => RouteMode::Reject,
        _ => RouteMode::Endpoint,
    }
}

fn route_mode_to_str(mode: RouteMode) -> &'static str {
    match mode {
        RouteMode::Endpoint => "Endpoint",
        RouteMode::Accept => "Accept",
        RouteMode::Hold => "Hold",
        RouteMode::Bounce => "Bounce",
        RouteMode::Reject => "Reject",
    }
}

fn credential_type_to_str(credential_type: CredentialType) -> &'static str {
    match credential_type {
        CredentialType::Smtp => "SMTP",
        CredentialType::Api => "API",
        CredentialType::SmtpIp => "SMTP-IP",
    }
}

const ROUTE_WITH_SERVER: &str = r#"
    SELECT r.id, r.uuid, r.server_id, r.domain_id, r.name, r.token, r.mode,
           COALESCE(d.name, '') AS domain_name,
           s.id AS s_id, s.uuid AS s_uuid, s.organization_id, s.name AS s_name,
           s.permalink, s.token AS s_token, s.mode AS s_mode, s.suspended,
           s.suspension_reason, s.privacy_mode, s.log_smtp_data, s.allow_sender
    FROM routes r
    JOIN servers s ON s.id = r.server_id
    LEFT JOIN domains d ON d.id = r.domain_id
"#;

fn resolved_route_from_row(row: &PgRow) -> ResolvedRoute {
    ResolvedRoute {
        route: route_from_row(row),
        server: Server {
            id: row.get::<i64, _>("s_id") as Id,
            uuid: row.get("s_uuid"),
            organization_id: row.get::<i64, _>("organization_id") as Id,
            name: row.get("s_name"),
            permalink: row.get("permalink"),
            token: row.get("s_token"),
            mode: match row.get::<String, _>("s_mode").as_str() {
                "Development" => ServerMode::Development,
                _ => ServerMode::Live,
            },
            suspended: row.get("suspended"),
            suspension_reason: row.get("suspension_reason"),
            privacy_mode: row.get("privacy_mode"),
            log_smtp_data: row.get("log_smtp_data"),
            allow_sender: row.get("allow_sender"),
        },
        domain_name: row.get("domain_name"),
    }
}

// --------------------------------------------------------- async helpers

impl PgStore {
    pub async fn organization_async(&self, id: Id) -> Result<Option<Organization>, StoreError> {
        sqlx::query("SELECT * FROM organizations WHERE id = $1")
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(organization_from_row))
            .map_err(Self::sqlx_error)
    }

    pub async fn server_async(&self, id: Id) -> Result<Option<Server>, StoreError> {
        sqlx::query("SELECT * FROM servers WHERE id = $1")
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(server_from_row))
            .map_err(Self::sqlx_error)
    }

    pub async fn create_domain(
        &self,
        owner: DomainOwner,
        name: &str,
        verified: bool,
    ) -> Result<Domain, StoreError> {
        let (owner_type, owner_id) = match owner {
            DomainOwner::Organization(id) => ("Organization", id as i64),
            DomainOwner::Server(id) => ("Server", id as i64),
        };
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO domains (uuid, owner_type, owner_id, name, verified)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(&uuid)
        .bind(owner_type)
        .bind(owner_id)
        .bind(name)
        .bind(verified)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(Domain {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            owner,
            name: name.into(),
            verified,
        })
    }

    pub async fn create_route(
        &self,
        server_id: Id,
        domain_id: Option<Id>,
        name: &str,
        mode: RouteMode,
    ) -> Result<Route, StoreError> {
        let uuid = token::generate_uuid();
        let route_token = token::generate_token(8);
        let row = sqlx::query(
            "INSERT INTO routes (uuid, server_id, domain_id, name, token, mode)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(&uuid)
        .bind(server_id as i64)
        .bind(domain_id.map(|id| id as i64))
        .bind(name)
        .bind(&route_token)
        .bind(route_mode_to_str(mode))
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(Route {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            server_id,
            domain_id,
            name: name.into(),
            token: route_token,
            mode,
        })
    }

    pub async fn create_credential(
        &self,
        server_id: Id,
        credential_type: CredentialType,
        key: &str,
    ) -> Result<Credential, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO credentials (uuid, server_id, type, name, key)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(&uuid)
        .bind(server_id as i64)
        .bind(credential_type_to_str(credential_type))
        .bind("Credential")
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(Credential {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            server_id,
            credential_type,
            name: "Credential".into(),
            key: key.into(),
            hold: false,
        })
    }
}

// ------------------------------------------------------------- AdminStore

#[async_trait]
impl AdminStore for PgStore {
    async fn list_organizations(&self) -> Result<Vec<Organization>, StoreError> {
        sqlx::query("SELECT * FROM organizations ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.iter().map(organization_from_row).collect())
            .map_err(Self::sqlx_error)
    }

    async fn organization_by_permalink(
        &self,
        permalink: &str,
    ) -> Result<Option<Organization>, StoreError> {
        sqlx::query("SELECT * FROM organizations WHERE permalink = $1")
            .bind(permalink)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(organization_from_row))
            .map_err(Self::sqlx_error)
    }

    async fn create_organization(
        &self,
        new: NewOrganization,
    ) -> Result<Organization, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO organizations (uuid, name, permalink)
             VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(&uuid)
        .bind(&new.name)
        .bind(&new.permalink)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(Organization {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            name: new.name,
            permalink: new.permalink,
        })
    }

    async fn delete_organization(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM organizations WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn servers_for_organization(
        &self,
        organization_id: Id,
    ) -> Result<Vec<Server>, StoreError> {
        sqlx::query("SELECT * FROM servers WHERE organization_id = $1 ORDER BY id")
            .bind(organization_id as i64)
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.iter().map(server_from_row).collect())
            .map_err(Self::sqlx_error)
    }

    async fn server_by_permalink(
        &self,
        organization_id: Id,
        permalink: &str,
    ) -> Result<Option<Server>, StoreError> {
        sqlx::query("SELECT * FROM servers WHERE organization_id = $1 AND permalink = $2")
            .bind(organization_id as i64)
            .bind(permalink)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(server_from_row))
            .map_err(Self::sqlx_error)
    }

    async fn create_server(&self, new: NewServer) -> Result<Server, StoreError> {
        let uuid = token::generate_uuid();
        let server_token = token::generate_token(6);
        let mode = match new.mode {
            ServerMode::Live => "Live",
            ServerMode::Development => "Development",
        };
        let row = sqlx::query(
            "INSERT INTO servers (uuid, organization_id, name, permalink, token, mode)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(&uuid)
        .bind(new.organization_id as i64)
        .bind(&new.name)
        .bind(&new.permalink)
        .bind(&server_token)
        .bind(mode)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(Server {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            organization_id: new.organization_id,
            name: new.name,
            permalink: new.permalink,
            token: server_token,
            mode: new.mode,
            suspended: false,
            suspension_reason: None,
            privacy_mode: false,
            log_smtp_data: false,
            allow_sender: false,
        })
    }

    async fn update_server(&self, server: Server) -> Result<Server, StoreError> {
        sqlx::query(
            "UPDATE servers SET name = $2, suspended = $3, suspension_reason = $4,
                    privacy_mode = $5, log_smtp_data = $6, allow_sender = $7
             WHERE id = $1",
        )
        .bind(server.id as i64)
        .bind(&server.name)
        .bind(server.suspended)
        .bind(&server.suspension_reason)
        .bind(server.privacy_mode)
        .bind(server.log_smtp_data)
        .bind(server.allow_sender)
        .execute(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(server)
    }

    async fn delete_server(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM servers WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn admin_api_key_valid(&self, key: &str) -> Result<bool, StoreError> {
        sqlx::query("UPDATE admin_api_keys SET last_used_at = now() WHERE key = $1")
            .bind(key)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn create_admin_api_key(&self, name: &str, key: &str) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO admin_api_keys (uuid, name, key) VALUES ($1, $2, $3)")
            .bind(token::generate_uuid())
            .bind(name)
            .bind(key)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(Self::sqlx_error)
    }
}

// ----------------------------------------------------- Store (SMTP, sync)

impl Store for PgStore {
    fn organization(&self, id: Id) -> Option<Organization> {
        self.wait(self.organization_async(id)).ok().flatten()
    }

    fn server(&self, id: Id) -> Option<Server> {
        self.wait(self.server_async(id)).ok().flatten()
    }

    fn find_smtp_credential_by_key(&self, key: &str) -> Option<Credential> {
        self.wait(async {
            sqlx::query("SELECT * FROM credentials WHERE type = 'SMTP' AND key = $1 LIMIT 1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await
        })
        .ok()
        .flatten()
        .as_ref()
        .map(credential_from_row)
    }

    fn find_server_by_token(&self, server_token: &str) -> Option<Server> {
        self.wait(async {
            sqlx::query("SELECT * FROM servers WHERE token = $1")
                .bind(server_token)
                .fetch_optional(&self.pool)
                .await
        })
        .ok()
        .flatten()
        .as_ref()
        .map(server_from_row)
    }

    fn find_route_by_token(&self, route_token: &str) -> Option<ResolvedRoute> {
        self.wait(async {
            sqlx::query(&format!("{ROUTE_WITH_SERVER} WHERE r.token = $1"))
                .bind(route_token)
                .fetch_optional(&self.pool)
                .await
        })
        .ok()
        .flatten()
        .as_ref()
        .map(resolved_route_from_row)
    }

    fn find_route_by_name_and_domain(&self, name: &str, domain: &str) -> Option<ResolvedRoute> {
        self.wait(async {
            sqlx::query(&format!(
                "{ROUTE_WITH_SERVER} WHERE r.name = $1 AND d.name = $2"
            ))
            .bind(name)
            .bind(domain)
            .fetch_optional(&self.pool)
            .await
        })
        .ok()
        .flatten()
        .as_ref()
        .map(resolved_route_from_row)
    }

    fn find_ip_credential(&self, ip: IpAddr) -> Option<Credential> {
        let credentials = self
            .wait(async {
                sqlx::query("SELECT * FROM credentials WHERE type = 'SMTP-IP'")
                    .fetch_all(&self.pool)
                    .await
            })
            .ok()?
            .iter()
            .map(credential_from_row)
            .collect();
        store::match_ip_credential(credentials, ip)
    }

    fn smtp_credentials_for_server(&self, server_id: Id) -> Vec<Credential> {
        self.wait(async {
            sqlx::query(
                "SELECT * FROM credentials WHERE server_id = $1 AND type = 'SMTP' ORDER BY id",
            )
            .bind(server_id as i64)
            .fetch_all(&self.pool)
            .await
        })
        .map(|rows| rows.iter().map(credential_from_row).collect())
        .unwrap_or_default()
    }

    fn find_server_by_permalinks(
        &self,
        org_permalink: &str,
        server_permalink: &str,
    ) -> Option<Server> {
        self.wait(async {
            sqlx::query(
                "SELECT s.* FROM servers s
                 JOIN organizations o ON o.id = s.organization_id
                 WHERE o.permalink = $1 AND s.permalink = $2",
            )
            .bind(org_permalink)
            .bind(server_permalink)
            .fetch_optional(&self.pool)
            .await
        })
        .ok()
        .flatten()
        .as_ref()
        .map(server_from_row)
    }

    fn find_authenticated_domain(&self, server_id: Id, header_values: &[&str]) -> Option<Id> {
        let server = self.server(server_id)?;

        let domain_for_address = |address: &str| -> Option<Id> {
            let address = store::strip_name_from_address(address);
            let (uname, domain_name) = address.split_once('@')?;
            if uname.is_empty() {
                return None;
            }
            self.wait(async {
                sqlx::query(
                    "SELECT id FROM domains
                     WHERE verified AND name = $1
                       AND ((owner_type = 'Server' AND owner_id = $2)
                         OR (owner_type = 'Organization' AND owner_id = $3))
                     ORDER BY owner_type DESC LIMIT 1",
                )
                .bind(domain_name)
                .bind(server.id as i64)
                .bind(server.organization_id as i64)
                .fetch_optional(&self.pool)
                .await
            })
            .ok()
            .flatten()
            .map(|row| row.get::<i64, _>("id") as Id)
        };

        let domains: Vec<Option<Id>> = header_values
            .iter()
            .map(|value| domain_for_address(value))
            .collect();
        if !domains.is_empty() && domains.iter().all(Option::is_some) {
            return domains[0];
        }
        None
    }

    fn return_path_route_for_server(&self, server_id: Id) -> Option<ResolvedRoute> {
        self.wait(async {
            sqlx::query(&format!(
                "{ROUTE_WITH_SERVER} WHERE r.server_id = $1 AND r.name = '__returnpath__'"
            ))
            .bind(server_id as i64)
            .fetch_optional(&self.pool)
            .await
        })
        .ok()
        .flatten()
        .as_ref()
        .map(resolved_route_from_row)
    }

    fn record_credential_use(&self, credential_id: Id) {
        let result = self.wait(async {
            sqlx::query("UPDATE credentials SET last_used_at = now() WHERE id = $1")
                .bind(credential_id as i64)
                .execute(&self.pool)
                .await
        });
        if let Err(error) = result {
            tracing::warn!(%error, credential_id, "failed to record credential use");
        }
    }
}

// ----------------------------------------------------------- message sink

/// A message stored in the shared, RLS-protected `messages` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: i64,
    pub server_id: Id,
    pub token: String,
    pub scope: String,
    pub rcpt_to: String,
    pub mail_from: String,
    pub bounce: bool,
    pub received_with_ssl: bool,
    pub domain_id: Option<Id>,
    pub credential_id: Option<Id>,
    pub route_id: Option<Id>,
    pub raw_message: Vec<u8>,
}

fn stored_message_from_row(row: &PgRow) -> StoredMessage {
    StoredMessage {
        id: row.get("id"),
        server_id: row.get::<i64, _>("server_id") as Id,
        token: row.get("token"),
        scope: row.get("scope"),
        rcpt_to: row.get("rcpt_to"),
        mail_from: row.get("mail_from"),
        bounce: row.get("bounce"),
        received_with_ssl: row.get("received_with_ssl"),
        domain_id: row.get::<Option<i64>, _>("domain_id").map(|id| id as Id),
        credential_id: row
            .get::<Option<i64>, _>("credential_id")
            .map(|id| id as Id),
        route_id: row.get::<Option<i64>, _>("route_id").map(|id| id as Id),
        raw_message: row.get("raw_message"),
    }
}

/// [`MessageSink`] backed by the RLS-protected `messages` table. All access
/// runs inside a transaction that establishes the tenant context first, so
/// the RLS policy validates every write.
#[derive(Clone)]
pub struct PgMessageSink {
    store: PgStore,
}

impl PgMessageSink {
    pub fn new(store: PgStore) -> Self {
        Self { store }
    }

    /// Establish the tenant context on a transaction (`SET LOCAL`).
    async fn set_tenant(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        server_id: Id,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT set_config('camelmailer.server_id', $1, true)")
            .bind(server_id.to_string())
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    pub async fn insert_message(&self, message: &QueuedMessage) -> Result<i64, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        Self::set_tenant(&mut tx, message.server_id).await?;
        let row = sqlx::query(
            "INSERT INTO messages
                 (server_id, token, scope, rcpt_to, mail_from, bounce,
                  received_with_ssl, domain_id, credential_id, route_id, raw_message)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             RETURNING id",
        )
        .bind(message.server_id as i64)
        .bind(token::generate_token(12))
        .bind(match message.scope {
            MessageScope::Incoming => "incoming",
            MessageScope::Outgoing => "outgoing",
        })
        .bind(&message.rcpt_to)
        .bind(&message.mail_from)
        .bind(message.bounce)
        .bind(message.received_with_ssl)
        .bind(message.domain_id.map(|id| id as i64))
        .bind(message.credential_id.map(|id| id as i64))
        .bind(message.route_id.map(|id| id as i64))
        .bind(&message.raw_message)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.get("id"))
    }

    /// List a tenant's messages. The query carries no `WHERE server_id`
    /// filter — row-level security scopes the result to the tenant context.
    pub async fn messages_for_server(
        &self,
        server_id: Id,
    ) -> Result<Vec<StoredMessage>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        Self::set_tenant(&mut tx, server_id).await?;
        let rows = sqlx::query("SELECT * FROM messages ORDER BY id")
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rows.iter().map(stored_message_from_row).collect())
    }
}

impl MessageSink for PgMessageSink {
    fn queue_message(&self, message: QueuedMessage) {
        let result = self.store.wait(self.insert_message(&message));
        if let Err(error) = result {
            tracing::error!(
                %error,
                server_id = message.server_id,
                rcpt_to = %message.rcpt_to,
                "failed to store accepted message"
            );
        }
    }
}
