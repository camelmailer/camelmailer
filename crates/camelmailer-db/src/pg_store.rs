//! The PostgreSQL implementations of the storage traits.
//!
//! [`PgStore`] implements both the async [`AdminStore`] (Admin API) and the
//! sync [`Store`] used by the SMTP session state machine. The sync methods
//! bridge onto the async pool via `block_in_place`, so they must run on a
//! multi-threaded tokio runtime (which the SMTP server does).

use async_trait::async_trait;
use camelmailer_core::{
    store, token, AdminStore, Credential, CredentialType, Domain, DomainOwner, Id, IpAddress,
    IpPool, MessageScope, MessageSink, NewCredential, NewIpAddress, NewOrganization, NewRoute,
    NewServer, NewSuppression, NewUser, NewWebhook, Organization, QueuedMessage, ResolvedRoute,
    Route, RouteMode, Server, ServerMode, Store, StoreError, Suppression, User, Webhook,
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
                let message = match db_error.constraint() {
                    Some(constraint) if constraint.starts_with("domains") => {
                        "Name has already been taken"
                    }
                    Some(constraint) if constraint.starts_with("suppressions") => {
                        "Address is already suppressed"
                    }
                    Some(constraint) if constraint.starts_with("users") => {
                        "Email address has already been taken"
                    }
                    _ => "Permalink has already been taken",
                };
                return StoreError::Conflict(message.into());
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
        ip_pool_id: row.get::<Option<i64>, _>("ip_pool_id").map(|id| id as Id),
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
        endpoint_url: row.get("endpoint_url"),
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

fn domain_from_row(row: &PgRow) -> Domain {
    let owner_id = row.get::<i64, _>("owner_id") as Id;
    Domain {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        owner: match row.get::<String, _>("owner_type").as_str() {
            "Organization" => DomainOwner::Organization(owner_id),
            _ => DomainOwner::Server(owner_id),
        },
        name: row.get("name"),
        verified: row.get("verified"),
    }
}

fn webhook_from_row(row: &PgRow) -> Webhook {
    Webhook {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        server_id: row.get::<i64, _>("server_id") as Id,
        name: row.get("name"),
        url: row.get("url"),
        all_events: row.get("all_events"),
        enabled: row.get("enabled"),
        sign: row.get("sign"),
    }
}

fn suppression_from_row(row: &PgRow) -> Suppression {
    Suppression {
        id: row.get::<i64, _>("id") as Id,
        server_id: row.get::<i64, _>("server_id") as Id,
        suppression_type: row.get("type"),
        address: row.get("address"),
        reason: row.get("reason"),
    }
}

fn user_from_row(row: &PgRow) -> User {
    User {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        email_address: row.get("email_address"),
        first_name: row.get("first_name"),
        last_name: row.get("last_name"),
        admin: row.get("admin"),
    }
}

fn ip_pool_from_row(row: &PgRow) -> IpPool {
    IpPool {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        name: row.get("name"),
        default: row.get("default"),
    }
}

fn ip_address_from_row(row: &PgRow) -> IpAddress {
    IpAddress {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        ip_pool_id: row.get::<i64, _>("ip_pool_id") as Id,
        ipv4: row.get("ipv4"),
        ipv6: row.get("ipv6"),
        hostname: row.get("hostname"),
        priority: row.get("priority"),
    }
}

/// Establish the tenant context on a transaction (`SET LOCAL`), scoping all
/// RLS-protected tables (messages, suppressions) to one mail server.
pub(crate) async fn set_tenant_context(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    server_id: Id,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('camelmailer.server_id', $1, true)")
        .bind(server_id.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

const ROUTE_WITH_SERVER: &str = r#"
    SELECT r.id, r.uuid, r.server_id, r.domain_id, r.name, r.token, r.mode, r.endpoint_url,
           COALESCE(d.name, '') AS domain_name,
           s.id AS s_id, s.uuid AS s_uuid, s.organization_id, s.name AS s_name,
           s.permalink, s.token AS s_token, s.mode AS s_mode, s.suspended,
           s.suspension_reason, s.privacy_mode, s.log_smtp_data, s.allow_sender,
           s.ip_pool_id AS s_ip_pool_id
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
            ip_pool_id: row.get::<Option<i64>, _>("s_ip_pool_id").map(|id| id as Id),
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

    /// The source IPv4 address to use for a server's outbound mail: the
    /// highest-priority (lowest priority number) address in the server's
    /// IP pool, if any.
    pub async fn source_ip_for_server(&self, server_id: Id) -> Option<std::net::IpAddr> {
        let row = self.wait(async {
            sqlx::query(
                "SELECT a.ipv4 FROM ip_addresses a
                 JOIN servers s ON s.ip_pool_id = a.ip_pool_id
                 WHERE s.id = $1
                 ORDER BY a.priority, a.id
                 LIMIT 1",
            )
            .bind(server_id as i64)
            .fetch_optional(&self.pool)
            .await
        })
        .ok()
        .flatten()?;
        row.get::<String, _>("ipv4").parse().ok()
    }

    pub async fn domain_by_id(&self, id: Id) -> Result<Option<Domain>, StoreError> {
        sqlx::query("SELECT * FROM domains WHERE id = $1")
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(domain_from_row))
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
        self.create_route_with_endpoint(server_id, domain_id, name, mode, None)
            .await
    }

    pub async fn create_route_with_endpoint(
        &self,
        server_id: Id,
        domain_id: Option<Id>,
        name: &str,
        mode: RouteMode,
        endpoint_url: Option<String>,
    ) -> Result<Route, StoreError> {
        let uuid = token::generate_uuid();
        let route_token = token::generate_token(8);
        let row = sqlx::query(
            "INSERT INTO routes (uuid, server_id, domain_id, name, token, mode, endpoint_url)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        )
        .bind(&uuid)
        .bind(server_id as i64)
        .bind(domain_id.map(|id| id as i64))
        .bind(name)
        .bind(&route_token)
        .bind(route_mode_to_str(mode))
        .bind(&endpoint_url)
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
            endpoint_url,
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
            ip_pool_id: None,
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

    async fn list_domains(&self, server_id: Id) -> Result<Vec<Domain>, StoreError> {
        sqlx::query(
            "SELECT * FROM domains WHERE owner_type = 'Server' AND owner_id = $1 ORDER BY id",
        )
        .bind(server_id as i64)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.iter().map(domain_from_row).collect())
        .map_err(Self::sqlx_error)
    }

    async fn domain_by_name(
        &self,
        server_id: Id,
        name: &str,
    ) -> Result<Option<Domain>, StoreError> {
        sqlx::query(
            "SELECT * FROM domains WHERE owner_type = 'Server' AND owner_id = $1 AND name = $2",
        )
        .bind(server_id as i64)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.as_ref().map(domain_from_row))
        .map_err(Self::sqlx_error)
    }

    async fn create_server_domain(
        &self,
        server_id: Id,
        name: &str,
    ) -> Result<Domain, StoreError> {
        self.create_domain(DomainOwner::Server(server_id), name, false)
            .await
    }

    async fn set_domain_verified(&self, domain_id: Id, verified: bool) -> Result<(), StoreError> {
        sqlx::query("UPDATE domains SET verified = $2 WHERE id = $1")
            .bind(domain_id as i64)
            .bind(verified)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(Self::sqlx_error)
    }

    async fn delete_domain(&self, domain_id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM domains WHERE id = $1")
            .bind(domain_id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn list_credentials(&self, server_id: Id) -> Result<Vec<Credential>, StoreError> {
        sqlx::query("SELECT * FROM credentials WHERE server_id = $1 ORDER BY id")
            .bind(server_id as i64)
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.iter().map(credential_from_row).collect())
            .map_err(Self::sqlx_error)
    }

    async fn credential_by_id(
        &self,
        server_id: Id,
        id: Id,
    ) -> Result<Option<Credential>, StoreError> {
        sqlx::query("SELECT * FROM credentials WHERE server_id = $1 AND id = $2")
            .bind(server_id as i64)
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(credential_from_row))
            .map_err(Self::sqlx_error)
    }

    async fn create_credential_record(
        &self,
        new: NewCredential,
    ) -> Result<Credential, StoreError> {
        let key = new.key.unwrap_or_else(token::generate_key);
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO credentials (uuid, server_id, type, name, key)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(&uuid)
        .bind(new.server_id as i64)
        .bind(credential_type_to_str(new.credential_type))
        .bind(&new.name)
        .bind(&key)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(Credential {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            server_id: new.server_id,
            credential_type: new.credential_type,
            name: new.name,
            key,
            hold: false,
        })
    }

    async fn update_credential(&self, credential: Credential) -> Result<Credential, StoreError> {
        sqlx::query("UPDATE credentials SET name = $2, hold = $3 WHERE id = $1")
            .bind(credential.id as i64)
            .bind(&credential.name)
            .bind(credential.hold)
            .execute(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(credential)
    }

    async fn delete_credential(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM credentials WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn list_routes(&self, server_id: Id) -> Result<Vec<Route>, StoreError> {
        sqlx::query("SELECT * FROM routes WHERE server_id = $1 ORDER BY id")
            .bind(server_id as i64)
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.iter().map(route_from_row).collect())
            .map_err(Self::sqlx_error)
    }

    async fn route_by_id(&self, server_id: Id, id: Id) -> Result<Option<Route>, StoreError> {
        sqlx::query("SELECT * FROM routes WHERE server_id = $1 AND id = $2")
            .bind(server_id as i64)
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(route_from_row))
            .map_err(Self::sqlx_error)
    }

    async fn create_route_record(&self, new: NewRoute) -> Result<Route, StoreError> {
        self.create_route_with_endpoint(
            new.server_id,
            new.domain_id,
            &new.name,
            new.mode,
            new.endpoint_url,
        )
        .await
    }

    async fn update_route(&self, route: Route) -> Result<Route, StoreError> {
        sqlx::query(
            "UPDATE routes SET name = $2, domain_id = $3, mode = $4, endpoint_url = $5
             WHERE id = $1",
        )
            .bind(route.id as i64)
            .bind(&route.name)
            .bind(route.domain_id.map(|id| id as i64))
            .bind(route_mode_to_str(route.mode))
            .bind(&route.endpoint_url)
            .execute(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(route)
    }

    async fn delete_route(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM routes WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn list_webhooks(&self, server_id: Id) -> Result<Vec<Webhook>, StoreError> {
        sqlx::query("SELECT * FROM webhooks WHERE server_id = $1 ORDER BY id")
            .bind(server_id as i64)
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.iter().map(webhook_from_row).collect())
            .map_err(Self::sqlx_error)
    }

    async fn webhook_by_id(&self, server_id: Id, id: Id) -> Result<Option<Webhook>, StoreError> {
        sqlx::query("SELECT * FROM webhooks WHERE server_id = $1 AND id = $2")
            .bind(server_id as i64)
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(webhook_from_row))
            .map_err(Self::sqlx_error)
    }

    async fn create_webhook(&self, new: NewWebhook) -> Result<Webhook, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO webhooks (uuid, server_id, name, url, all_events, sign)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(&uuid)
        .bind(new.server_id as i64)
        .bind(&new.name)
        .bind(&new.url)
        .bind(new.all_events)
        .bind(new.sign)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(Webhook {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            server_id: new.server_id,
            name: new.name,
            url: new.url,
            all_events: new.all_events,
            enabled: true,
            sign: new.sign,
        })
    }

    async fn update_webhook(&self, webhook: Webhook) -> Result<Webhook, StoreError> {
        sqlx::query(
            "UPDATE webhooks SET name = $2, url = $3, all_events = $4, enabled = $5, sign = $6
             WHERE id = $1",
        )
        .bind(webhook.id as i64)
        .bind(&webhook.name)
        .bind(&webhook.url)
        .bind(webhook.all_events)
        .bind(webhook.enabled)
        .bind(webhook.sign)
        .execute(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(webhook)
    }

    async fn delete_webhook(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM webhooks WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn list_suppressions(&self, server_id: Id) -> Result<Vec<Suppression>, StoreError> {
        // Tenant-scoped table: enter the tenant context; the SELECT itself
        // carries no WHERE server_id — RLS scopes it.
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let rows = sqlx::query("SELECT * FROM suppressions ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(rows.iter().map(suppression_from_row).collect())
    }

    async fn create_suppression(&self, new: NewSuppression) -> Result<Suppression, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, new.server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let row = sqlx::query(
            "INSERT INTO suppressions (server_id, type, address, reason)
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(new.server_id as i64)
        .bind(&new.suppression_type)
        .bind(&new.address)
        .bind(&new.reason)
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(Suppression {
            id: row.get::<i64, _>("id") as Id,
            server_id: new.server_id,
            suppression_type: new.suppression_type,
            address: new.address,
            reason: new.reason,
        })
    }

    async fn delete_suppression(
        &self,
        server_id: Id,
        address: &str,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let result = sqlx::query("DELETE FROM suppressions WHERE address = $1")
            .bind(address)
            .execute(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        sqlx::query("SELECT * FROM users ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.iter().map(user_from_row).collect())
            .map_err(Self::sqlx_error)
    }

    async fn user_by_id(&self, id: Id) -> Result<Option<User>, StoreError> {
        sqlx::query("SELECT * FROM users WHERE id = $1")
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(user_from_row))
            .map_err(Self::sqlx_error)
    }

    async fn create_user(&self, new: NewUser) -> Result<User, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO users (uuid, email_address, first_name, last_name, admin)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(&uuid)
        .bind(&new.email_address)
        .bind(&new.first_name)
        .bind(&new.last_name)
        .bind(new.admin)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(User {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            email_address: new.email_address,
            first_name: new.first_name,
            last_name: new.last_name,
            admin: new.admin,
        })
    }

    async fn update_user(&self, user: User) -> Result<User, StoreError> {
        sqlx::query(
            "UPDATE users SET email_address = $2, first_name = $3, last_name = $4, admin = $5
             WHERE id = $1",
        )
        .bind(user.id as i64)
        .bind(&user.email_address)
        .bind(&user.first_name)
        .bind(&user.last_name)
        .bind(user.admin)
        .execute(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(user)
    }

    async fn delete_user(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn list_ip_pools(&self) -> Result<Vec<IpPool>, StoreError> {
        sqlx::query("SELECT * FROM ip_pools ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.iter().map(ip_pool_from_row).collect())
            .map_err(Self::sqlx_error)
    }

    async fn ip_pool_by_id(&self, id: Id) -> Result<Option<IpPool>, StoreError> {
        sqlx::query("SELECT * FROM ip_pools WHERE id = $1")
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(ip_pool_from_row))
            .map_err(Self::sqlx_error)
    }

    async fn create_ip_pool(&self, name: &str, default: bool) -> Result<IpPool, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO ip_pools (uuid, name, \"default\") VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(&uuid)
        .bind(name)
        .bind(default)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(IpPool {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            name: name.into(),
            default,
        })
    }

    async fn update_ip_pool(&self, pool: IpPool) -> Result<IpPool, StoreError> {
        sqlx::query("UPDATE ip_pools SET name = $2, \"default\" = $3 WHERE id = $1")
            .bind(pool.id as i64)
            .bind(&pool.name)
            .bind(pool.default)
            .execute(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(pool)
    }

    async fn delete_ip_pool(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM ip_pools WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn list_ip_addresses(&self, ip_pool_id: Id) -> Result<Vec<IpAddress>, StoreError> {
        sqlx::query("SELECT * FROM ip_addresses WHERE ip_pool_id = $1 ORDER BY id")
            .bind(ip_pool_id as i64)
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.iter().map(ip_address_from_row).collect())
            .map_err(Self::sqlx_error)
    }

    async fn ip_address_by_id(
        &self,
        ip_pool_id: Id,
        id: Id,
    ) -> Result<Option<IpAddress>, StoreError> {
        sqlx::query("SELECT * FROM ip_addresses WHERE ip_pool_id = $1 AND id = $2")
            .bind(ip_pool_id as i64)
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(ip_address_from_row))
            .map_err(Self::sqlx_error)
    }

    async fn create_ip_address(&self, new: NewIpAddress) -> Result<IpAddress, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO ip_addresses (uuid, ip_pool_id, ipv4, ipv6, hostname, priority)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(&uuid)
        .bind(new.ip_pool_id as i64)
        .bind(&new.ipv4)
        .bind(&new.ipv6)
        .bind(&new.hostname)
        .bind(new.priority)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(IpAddress {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            ip_pool_id: new.ip_pool_id,
            ipv4: new.ipv4,
            ipv6: new.ipv6,
            hostname: new.hostname,
            priority: new.priority,
        })
    }

    async fn delete_ip_address(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM ip_addresses WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn set_server_ip_pool(
        &self,
        server_id: Id,
        ip_pool_id: Option<Id>,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE servers SET ip_pool_id = $2 WHERE id = $1")
            .bind(server_id as i64)
            .bind(ip_pool_id.map(|id| id as i64))
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
#[derive(Debug, Clone, PartialEq)]
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
    pub status: String,
    pub subject: Option<String>,
    pub message_id_header: Option<String>,
    pub spam_status: String,
    pub spam_score: f64,
    pub held: bool,
    pub size: i64,
    pub threat: bool,
    pub threat_details: Option<String>,
    pub inspected: bool,
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
        status: row.get("status"),
        subject: row.get("subject"),
        message_id_header: row.get("message_id_header"),
        spam_status: row.get("spam_status"),
        spam_score: row.get("spam_score"),
        held: row.get("held"),
        size: row.get("size"),
        threat: row.get("threat"),
        threat_details: row.get("threat_details"),
        inspected: row.get("inspected"),
    }
}

/// A delivery attempt record (port of the message DB `deliveries` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub id: i64,
    pub message_id: i64,
    pub status: String,
    pub details: Option<String>,
    pub output: Option<String>,
    pub sent_with_ssl: bool,
}

fn delivery_from_row(row: &PgRow) -> Delivery {
    Delivery {
        id: row.get("id"),
        message_id: row.get("message_id"),
        status: row.get("status"),
        details: row.get("details"),
        output: row.get("output"),
        sent_with_ssl: row.get("sent_with_ssl"),
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

    pub async fn insert_message(&self, message: &QueuedMessage) -> Result<i64, sqlx::Error> {
        // index the interesting headers at insert time, like the Ruby
        // message DB does on save
        let subject = camelmailer_core::message::header_value(&message.raw_message, "subject");
        let message_id_header =
            camelmailer_core::message::header_value(&message.raw_message, "message-id");

        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, message.server_id).await?;
        let row = sqlx::query(
            "INSERT INTO messages
                 (server_id, token, scope, rcpt_to, mail_from, bounce,
                  received_with_ssl, domain_id, credential_id, route_id, raw_message,
                  subject, message_id_header, size)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
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
        .bind(&subject)
        .bind(&message_id_header)
        .bind(message.raw_message.len() as i64)
        .fetch_one(&mut *tx)
        .await?;
        let message_id: i64 = row.get("id");

        // Queue for delivery in the same transaction (the queue table is
        // cross-tenant and not RLS-protected — see migrations/0004_queue.sql).
        let destination_domain = message
            .rcpt_to
            .rsplit_once('@')
            .map(|(_, domain)| domain)
            .unwrap_or_default();
        sqlx::query(
            "INSERT INTO queued_messages (message_id, server_id, domain) VALUES ($1, $2, $3)",
        )
        .bind(message_id)
        .bind(message.server_id as i64)
        .bind(destination_domain)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(message_id)
    }

    /// Load one message within its tenant's RLS context.
    pub async fn message_by_id(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<StoredMessage>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let row = sqlx::query("SELECT * FROM messages WHERE id = $1")
            .bind(message_id)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row.as_ref().map(stored_message_from_row))
    }

    /// Record a delivery attempt and update the message's status,
    /// `last_delivery_attempt` and `held` flag (port of
    /// `Postal::MessageDB::Message#create_delivery`).
    pub async fn record_delivery(
        &self,
        server_id: Id,
        message_id: i64,
        status: &str,
        details: &str,
        output: &str,
        sent_with_ssl: bool,
    ) -> Result<i64, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let row = sqlx::query(
            "INSERT INTO deliveries (server_id, message_id, status, details, output, sent_with_ssl)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(server_id as i64)
        .bind(message_id)
        .bind(status)
        .bind(details)
        .bind(output)
        .bind(sent_with_ssl)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE messages
             SET status = $2, held = ($2 = 'Held'), last_delivery_attempt = now()
             WHERE id = $1",
        )
        .bind(message_id)
        .bind(status)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.get("id"))
    }

    pub async fn deliveries_for_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<Delivery>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let rows = sqlx::query("SELECT * FROM deliveries WHERE message_id = $1 ORDER BY id")
            .bind(message_id)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rows.iter().map(delivery_from_row).collect())
    }

    /// Store a spam-check result (port of `update_spam` on the message).
    pub async fn set_spam_result(
        &self,
        server_id: Id,
        message_id: i64,
        spam_status: &str,
        spam_score: f64,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        sqlx::query("UPDATE messages SET spam_status = $2, spam_score = $3 WHERE id = $1")
            .bind(message_id)
            .bind(spam_status)
            .bind(spam_score)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Store the result of inspecting an incoming message (rspamd spam
    /// score + status, ClamAV threat verdict). Sets `inspected = true`.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_inspection(
        &self,
        server_id: Id,
        message_id: i64,
        spam_status: &str,
        spam_score: f64,
        threat: bool,
        threat_details: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        sqlx::query(
            "UPDATE messages
             SET spam_status = $2, spam_score = $3, threat = $4, threat_details = $5,
                 inspected = true
             WHERE id = $1",
        )
        .bind(message_id)
        .bind(spam_status)
        .bind(spam_score)
        .bind(threat)
        .bind(threat_details)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Register a trackable link in a message; returns (link id, token).
    pub async fn create_link(
        &self,
        server_id: Id,
        message_id: i64,
        url: &str,
    ) -> Result<(i64, String), sqlx::Error> {
        let link_token = token::generate_token(16);
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let row = sqlx::query(
            "INSERT INTO links (server_id, message_id, token, url)
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(server_id as i64)
        .bind(message_id)
        .bind(&link_token)
        .bind(url)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((row.get("id"), link_token))
    }

    /// Record a click on a tracked link.
    pub async fn record_link_click(
        &self,
        server_id: Id,
        link_id: i64,
        ip_address: &str,
        user_agent: &str,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        sqlx::query(
            "INSERT INTO link_clicks (server_id, link_id, ip_address, user_agent)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(server_id as i64)
        .bind(link_id)
        .bind(ip_address)
        .bind(user_agent)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Record an open (tracking-pixel load).
    pub async fn record_load(
        &self,
        server_id: Id,
        message_id: i64,
        ip_address: &str,
        user_agent: &str,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        sqlx::query(
            "INSERT INTO loads (server_id, message_id, ip_address, user_agent)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(server_id as i64)
        .bind(message_id)
        .bind(ip_address)
        .bind(user_agent)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Clicks (per link) and opens for a message: (clicks, opens).
    pub async fn activity_counts(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<(i64, i64), sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let clicks: i64 = sqlx::query(
            "SELECT count(*) AS c FROM link_clicks lc
             JOIN links l ON l.id = lc.link_id
             WHERE l.message_id = $1",
        )
        .bind(message_id)
        .fetch_one(&mut *tx)
        .await?
        .get("c");
        let opens: i64 = sqlx::query("SELECT count(*) AS c FROM loads WHERE message_id = $1")
            .bind(message_id)
            .fetch_one(&mut *tx)
            .await?
            .get("c");
        tx.commit().await?;
        Ok((clicks, opens))
    }

    /// Is this address on the tenant's suppression list?
    pub async fn address_suppressed(
        &self,
        server_id: Id,
        address: &str,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let row = sqlx::query("SELECT count(*) AS c FROM suppressions WHERE address = $1")
            .bind(address)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row.get::<i64, _>("c") > 0)
    }

    /// List a tenant's messages. The query carries no `WHERE server_id`
    /// filter — row-level security scopes the result to the tenant context.
    pub async fn messages_for_server(
        &self,
        server_id: Id,
    ) -> Result<Vec<StoredMessage>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
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
