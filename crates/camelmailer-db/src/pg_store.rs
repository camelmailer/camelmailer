//! The PostgreSQL implementations of the storage traits.
//!
//! [`PgStore`] implements both the async [`AdminStore`] (Admin API) and the
//! sync [`Store`] used by the SMTP session state machine. The sync methods
//! bridge onto the async pool via `block_in_place`, so they must run on a
//! multi-threaded tokio runtime (which the SMTP server does).

use async_trait::async_trait;
use camelmailer_core::{
    store, token, ActivityEvent, AdminApiKey, AdminStore, Campaign, CampaignStats, Credential,
    CredentialType, DeliveryRecord, Domain, DomainOwner, Id, IpAddress, IpPool, MessageFilter,
    MessageRecord, MessageScope, MessageSink, NewCampaign, NewCredential, NewIpAddress,
    NewOrganization, NewRoute, NewSenderAddress, NewServer, NewSuppression, NewUser, NewWebhook,
    Organization, QueuedMessage, ResolvedRoute, Route, RouteMode, SenderAddress, Server,
    ServerMode, Store, StoreError, Subscription, Suppression, User, Webhook,
};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, QueryBuilder, Row};
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
        require_two_factor: row.get("require_two_factor"),
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
        track_opens: row.get("track_opens"),
        track_clicks: row.get("track_clicks"),
        spam_threshold: row.get("spam_threshold"),
        outbound_spam_threshold: row.get("outbound_spam_threshold"),
        bounce_hook_url: row.get("bounce_hook_url"),
        delivery_hook_url: row.get("delivery_hook_url"),
        inbound_domain: row.get("inbound_domain"),
        broadcast_physical_address: row.get("broadcast_physical_address"),
        color: row.get("color"),
        default_stream_id: row
            .get::<Option<i64>, _>("default_stream_id")
            .map(|id| id as Id),
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
        last_used_at: row.get("last_used_at"),
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
        verification_token: row.get("verification_token"),
        dkim_private_key: row.get("dkim_private_key"),
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
        events: serde_json::from_value(row.get::<serde_json::Value, _>("events"))
            .unwrap_or_default(),
        headers: serde_json::from_value(row.get::<serde_json::Value, _>("headers"))
            .unwrap_or_default(),
    }
}

fn sender_address_from_row(row: &PgRow) -> SenderAddress {
    SenderAddress {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        server_id: row.get::<i64, _>("server_id") as Id,
        email_address: row.get("email_address"),
        verified: row.get("verified"),
        verification_token_hash: row.get("verification_token_hash"),
    }
}

fn suppression_from_row(row: &PgRow) -> Suppression {
    Suppression {
        id: row.get::<i64, _>("id") as Id,
        server_id: row.get::<i64, _>("server_id") as Id,
        suppression_type: row.get("type"),
        address: row.get("address"),
        reason: row.get("reason"),
        stream_id: row.get::<Option<i64>, _>("stream_id").map(|id| id as Id),
    }
}

fn subscription_from_row(row: &PgRow) -> Subscription {
    Subscription {
        id: row.get::<i64, _>("id") as Id,
        server_id: row.get::<i64, _>("server_id") as Id,
        stream_id: row.get::<i64, _>("stream_id") as Id,
        address: row.get("address"),
        status: row.get("status"),
        created_at: row.get("created_at"),
    }
}

fn campaign_from_row(row: &PgRow) -> Campaign {
    Campaign {
        id: row.get::<i64, _>("id") as Id,
        server_id: row.get::<i64, _>("server_id") as Id,
        stream_id: row.get::<i64, _>("stream_id") as Id,
        name: row.get("name"),
        subject: row.get("subject"),
        from_address: row.get("from_address"),
        html_body: row.get("html_body"),
        text_body: row.get("text_body"),
        status: row.get("status"),
        total: row.get::<i32, _>("total") as i64,
        sent: row.get::<i32, _>("sent") as i64,
        created_at: row.get("created_at"),
        completed_at: row.get("completed_at"),
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
           s.ip_pool_id AS s_ip_pool_id, s.track_opens, s.track_clicks,
           s.spam_threshold, s.outbound_spam_threshold, s.bounce_hook_url,
           s.delivery_hook_url, s.inbound_domain, s.broadcast_physical_address,
           s.color, s.default_stream_id
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
            track_opens: row.get("track_opens"),
            track_clicks: row.get("track_clicks"),
            spam_threshold: row.get("spam_threshold"),
            outbound_spam_threshold: row.get("outbound_spam_threshold"),
            bounce_hook_url: row.get("bounce_hook_url"),
            delivery_hook_url: row.get("delivery_hook_url"),
            inbound_domain: row.get("inbound_domain"),
            broadcast_physical_address: row.get("broadcast_physical_address"),
            color: row.get("color"),
            default_stream_id: row
                .get::<Option<i64>, _>("default_stream_id")
                .map(|id| id as Id),
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
        let row = self
            .wait(async {
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
        dkim_private_key: Option<&str>,
    ) -> Result<Domain, StoreError> {
        let (owner_type, owner_id) = match owner {
            DomainOwner::Organization(id) => ("Organization", id as i64),
            DomainOwner::Server(id) => ("Server", id as i64),
        };
        let uuid = token::generate_uuid();
        let verification_token = token::generate_token(32);
        let row = sqlx::query(
            "INSERT INTO domains
                 (uuid, owner_type, owner_id, name, verified,
                  verification_token, dkim_private_key)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        )
        .bind(&uuid)
        .bind(owner_type)
        .bind(owner_id)
        .bind(name)
        .bind(verified)
        .bind(&verification_token)
        .bind(dkim_private_key)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(Domain {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            owner,
            name: name.into(),
            verified,
            verification_token,
            dkim_private_key: dkim_private_key.map(str::to_string),
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
            last_used_at: None,
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

    async fn create_organization(&self, new: NewOrganization) -> Result<Organization, StoreError> {
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
            require_two_factor: false,
        })
    }

    async fn update_organization(
        &self,
        organization: Organization,
    ) -> Result<Organization, StoreError> {
        sqlx::query(
            "UPDATE organizations SET name = $2, permalink = $3, require_two_factor = $4
             WHERE id = $1",
        )
        .bind(organization.id as i64)
        .bind(&organization.name)
        .bind(&organization.permalink)
        .bind(organization.require_two_factor)
        .execute(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(organization)
    }

    async fn organization_billing_customer_id(
        &self,
        organization_id: Id,
    ) -> Result<Option<String>, StoreError> {
        sqlx::query("SELECT billing_customer_id FROM organizations WHERE id = $1")
            .bind(organization_id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.and_then(|row| row.get::<Option<String>, _>("billing_customer_id")))
            .map_err(Self::sqlx_error)
    }

    async fn set_organization_billing_customer_id(
        &self,
        organization_id: Id,
        customer_id: &str,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE organizations SET billing_customer_id = $2 WHERE id = $1")
            .bind(organization_id as i64)
            .bind(customer_id)
            .execute(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        if result.rows_affected() == 0 {
            return Err(StoreError::Other(format!(
                "organization {organization_id} not found"
            )));
        }
        Ok(())
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
        let server_id = row.get::<i64, _>("id") as Id;

        // Give every new server a built-in transactional stream (parity with
        // the 0012 migration's backfill) and point default_stream_id at it.
        let default_stream_id = sqlx::query(
            "INSERT INTO message_streams (uuid, server_id, name, permalink, stream_type)
             VALUES ($1, $2, 'Default Transactional Stream', 'outbound', 'transactional')
             RETURNING id",
        )
        .bind(token::generate_uuid())
        .bind(server_id as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?
        .get::<i64, _>("id") as Id;
        sqlx::query("UPDATE servers SET default_stream_id = $2 WHERE id = $1")
            .bind(server_id as i64)
            .bind(default_stream_id as i64)
            .execute(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;

        Ok(Server {
            id: server_id,
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
            track_opens: false,
            track_clicks: false,
            spam_threshold: None,
            outbound_spam_threshold: None,
            bounce_hook_url: None,
            delivery_hook_url: None,
            inbound_domain: None,
            broadcast_physical_address: None,
            color: None,
            default_stream_id: Some(default_stream_id),
        })
    }

    async fn update_server(&self, server: Server) -> Result<Server, StoreError> {
        sqlx::query(
            "UPDATE servers SET name = $2, suspended = $3, suspension_reason = $4,
                    privacy_mode = $5, log_smtp_data = $6, allow_sender = $7,
                    mode = $8, track_opens = $9, track_clicks = $10,
                    spam_threshold = $11, outbound_spam_threshold = $12,
                    bounce_hook_url = $13, delivery_hook_url = $14,
                    inbound_domain = $15, broadcast_physical_address = $16,
                    color = $17, default_stream_id = $18
             WHERE id = $1",
        )
        .bind(server.id as i64)
        .bind(&server.name)
        .bind(server.suspended)
        .bind(&server.suspension_reason)
        .bind(server.privacy_mode)
        .bind(server.log_smtp_data)
        .bind(server.allow_sender)
        .bind(match server.mode {
            ServerMode::Live => "Live",
            ServerMode::Development => "Development",
        })
        .bind(server.track_opens)
        .bind(server.track_clicks)
        .bind(server.spam_threshold)
        .bind(server.outbound_spam_threshold)
        .bind(&server.bounce_hook_url)
        .bind(&server.delivery_hook_url)
        .bind(&server.inbound_domain)
        .bind(&server.broadcast_physical_address)
        .bind(&server.color)
        .bind(server.default_stream_id.map(|id| id as i64))
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
        self.create_admin_api_key_record(name, key)
            .await
            .map(|_| ())
    }

    async fn server_for_api_token(&self, key: &str) -> Result<Option<Server>, StoreError> {
        let server = sqlx::query(
            "SELECT s.* FROM credentials c
             JOIN servers s ON s.id = c.server_id
             WHERE c.type = 'API' AND c.key = $1 AND NOT c.hold",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(Self::sqlx_error)?
        .as_ref()
        .map(server_from_row);
        if server.is_some() {
            let _ = sqlx::query("UPDATE credentials SET last_used_at = now() WHERE key = $1")
                .bind(key)
                .execute(&self.pool)
                .await;
        }
        Ok(server)
    }

    async fn authenticated_domain(
        &self,
        server_id: Id,
        domain_name: &str,
    ) -> Result<Option<Id>, StoreError> {
        sqlx::query(
            "SELECT d.id FROM domains d
             JOIN servers s ON s.id = $1
             WHERE d.verified AND d.name = $2
               AND ((d.owner_type = 'Server' AND d.owner_id = s.id)
                 OR (d.owner_type = 'Organization' AND d.owner_id = s.organization_id))
             LIMIT 1",
        )
        .bind(server_id as i64)
        .bind(domain_name)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(|row| row.get::<i64, _>("id") as Id))
        .map_err(Self::sqlx_error)
    }

    async fn list_admin_api_keys(&self) -> Result<Vec<AdminApiKey>, StoreError> {
        sqlx::query("SELECT id, uuid, name, key FROM admin_api_keys ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.iter()
                    .map(|row| AdminApiKey {
                        id: row.get::<i64, _>("id") as Id,
                        uuid: row.get("uuid"),
                        name: row.get("name"),
                        key_prefix: row.get::<String, _>("key").chars().take(6).collect(),
                    })
                    .collect()
            })
            .map_err(Self::sqlx_error)
    }

    async fn create_admin_api_key_record(
        &self,
        name: &str,
        key: &str,
    ) -> Result<AdminApiKey, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO admin_api_keys (uuid, name, key) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(&uuid)
        .bind(name)
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(AdminApiKey {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            name: name.into(),
            key_prefix: key.chars().take(6).collect(),
        })
    }

    async fn delete_admin_api_key(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM admin_api_keys WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
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
        dkim_private_key: Option<String>,
    ) -> Result<Domain, StoreError> {
        self.create_domain(
            DomainOwner::Server(server_id),
            name,
            false,
            dkim_private_key.as_deref(),
        )
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

    async fn create_credential_record(&self, new: NewCredential) -> Result<Credential, StoreError> {
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
            last_used_at: None,
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
        let events = serde_json::to_value(&new.events).unwrap_or_default();
        let headers = serde_json::to_value(&new.headers).unwrap_or_default();
        let row = sqlx::query(
            "INSERT INTO webhooks (uuid, server_id, name, url, all_events, sign, events, headers)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(&uuid)
        .bind(new.server_id as i64)
        .bind(&new.name)
        .bind(&new.url)
        .bind(new.all_events)
        .bind(new.sign)
        .bind(&events)
        .bind(&headers)
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
            events: new.events,
            headers: new.headers,
        })
    }

    async fn update_webhook(&self, webhook: Webhook) -> Result<Webhook, StoreError> {
        let events = serde_json::to_value(&webhook.events).unwrap_or_default();
        let headers = serde_json::to_value(&webhook.headers).unwrap_or_default();
        sqlx::query(
            "UPDATE webhooks SET name = $2, url = $3, all_events = $4, enabled = $5, sign = $6,
                 events = $7, headers = $8
             WHERE id = $1",
        )
        .bind(webhook.id as i64)
        .bind(&webhook.name)
        .bind(&webhook.url)
        .bind(webhook.all_events)
        .bind(webhook.enabled)
        .bind(webhook.sign)
        .bind(&events)
        .bind(&headers)
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

    async fn list_sender_addresses(&self, server_id: Id) -> Result<Vec<SenderAddress>, StoreError> {
        sqlx::query("SELECT * FROM sender_addresses WHERE server_id = $1 ORDER BY id")
            .bind(server_id as i64)
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.iter().map(sender_address_from_row).collect())
            .map_err(Self::sqlx_error)
    }

    async fn sender_address_by_id(
        &self,
        server_id: Id,
        id: Id,
    ) -> Result<Option<SenderAddress>, StoreError> {
        sqlx::query("SELECT * FROM sender_addresses WHERE server_id = $1 AND id = $2")
            .bind(server_id as i64)
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.as_ref().map(sender_address_from_row))
            .map_err(Self::sqlx_error)
    }

    async fn create_sender_address(
        &self,
        new: NewSenderAddress,
    ) -> Result<SenderAddress, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO sender_addresses (uuid, server_id, email_address, verification_token_hash)
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(&uuid)
        .bind(new.server_id as i64)
        .bind(&new.email_address)
        .bind(&new.verification_token_hash)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| {
            if let sqlx::Error::Database(db_error) = &error {
                if db_error.code().as_deref() == Some("23505") {
                    return StoreError::Conflict("Email address has already been added".into());
                }
            }
            Self::sqlx_error(error)
        })?;
        Ok(SenderAddress {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            server_id: new.server_id,
            email_address: new.email_address,
            verified: false,
            verification_token_hash: Some(new.verification_token_hash),
        })
    }

    async fn confirm_sender_address(
        &self,
        token_hash: &str,
    ) -> Result<Option<SenderAddress>, StoreError> {
        sqlx::query(
            "UPDATE sender_addresses
             SET verified = TRUE, verification_token_hash = NULL
             WHERE verification_token_hash = $1 AND NOT verified
             RETURNING *",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.as_ref().map(sender_address_from_row))
        .map_err(Self::sqlx_error)
    }

    async fn delete_sender_address(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM sender_addresses WHERE id = $1")
            .bind(id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    async fn confirmed_sender_address(
        &self,
        server_id: Id,
        email: &str,
    ) -> Result<bool, StoreError> {
        sqlx::query(
            "SELECT 1 AS one FROM sender_addresses
             WHERE server_id = $1 AND verified AND LOWER(email_address) = LOWER($2)
             LIMIT 1",
        )
        .bind(server_id as i64)
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.is_some())
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
            "INSERT INTO suppressions (server_id, type, address, reason, stream_id)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(new.server_id as i64)
        .bind(&new.suppression_type)
        .bind(&new.address)
        .bind(&new.reason)
        .bind(new.stream_id.map(|id| id as i64))
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
            stream_id: new.stream_id,
        })
    }

    async fn delete_suppression(&self, server_id: Id, address: &str) -> Result<bool, StoreError> {
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

#[async_trait]
impl camelmailer_core::ServerStore for PgStore {
    async fn store_outgoing(
        &self,
        message: QueuedMessage,
    ) -> Result<camelmailer_core::SentMessage, StoreError> {
        let rcpt_to = message.rcpt_to.clone();
        let sink = PgMessageSink::new(self.clone());
        let (id, token) = sink
            .insert_message_returning(&message)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(camelmailer_core::SentMessage { id, token, rcpt_to })
    }

    async fn messages(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, StoreError> {
        PgMessageSink::new(self.clone())
            .message_records(server_id, filter)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        PgMessageSink::new(self.clone())
            .message_record(server_id, message_id)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn deliveries(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<DeliveryRecord>, StoreError> {
        // A message id from another tenant returns no rows under RLS, so
        // there is no cross-tenant leak even without an existence check.
        PgMessageSink::new(self.clone())
            .delivery_records(server_id, message_id)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn opens(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<ActivityEvent>, StoreError> {
        PgMessageSink::new(self.clone())
            .opens_for_message(server_id, message_id)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn clicks(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<ActivityEvent>, StoreError> {
        PgMessageSink::new(self.clone())
            .clicks_for_message(server_id, message_id)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn message_stats(
        &self,
        server_id: Id,
        filter: &camelmailer_core::StatsFilter,
    ) -> Result<camelmailer_core::MessageStats, StoreError> {
        PgMessageSink::new(self.clone())
            .message_stats(server_id, filter)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn delivery_stats(
        &self,
        server_id: Id,
    ) -> Result<camelmailer_core::DeliveryStats, StoreError> {
        PgMessageSink::new(self.clone())
            .delivery_stats(server_id)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn bounces(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, StoreError> {
        PgMessageSink::new(self.clone())
            .bounce_records(server_id, filter)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn bounce(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        Ok(PgMessageSink::new(self.clone())
            .message_record(server_id, message_id)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))?
            .filter(|m| m.bounce || m.status == "Bounced"))
    }

    async fn list_streams(
        &self,
        server_id: Id,
    ) -> Result<Vec<camelmailer_core::MessageStream>, StoreError> {
        let rows = sqlx::query("SELECT * FROM message_streams WHERE server_id = $1 ORDER BY id")
            .bind(server_id as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(rows.iter().map(message_stream_from_row).collect())
    }

    async fn stream_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<camelmailer_core::MessageStream>, StoreError> {
        let row =
            sqlx::query("SELECT * FROM message_streams WHERE server_id = $1 AND permalink = $2")
                .bind(server_id as i64)
                .bind(permalink)
                .fetch_optional(&self.pool)
                .await
                .map_err(Self::sqlx_error)?;
        Ok(row.as_ref().map(message_stream_from_row))
    }

    async fn create_stream(
        &self,
        new: camelmailer_core::NewStream,
    ) -> Result<camelmailer_core::MessageStream, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO message_streams (uuid, server_id, name, permalink, stream_type, ip_pool_id)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(&uuid)
        .bind(new.server_id as i64)
        .bind(&new.name)
        .bind(&new.permalink)
        .bind(&new.stream_type)
        .bind(new.ip_pool_id.map(|id| id as i64))
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(camelmailer_core::MessageStream {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            server_id: new.server_id,
            name: new.name,
            permalink: new.permalink,
            stream_type: new.stream_type,
            archived: false,
            ip_pool_id: new.ip_pool_id,
        })
    }

    async fn update_stream(
        &self,
        stream: camelmailer_core::MessageStream,
    ) -> Result<camelmailer_core::MessageStream, StoreError> {
        sqlx::query(
            "UPDATE message_streams
             SET name = $2, stream_type = $3, archived = $4, ip_pool_id = $5 WHERE id = $1",
        )
        .bind(stream.id as i64)
        .bind(&stream.name)
        .bind(&stream.stream_type)
        .bind(stream.archived)
        .bind(stream.ip_pool_id.map(|id| id as i64))
        .execute(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(stream)
    }

    async fn source_ip_for(
        &self,
        server_id: Id,
        stream_id: Option<Id>,
    ) -> Result<Option<String>, StoreError> {
        // COALESCE resolves the stream's pool first, then the server's — so a
        // NULL stream_id, an unknown stream, or a stream without a pool all
        // fall through to the server pool, matching source_ip_for_server
        // exactly (no regression for transactional streams).
        let row = sqlx::query(
            "SELECT a.ipv4 FROM ip_addresses a
             WHERE a.ip_pool_id = COALESCE(
                 (SELECT ms.ip_pool_id FROM message_streams ms
                  WHERE ms.id = $2 AND ms.server_id = $1),
                 (SELECT s.ip_pool_id FROM servers s WHERE s.id = $1))
             ORDER BY a.priority, a.id
             LIMIT 1",
        )
        .bind(server_id as i64)
        .bind(stream_id.map(|id| id as i64))
        .fetch_optional(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(row.map(|row| row.get::<String, _>("ipv4")))
    }

    async fn inbound_messages(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, StoreError> {
        let filter = MessageFilter {
            scope: Some("incoming".into()),
            ..filter.clone()
        };
        PgMessageSink::new(self.clone())
            .message_records(server_id, &filter)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn inbound_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        Ok(PgMessageSink::new(self.clone())
            .message_record(server_id, message_id)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))?
            .filter(|m| m.scope == "incoming"))
    }

    async fn bypass_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        PgMessageSink::new(self.clone())
            .requeue_inbound(server_id, message_id, true)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn retry_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        PgMessageSink::new(self.clone())
            .requeue_inbound(server_id, message_id, false)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    async fn list_templates(
        &self,
        server_id: Id,
    ) -> Result<Vec<camelmailer_core::Template>, StoreError> {
        let rows = sqlx::query("SELECT * FROM templates WHERE server_id = $1 ORDER BY id")
            .bind(server_id as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(rows.iter().map(template_from_row).collect())
    }

    async fn template_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<camelmailer_core::Template>, StoreError> {
        let row = sqlx::query("SELECT * FROM templates WHERE server_id = $1 AND permalink = $2")
            .bind(server_id as i64)
            .bind(permalink)
            .fetch_optional(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(row.as_ref().map(template_from_row))
    }

    async fn create_template(
        &self,
        new: camelmailer_core::NewTemplate,
    ) -> Result<camelmailer_core::Template, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO templates (uuid, server_id, name, permalink, subject, html_body, text_body, layout_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(&uuid)
        .bind(new.server_id as i64)
        .bind(&new.name)
        .bind(&new.permalink)
        .bind(&new.subject)
        .bind(&new.html_body)
        .bind(&new.text_body)
        .bind(new.layout_id.map(|id| id as i64))
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(camelmailer_core::Template {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            server_id: new.server_id,
            name: new.name,
            permalink: new.permalink,
            subject: new.subject,
            html_body: new.html_body,
            text_body: new.text_body,
            archived: false,
            layout_id: new.layout_id,
        })
    }

    async fn update_template(
        &self,
        template: camelmailer_core::Template,
    ) -> Result<camelmailer_core::Template, StoreError> {
        sqlx::query(
            "UPDATE templates
             SET name = $2, subject = $3, html_body = $4, text_body = $5, archived = $6,
                 layout_id = $7
             WHERE id = $1",
        )
        .bind(template.id as i64)
        .bind(&template.name)
        .bind(&template.subject)
        .bind(&template.html_body)
        .bind(&template.text_body)
        .bind(template.archived)
        .bind(template.layout_id.map(|id| id as i64))
        .execute(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(template)
    }

    async fn list_layouts(
        &self,
        server_id: Id,
    ) -> Result<Vec<camelmailer_core::Layout>, StoreError> {
        let rows = sqlx::query("SELECT * FROM layouts WHERE server_id = $1 ORDER BY id")
            .bind(server_id as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(rows.iter().map(layout_from_row).collect())
    }

    async fn layout_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<camelmailer_core::Layout>, StoreError> {
        let row = sqlx::query("SELECT * FROM layouts WHERE server_id = $1 AND permalink = $2")
            .bind(server_id as i64)
            .bind(permalink)
            .fetch_optional(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(row.as_ref().map(layout_from_row))
    }

    async fn layout_by_id(
        &self,
        server_id: Id,
        layout_id: Id,
    ) -> Result<Option<camelmailer_core::Layout>, StoreError> {
        let row = sqlx::query("SELECT * FROM layouts WHERE server_id = $1 AND id = $2")
            .bind(server_id as i64)
            .bind(layout_id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(row.as_ref().map(layout_from_row))
    }

    async fn create_layout(
        &self,
        new: camelmailer_core::NewLayout,
    ) -> Result<camelmailer_core::Layout, StoreError> {
        let uuid = token::generate_uuid();
        let row = sqlx::query(
            "INSERT INTO layouts (uuid, server_id, name, permalink, html_wrapper, text_wrapper)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(&uuid)
        .bind(new.server_id as i64)
        .bind(&new.name)
        .bind(&new.permalink)
        .bind(&new.html_wrapper)
        .bind(&new.text_wrapper)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(camelmailer_core::Layout {
            id: row.get::<i64, _>("id") as Id,
            uuid,
            server_id: new.server_id,
            name: new.name,
            permalink: new.permalink,
            html_wrapper: new.html_wrapper,
            text_wrapper: new.text_wrapper,
        })
    }

    async fn update_layout(
        &self,
        layout: camelmailer_core::Layout,
    ) -> Result<camelmailer_core::Layout, StoreError> {
        sqlx::query(
            "UPDATE layouts SET name = $2, html_wrapper = $3, text_wrapper = $4 WHERE id = $1",
        )
        .bind(layout.id as i64)
        .bind(&layout.name)
        .bind(&layout.html_wrapper)
        .bind(&layout.text_wrapper)
        .execute(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(layout)
    }

    async fn delete_layout(&self, server_id: Id, layout_id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM layouts WHERE id = $1 AND server_id = $2")
            .bind(layout_id as i64)
            .bind(server_id as i64)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(Self::sqlx_error)
    }

    // DMARC aggregate reports — tenant tables: every query runs inside a
    // transaction that enters the tenant context first; RLS scopes the rows.

    async fn store_dmarc_report(
        &self,
        new: camelmailer_core::NewDmarcReport,
    ) -> Result<camelmailer_core::DmarcReport, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, new.server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let row = sqlx::query(
            "INSERT INTO dmarc_reports
                 (server_id, domain, org_name, org_email, report_id,
                  date_range_begin, date_range_end)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id, received_at",
        )
        .bind(new.server_id as i64)
        .bind(&new.domain)
        .bind(&new.org_name)
        .bind(&new.org_email)
        .bind(&new.report_id)
        .bind(new.date_range_begin)
        .bind(new.date_range_end)
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        let id: i64 = row.get("id");
        let received_at: chrono::DateTime<chrono::Utc> = row.get("received_at");
        for record in &new.records {
            sqlx::query(
                "INSERT INTO dmarc_report_records
                     (server_id, report_id, source_ip, count, disposition,
                      dkim_result, spf_result, dkim_aligned, spf_aligned,
                      header_from, envelope_from)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(new.server_id as i64)
            .bind(id)
            .bind(&record.source_ip)
            .bind(record.count)
            .bind(&record.disposition)
            .bind(&record.dkim_result)
            .bind(&record.spf_result)
            .bind(record.dkim_aligned)
            .bind(record.spf_aligned)
            .bind(&record.header_from)
            .bind(&record.envelope_from)
            .execute(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        }
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(camelmailer_core::DmarcReport {
            id,
            server_id: new.server_id,
            domain: new.domain,
            org_name: new.org_name,
            org_email: new.org_email,
            report_id: new.report_id,
            date_range_begin: new.date_range_begin,
            date_range_end: new.date_range_end,
            received_at,
            record_count: new.records.len() as i64,
        })
    }

    async fn dmarc_reports(
        &self,
        server_id: Id,
        filter: &camelmailer_core::DmarcFilter,
    ) -> Result<Vec<camelmailer_core::DmarcReport>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        // No WHERE server_id — RLS scopes the rows to the tenant context.
        let rows = sqlx::query(
            "SELECT r.*, (SELECT COUNT(*) FROM dmarc_report_records d
                          WHERE d.report_id = r.id) AS record_count
             FROM dmarc_reports r
             WHERE ($1::text IS NULL OR lower(r.domain) = lower($1))
               AND ($2::timestamptz IS NULL OR r.date_range_end >= $2)
               AND ($3::timestamptz IS NULL OR r.date_range_begin <= $3)
             ORDER BY r.date_range_begin DESC, r.id DESC",
        )
        .bind(&filter.domain)
        .bind(filter.from)
        .bind(filter.to)
        .fetch_all(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(rows.iter().map(dmarc_report_from_row).collect())
    }

    async fn dmarc_report(
        &self,
        server_id: Id,
        report_id: i64,
    ) -> Result<
        Option<(
            camelmailer_core::DmarcReport,
            Vec<camelmailer_core::DmarcRecordRow>,
        )>,
        StoreError,
    > {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let row = sqlx::query(
            "SELECT r.*, (SELECT COUNT(*) FROM dmarc_report_records d
                          WHERE d.report_id = r.id) AS record_count
             FROM dmarc_reports r WHERE r.id = $1",
        )
        .bind(report_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        let Some(row) = row else {
            tx.commit().await.map_err(Self::sqlx_error)?;
            return Ok(None);
        };
        let report = dmarc_report_from_row(&row);
        let records =
            sqlx::query("SELECT * FROM dmarc_report_records WHERE report_id = $1 ORDER BY id")
                .bind(report_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(Some((
            report,
            records.iter().map(dmarc_record_from_row).collect(),
        )))
    }

    async fn dmarc_records(
        &self,
        server_id: Id,
        filter: &camelmailer_core::DmarcFilter,
    ) -> Result<Vec<camelmailer_core::DmarcRecordRow>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let rows = sqlx::query(
            "SELECT d.* FROM dmarc_report_records d
             JOIN dmarc_reports r ON r.id = d.report_id
             WHERE ($1::text IS NULL OR lower(r.domain) = lower($1))
               AND ($2::timestamptz IS NULL OR r.date_range_end >= $2)
               AND ($3::timestamptz IS NULL OR r.date_range_begin <= $3)
             ORDER BY d.id",
        )
        .bind(&filter.domain)
        .bind(filter.from)
        .bind(filter.to)
        .fetch_all(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(rows.iter().map(dmarc_record_from_row).collect())
    }

    // Message share links — a cross-tenant lookup table (like
    // tracking_tokens): only the token hash is stored, and resolution by
    // hash deliberately runs without a tenant context.

    async fn create_message_share(
        &self,
        new: camelmailer_core::NewMessageShare,
    ) -> Result<camelmailer_core::MessageShare, StoreError> {
        let row = sqlx::query(
            "INSERT INTO message_shares (server_id, message_id, token_hash, expires_at)
             VALUES ($1, $2, $3, $4) RETURNING id, created_at",
        )
        .bind(new.server_id as i64)
        .bind(new.message_id)
        .bind(&new.token_hash)
        .bind(new.expires_at)
        .fetch_one(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(camelmailer_core::MessageShare {
            id: row.get("id"),
            server_id: new.server_id,
            message_id: new.message_id,
            token_hash: new.token_hash,
            expires_at: new.expires_at,
            created_at: row.get("created_at"),
        })
    }

    async fn message_share_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<camelmailer_core::MessageShare>, StoreError> {
        let row = sqlx::query("SELECT * FROM message_shares WHERE token_hash = $1")
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await
            .map_err(Self::sqlx_error)?;
        Ok(row.map(|row| camelmailer_core::MessageShare {
            id: row.get("id"),
            server_id: row.get::<i64, _>("server_id") as Id,
            message_id: row.get("message_id"),
            token_hash: row.get("token_hash"),
            expires_at: row.get("expires_at"),
            created_at: row.get("created_at"),
        }))
    }

    async fn address_suppressed(
        &self,
        server_id: Id,
        address: &str,
        stream_id: Option<Id>,
    ) -> Result<bool, StoreError> {
        // Tenant-scoped table: enter the RLS context (no WHERE server_id).
        // Suppressed when a server-wide (stream_id IS NULL) or matching
        // stream-scoped row exists for the address.
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let row = sqlx::query(
            "SELECT count(*) AS c FROM suppressions
             WHERE address = $1 AND (stream_id IS NULL OR stream_id = $2)",
        )
        .bind(address)
        .bind(stream_id.map(|id| id as i64))
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(row.get::<i64, _>("c") > 0)
    }

    async fn create_unsubscribe_token(
        &self,
        server_id: Id,
        stream_id: Option<Id>,
        address: &str,
    ) -> Result<String, StoreError> {
        // Cross-tenant lookup table (resolved by token alone); no RLS.
        let token = token::generate_token(32);
        sqlx::query(
            "INSERT INTO unsubscribe_tokens (server_id, stream_id, address, token)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(server_id as i64)
        .bind(stream_id.map(|id| id as i64))
        .bind(address)
        .bind(&token)
        .execute(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(token)
    }

    async fn resolve_unsubscribe_token(
        &self,
        token: &str,
    ) -> Result<Option<(Id, Option<Id>, String)>, StoreError> {
        let row = sqlx::query(
            "SELECT server_id, stream_id, address FROM unsubscribe_tokens WHERE token = $1",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(row.map(|row| {
            (
                row.get::<i64, _>("server_id") as Id,
                row.get::<Option<i64>, _>("stream_id").map(|id| id as Id),
                row.get::<String, _>("address"),
            )
        }))
    }

    async fn record_unsubscribe(&self, token: &str) -> Result<bool, StoreError> {
        let Some((server_id, stream_id, address)) = self.resolve_unsubscribe_token(token).await?
        else {
            return Ok(false);
        };
        // Idempotent stream-scoped suppression: ON CONFLICT DO NOTHING against
        // the (server_id, address, COALESCE(stream_id, 0)) unique index.
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        sqlx::query(
            "INSERT INTO suppressions (server_id, type, address, reason, stream_id)
             VALUES ($1, 'unsubscribe', $2, 'Unsubscribed via List-Unsubscribe', $3)
             ON CONFLICT (server_id, address, COALESCE(stream_id, 0)) DO NOTHING",
        )
        .bind(server_id as i64)
        .bind(&address)
        .bind(stream_id.map(|id| id as i64))
        .execute(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        // Flip the opt-in to `unsubscribed` for the targeted stream (a
        // subscription always belongs to a concrete stream). Same tenant tx.
        if let Some(stream_id) = stream_id {
            sqlx::query(
                "INSERT INTO subscriptions (server_id, stream_id, address, status)
                 VALUES ($1, $2, $3, 'unsubscribed')
                 ON CONFLICT (server_id, stream_id, address)
                 DO UPDATE SET status = 'unsubscribed'",
            )
            .bind(server_id as i64)
            .bind(stream_id as i64)
            .bind(&address)
            .execute(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        }
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(true)
    }

    async fn list_subscriptions(
        &self,
        server_id: Id,
        stream_id: Id,
    ) -> Result<Vec<Subscription>, StoreError> {
        // Tenant-scoped table: enter the RLS context; the query filters the
        // stream (server scope comes from RLS).
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let rows = sqlx::query("SELECT * FROM subscriptions WHERE stream_id = $1 ORDER BY id")
            .bind(stream_id as i64)
            .fetch_all(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(rows.iter().map(subscription_from_row).collect())
    }

    async fn upsert_subscription(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
        status: &str,
    ) -> Result<Subscription, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let row = sqlx::query(
            "INSERT INTO subscriptions (server_id, stream_id, address, status)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (server_id, stream_id, address)
             DO UPDATE SET status = EXCLUDED.status
             RETURNING *",
        )
        .bind(server_id as i64)
        .bind(stream_id as i64)
        .bind(address)
        .bind(status)
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(subscription_from_row(&row))
    }

    async fn remove_subscription(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let result = sqlx::query("DELETE FROM subscriptions WHERE stream_id = $1 AND address = $2")
            .bind(stream_id as i64)
            .bind(address)
            .execute(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(result.rows_affected() > 0)
    }

    async fn is_subscribed(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let row = sqlx::query(
            "SELECT 1 AS one FROM subscriptions
             WHERE stream_id = $1 AND address = $2 AND status = 'subscribed'
             LIMIT 1",
        )
        .bind(stream_id as i64)
        .bind(address)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(row.is_some())
    }

    async fn record_complaint(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
    ) -> Result<Subscription, StoreError> {
        // Same tenant transaction writes both sides, mirroring
        // record_unsubscribe but with a `complaint` suppression keyed on the
        // address (no token) and a concrete stream.
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        // Idempotent stream-scoped suppression: ON CONFLICT DO NOTHING against
        // the (server_id, address, COALESCE(stream_id, 0)) unique index.
        sqlx::query(
            "INSERT INTO suppressions (server_id, type, address, reason, stream_id)
             VALUES ($1, 'complaint', $2, 'Marked as spam', $3)
             ON CONFLICT (server_id, address, COALESCE(stream_id, 0)) DO NOTHING",
        )
        .bind(server_id as i64)
        .bind(address)
        .bind(stream_id as i64)
        .execute(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        // Flip the opt-in closed for the targeted stream.
        let row = sqlx::query(
            "INSERT INTO subscriptions (server_id, stream_id, address, status)
             VALUES ($1, $2, $3, 'unsubscribed')
             ON CONFLICT (server_id, stream_id, address)
             DO UPDATE SET status = 'unsubscribed'
             RETURNING *",
        )
        .bind(server_id as i64)
        .bind(stream_id as i64)
        .bind(address)
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(subscription_from_row(&row))
    }

    async fn create_campaign(&self, new: NewCampaign) -> Result<Campaign, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, new.server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let row = sqlx::query(
            "INSERT INTO campaigns
                 (server_id, stream_id, name, subject, from_address, html_body,
                  text_body, total)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING *",
        )
        .bind(new.server_id as i64)
        .bind(new.stream_id as i64)
        .bind(&new.name)
        .bind(&new.subject)
        .bind(&new.from_address)
        .bind(&new.html_body)
        .bind(&new.text_body)
        .bind(new.total as i32)
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(campaign_from_row(&row))
    }

    async fn get_campaign(&self, server_id: Id, id: Id) -> Result<Option<Campaign>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let row = sqlx::query("SELECT * FROM campaigns WHERE id = $1")
            .bind(id as i64)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(row.as_ref().map(campaign_from_row))
    }

    async fn list_campaigns(
        &self,
        server_id: Id,
        stream_id: Id,
    ) -> Result<Vec<Campaign>, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        let rows = sqlx::query("SELECT * FROM campaigns WHERE stream_id = $1 ORDER BY id DESC")
            .bind(stream_id as i64)
            .fetch_all(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(rows.iter().map(campaign_from_row).collect())
    }

    async fn set_campaign_progress(
        &self,
        server_id: Id,
        id: Id,
        sent: i64,
        status: &str,
        completed: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        sqlx::query("UPDATE campaigns SET sent = $1, status = $2, completed_at = $3 WHERE id = $4")
            .bind(sent as i32)
            .bind(status)
            .bind(completed)
            .bind(id as i64)
            .execute(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(())
    }

    async fn set_message_campaign(
        &self,
        server_id: Id,
        message_id: i64,
        campaign_id: Id,
    ) -> Result<(), StoreError> {
        // RLS scopes the UPDATE to the tenant; a foreign message id matches no
        // row and is a silent no-op.
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        sqlx::query("UPDATE messages SET campaign_id = $1 WHERE id = $2")
            .bind(campaign_id as i64)
            .bind(message_id)
            .execute(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(())
    }

    async fn campaign_stats(
        &self,
        server_id: Id,
        campaign_id: Id,
    ) -> Result<CampaignStats, StoreError> {
        let mut tx = self.pool.begin().await.map_err(Self::sqlx_error)?;
        set_tenant_context(&mut tx, server_id)
            .await
            .map_err(Self::sqlx_error)?;
        // total/sent come from the campaign row; the rest aggregate over the
        // attributed messages plus their loads/link_clicks. When the campaign
        // is not the tenant's, RLS returns no row and we report zeros.
        let campaign = sqlx::query("SELECT * FROM campaigns WHERE id = $1")
            .bind(campaign_id as i64)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Self::sqlx_error)?;
        let Some(campaign) = campaign.as_ref().map(campaign_from_row) else {
            tx.commit().await.map_err(Self::sqlx_error)?;
            return Ok(CampaignStats::default());
        };
        let counts = sqlx::query(
            "SELECT
                 count(*) FILTER (WHERE status = 'Sent' AND NOT held) AS delivered,
                 count(*) FILTER (WHERE status IN ('Bounced', 'HardFail') OR bounce) AS failed
             FROM messages WHERE campaign_id = $1",
        )
        .bind(campaign_id as i64)
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        let opened = sqlx::query(
            "SELECT count(DISTINCT l.message_id) AS opened
             FROM loads l JOIN messages m ON m.id = l.message_id
             WHERE m.campaign_id = $1",
        )
        .bind(campaign_id as i64)
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        let clicked = sqlx::query(
            "SELECT count(DISTINCT li.message_id) AS clicked
             FROM link_clicks lc
             JOIN links li ON li.id = lc.link_id
             JOIN messages m ON m.id = li.message_id
             WHERE m.campaign_id = $1",
        )
        .bind(campaign_id as i64)
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        let unsubscribed = sqlx::query(
            "SELECT count(*) AS unsubscribed FROM suppressions
             WHERE stream_id = $1 AND type IN ('unsubscribe', 'complaint')
               AND created_at >= $2",
        )
        .bind(campaign.stream_id as i64)
        .bind(campaign.created_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(Self::sqlx_error)?;
        tx.commit().await.map_err(Self::sqlx_error)?;
        Ok(CampaignStats {
            total: campaign.total,
            sent: campaign.sent,
            delivered: counts.get("delivered"),
            failed: counts.get("failed"),
            opened: opened.get("opened"),
            clicked: clicked.get("clicked"),
            unsubscribed: unsubscribed.get("unsubscribed"),
        })
    }

    async fn tags(
        &self,
        server_id: Id,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<camelmailer_core::TagCount>, StoreError> {
        PgMessageSink::new(self.clone())
            .tag_counts(server_id, since)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    // The api_requests table is deliberately not RLS-protected (the
    // worker's retention job deletes across tenants) — every query here
    // therefore carries an explicit server_id filter.
    async fn record_api_request(
        &self,
        new: camelmailer_core::NewApiRequest,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO api_requests
                 (server_id, method, path, status_code, duration_ms, user_agent)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(new.server_id as i64)
        .bind(&new.method)
        .bind(&new.path)
        .bind(new.status_code)
        .bind(new.duration_ms)
        .bind(&new.user_agent)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(Self::sqlx_error)
    }

    async fn api_requests(
        &self,
        server_id: Id,
        filter: &camelmailer_core::ApiRequestFilter,
    ) -> Result<Vec<camelmailer_core::ApiRequestRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM api_requests
             WHERE server_id = $1
               AND ($2::int IS NULL OR status_code / 100 = $2)
               AND ($3::text IS NULL OR upper(method) = upper($3))
               AND ($4::timestamptz IS NULL OR created_at >= $4)
               AND ($5::timestamptz IS NULL OR created_at <= $5)
             ORDER BY id DESC",
        )
        .bind(server_id as i64)
        .bind(filter.status_class)
        .bind(&filter.method)
        .bind(filter.from)
        .bind(filter.to)
        .fetch_all(&self.pool)
        .await
        .map_err(Self::sqlx_error)?;
        Ok(rows.iter().map(api_request_from_row).collect())
    }

    async fn prune_api_requests(
        &self,
        older_than: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, StoreError> {
        sqlx::query("DELETE FROM api_requests WHERE created_at < $1")
            .bind(older_than)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
            .map_err(Self::sqlx_error)
    }
}

fn api_request_from_row(row: &PgRow) -> camelmailer_core::ApiRequestRecord {
    camelmailer_core::ApiRequestRecord {
        id: row.get("id"),
        server_id: row.get::<i64, _>("server_id") as Id,
        method: row.get("method"),
        path: row.get("path"),
        status_code: row.get("status_code"),
        duration_ms: row.get("duration_ms"),
        user_agent: row.get("user_agent"),
        created_at: row.get("created_at"),
    }
}

fn dmarc_report_from_row(row: &PgRow) -> camelmailer_core::DmarcReport {
    camelmailer_core::DmarcReport {
        id: row.get("id"),
        server_id: row.get::<i64, _>("server_id") as Id,
        domain: row.get("domain"),
        org_name: row.get("org_name"),
        org_email: row.get("org_email"),
        report_id: row.get("report_id"),
        date_range_begin: row.get("date_range_begin"),
        date_range_end: row.get("date_range_end"),
        received_at: row.get("received_at"),
        record_count: row.get("record_count"),
    }
}

fn dmarc_record_from_row(row: &PgRow) -> camelmailer_core::DmarcRecordRow {
    camelmailer_core::DmarcRecordRow {
        id: row.get("id"),
        report_id: row.get("report_id"),
        source_ip: row.get("source_ip"),
        count: row.get("count"),
        disposition: row.get("disposition"),
        dkim_result: row.get("dkim_result"),
        spf_result: row.get("spf_result"),
        dkim_aligned: row.get("dkim_aligned"),
        spf_aligned: row.get("spf_aligned"),
        header_from: row.get("header_from"),
        envelope_from: row.get("envelope_from"),
    }
}

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

    fn find_confirmed_sender_address(&self, server_id: Id, header_values: &[&str]) -> bool {
        if header_values.is_empty() {
            return false;
        }
        header_values.iter().all(|value| {
            let address = store::strip_name_from_address(value);
            self.wait(AdminStore::confirmed_sender_address(
                self, server_id, address,
            ))
            .unwrap_or(false)
        })
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
    pub tag: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub stream_id: Option<Id>,
    pub created_at: chrono::DateTime<chrono::Utc>,
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
        tag: row.get("tag"),
        metadata: row.get("metadata"),
        stream_id: row.get::<Option<i64>, _>("stream_id").map(|id| id as Id),
        created_at: row.get("created_at"),
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
        self.insert_message_returning(message)
            .await
            .map(|(id, _)| id)
    }

    /// Insert and return both the id and the generated public token.
    pub async fn insert_message_returning(
        &self,
        message: &QueuedMessage,
    ) -> Result<(i64, String), sqlx::Error> {
        // index the interesting headers at insert time, like the Ruby
        // message DB does on save
        let subject = camelmailer_core::message::header_value(&message.raw_message, "subject");
        let message_id_header =
            camelmailer_core::message::header_value(&message.raw_message, "message-id");
        let public_token = token::generate_token(12);

        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, message.server_id).await?;
        let row = sqlx::query(
            "INSERT INTO messages
                 (server_id, token, scope, rcpt_to, mail_from, bounce,
                  received_with_ssl, domain_id, credential_id, route_id, raw_message,
                  subject, message_id_header, size, tag, metadata, stream_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
             RETURNING id",
        )
        .bind(message.server_id as i64)
        .bind(&public_token)
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
        .bind(&message.tag)
        .bind(&message.metadata)
        .bind(message.stream_id.map(|id| id as i64))
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
        Ok((message_id, public_token))
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
    /// `Postal::MessageDB::Message#create_delivery`). `bounce_category`
    /// (hard / soft / undetermined) is persisted on the message for
    /// terminal failures; `None` leaves any existing classification alone.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_delivery(
        &self,
        server_id: Id,
        message_id: i64,
        status: &str,
        details: &str,
        output: &str,
        sent_with_ssl: bool,
        bounce_category: Option<&str>,
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
             SET status = $2, held = ($2 = 'Held'), last_delivery_attempt = now(),
                 bounce_category = COALESCE($3, bounce_category)
             WHERE id = $1",
        )
        .bind(message_id)
        .bind(status)
        .bind(bounce_category)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.get("id"))
    }

    /// Persist a bounce classification on a message without touching its
    /// delivery state (used when an inbound bounce/DSN is processed).
    pub async fn set_bounce_category(
        &self,
        server_id: Id,
        message_id: i64,
        bounce_category: &str,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        sqlx::query("UPDATE messages SET bounce_category = $2 WHERE id = $1")
            .bind(message_id)
            .bind(bounce_category)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
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

    /// The tenant's messages matching `filter`, newest first. RLS scopes the
    /// result to the tenant; the filter narrows scope/status/tag and does an
    /// ILIKE substring match on subject/recipient.
    pub async fn message_records(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, sqlx::Error> {
        self.message_rows(server_id, filter, false).await
    }

    /// Bounced messages (`bounce` flag or `Bounced` status), newest first.
    pub async fn bounce_records(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, sqlx::Error> {
        self.message_rows(server_id, filter, true).await
    }

    async fn message_rows(
        &self,
        server_id: Id,
        filter: &MessageFilter,
        only_bounces: bool,
    ) -> Result<Vec<MessageRecord>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let mut qb: QueryBuilder<sqlx::Postgres> =
            QueryBuilder::new("SELECT * FROM messages WHERE TRUE");
        if only_bounces {
            qb.push(" AND (bounce OR status = 'Bounced')");
        }
        if let Some(scope) = &filter.scope {
            qb.push(" AND scope = ").push_bind(scope.clone());
        }
        if let Some(status) = &filter.status {
            qb.push(" AND status = ").push_bind(status.clone());
        }
        if let Some(tag) = &filter.tag {
            qb.push(" AND tag = ").push_bind(tag.clone());
        }
        if let Some(stream_id) = filter.stream_id {
            qb.push(" AND stream_id = ").push_bind(stream_id as i64);
        }
        if let Some(query) = &filter.query {
            let like = format!("%{query}%");
            qb.push(" AND (subject ILIKE ")
                .push_bind(like.clone())
                .push(" OR rcpt_to ILIKE ")
                .push_bind(like)
                .push(")");
        }
        qb.push(" ORDER BY id DESC");
        let rows = qb.build().fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(rows.iter().map(message_record_from_row).collect())
    }

    /// One message as a read-model record, tenant-scoped.
    pub async fn message_record(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let row = sqlx::query("SELECT * FROM messages WHERE id = $1")
            .bind(message_id)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row.as_ref().map(message_record_from_row))
    }

    /// Re-queue an incoming message for processing: reset status to Pending,
    /// optionally set the `bypassed` flag, and enqueue it (the worker's
    /// inbound path re-delivers). Both the status update and the enqueue run
    /// in one tenant transaction. Returns the updated record, or `None` if it
    /// isn't the server's incoming message.
    pub async fn requeue_inbound(
        &self,
        server_id: Id,
        message_id: i64,
        bypass: bool,
    ) -> Result<Option<MessageRecord>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let row = sqlx::query(
            "UPDATE messages
             SET status = 'Pending', bypassed = (bypassed OR $2)
             WHERE id = $1 AND scope = 'incoming'
             RETURNING *",
        )
        .bind(message_id)
        .bind(bypass)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let record = message_record_from_row(&row);
        let domain = record
            .rcpt_to
            .rsplit_once('@')
            .map(|(_, d)| d)
            .unwrap_or_default();
        sqlx::query(
            "INSERT INTO queued_messages (message_id, server_id, domain) VALUES ($1, $2, $3)",
        )
        .bind(message_id)
        .bind(server_id as i64)
        .bind(domain)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(record))
    }

    /// Delivery attempts (with timestamps) for a message, tenant-scoped.
    pub async fn delivery_records(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<DeliveryRecord>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let rows = sqlx::query("SELECT * FROM deliveries WHERE message_id = $1 ORDER BY id")
            .bind(message_id)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rows.iter().map(delivery_record_from_row).collect())
    }

    /// Opens (pixel loads) for a message, newest first, tenant-scoped.
    pub async fn opens_for_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<ActivityEvent>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let rows = sqlx::query(
            "SELECT ip_address, user_agent, NULL::text AS url, created_at
             FROM loads WHERE message_id = $1 ORDER BY id DESC",
        )
        .bind(message_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.iter().map(activity_event_from_row).collect())
    }

    /// Link clicks for a message, newest first, tenant-scoped.
    pub async fn clicks_for_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<ActivityEvent>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let rows = sqlx::query(
            "SELECT lc.ip_address AS ip_address, lc.user_agent AS user_agent,
                    l.url AS url, lc.created_at AS created_at
             FROM link_clicks lc
             JOIN links l ON l.id = lc.link_id
             WHERE l.message_id = $1
             ORDER BY lc.id DESC",
        )
        .bind(message_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.iter().map(activity_event_from_row).collect())
    }

    /// Aggregate message + engagement counters over an optional time window,
    /// tenant-scoped (RLS).
    pub async fn message_stats(
        &self,
        server_id: Id,
        filter: &camelmailer_core::StatsFilter,
    ) -> Result<camelmailer_core::MessageStats, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let from = filter.from;
        let to = filter.to;
        let tag = filter.tag.as_deref();
        let counts = sqlx::query(
            "SELECT
                 count(*) AS total,
                 count(*) FILTER (WHERE scope = 'incoming') AS incoming,
                 count(*) FILTER (WHERE scope = 'outgoing') AS outgoing,
                 count(*) FILTER (WHERE status = 'Sent') AS sent,
                 count(*) FILTER (WHERE status = 'Held') AS held,
                 count(*) FILTER (WHERE status = 'SoftFail') AS soft_fail,
                 count(*) FILTER (WHERE status = 'HardFail') AS hard_fail,
                 count(*) FILTER (WHERE status = 'Bounced') AS bounced,
                 count(*) FILTER (WHERE status = 'Pending') AS pending,
                 count(*) FILTER (WHERE bounce_category = 'hard') AS bounces_hard,
                 count(*) FILTER (WHERE bounce_category = 'soft') AS bounces_soft,
                 count(*) FILTER (WHERE bounce_category = 'undetermined'
                     OR (bounce_category IS NULL AND (bounce OR status = 'Bounced')))
                     AS bounces_undetermined
             FROM messages
             WHERE ($1::timestamptz IS NULL OR created_at >= $1)
               AND ($2::timestamptz IS NULL OR created_at <= $2)
               AND ($3::text IS NULL OR tag = $3)",
        )
        .bind(from)
        .bind(to)
        .bind(tag)
        .fetch_one(&mut *tx)
        .await?;
        let opens = sqlx::query(
            "SELECT count(*) AS opens, count(DISTINCT l.message_id) AS unique_opens
             FROM loads l JOIN messages m ON m.id = l.message_id
             WHERE ($1::timestamptz IS NULL OR m.created_at >= $1)
               AND ($2::timestamptz IS NULL OR m.created_at <= $2)
               AND ($3::text IS NULL OR m.tag = $3)",
        )
        .bind(from)
        .bind(to)
        .bind(tag)
        .fetch_one(&mut *tx)
        .await?;
        let clicks = sqlx::query(
            "SELECT count(*) AS clicks, count(DISTINCT li.message_id) AS unique_clicks
             FROM link_clicks lc
             JOIN links li ON li.id = lc.link_id
             JOIN messages m ON m.id = li.message_id
             WHERE ($1::timestamptz IS NULL OR m.created_at >= $1)
               AND ($2::timestamptz IS NULL OR m.created_at <= $2)
               AND ($3::text IS NULL OR m.tag = $3)",
        )
        .bind(from)
        .bind(to)
        .bind(tag)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(camelmailer_core::MessageStats {
            total: counts.get("total"),
            incoming: counts.get("incoming"),
            outgoing: counts.get("outgoing"),
            sent: counts.get("sent"),
            held: counts.get("held"),
            soft_fail: counts.get("soft_fail"),
            hard_fail: counts.get("hard_fail"),
            bounced: counts.get("bounced"),
            pending: counts.get("pending"),
            opens: opens.get("opens"),
            unique_opens: opens.get("unique_opens"),
            clicks: clicks.get("clicks"),
            unique_clicks: clicks.get("unique_clicks"),
            bounces_hard: counts.get("bounces_hard"),
            bounces_soft: counts.get("bounces_soft"),
            bounces_undetermined: counts.get("bounces_undetermined"),
        })
    }

    /// Tags used by the tenant's messages since `since`, with counts,
    /// most used first. RLS scopes the rows to the tenant context.
    pub async fn tag_counts(
        &self,
        server_id: Id,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<camelmailer_core::TagCount>, sqlx::Error> {
        let mut tx = self.store.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let rows = sqlx::query(
            "SELECT tag, count(*) AS c FROM messages
             WHERE tag IS NOT NULL AND created_at >= $1
             GROUP BY tag ORDER BY c DESC, tag ASC",
        )
        .bind(since)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .iter()
            .map(|row| camelmailer_core::TagCount {
                tag: row.get("tag"),
                count: row.get("c"),
            })
            .collect())
    }

    /// Pending outbound queue depth per destination domain. Reads the
    /// cross-tenant `queued_messages` work list with an explicit
    /// `server_id` filter (the table is deliberately not RLS-protected).
    pub async fn delivery_stats(
        &self,
        server_id: Id,
    ) -> Result<camelmailer_core::DeliveryStats, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT domain, count(*) AS c FROM queued_messages
             WHERE server_id = $1 GROUP BY domain ORDER BY domain",
        )
        .bind(server_id as i64)
        .fetch_all(&self.store.pool)
        .await?;
        let domains: Vec<camelmailer_core::QueuedDomain> = rows
            .iter()
            .map(|row| camelmailer_core::QueuedDomain {
                domain: row.get("domain"),
                count: row.get("c"),
            })
            .collect();
        let queued = domains.iter().map(|d| d.count).sum();
        Ok(camelmailer_core::DeliveryStats { queued, domains })
    }
}

fn message_record_from_row(row: &PgRow) -> MessageRecord {
    MessageRecord {
        id: row.get("id"),
        token: row.get("token"),
        server_id: row.get::<i64, _>("server_id") as Id,
        scope: row.get("scope"),
        rcpt_to: row.get("rcpt_to"),
        mail_from: row.get("mail_from"),
        subject: row.get("subject"),
        message_id_header: row.get("message_id_header"),
        tag: row.get("tag"),
        status: row.get("status"),
        bounce: row.get("bounce"),
        bounce_category: row.get("bounce_category"),
        spam_status: row.get("spam_status"),
        spam_score: row.get("spam_score"),
        held: row.get("held"),
        threat: row.get("threat"),
        size: row.get("size"),
        metadata: row.get("metadata"),
        stream_id: row.get::<Option<i64>, _>("stream_id").map(|id| id as Id),
        bypassed: row.get("bypassed"),
        created_at: row.get("created_at"),
        raw_message: row.get("raw_message"),
    }
}

fn message_stream_from_row(row: &PgRow) -> camelmailer_core::MessageStream {
    camelmailer_core::MessageStream {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        server_id: row.get::<i64, _>("server_id") as Id,
        name: row.get("name"),
        permalink: row.get("permalink"),
        stream_type: row.get("stream_type"),
        archived: row.get("archived"),
        ip_pool_id: row.get::<Option<i64>, _>("ip_pool_id").map(|id| id as Id),
    }
}

fn template_from_row(row: &PgRow) -> camelmailer_core::Template {
    camelmailer_core::Template {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        server_id: row.get::<i64, _>("server_id") as Id,
        name: row.get("name"),
        permalink: row.get("permalink"),
        subject: row.get("subject"),
        html_body: row.get("html_body"),
        text_body: row.get("text_body"),
        archived: row.get("archived"),
        layout_id: row.get::<Option<i64>, _>("layout_id").map(|id| id as Id),
    }
}

fn layout_from_row(row: &PgRow) -> camelmailer_core::Layout {
    camelmailer_core::Layout {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        server_id: row.get::<i64, _>("server_id") as Id,
        name: row.get("name"),
        permalink: row.get("permalink"),
        html_wrapper: row.get("html_wrapper"),
        text_wrapper: row.get("text_wrapper"),
    }
}

fn delivery_record_from_row(row: &PgRow) -> DeliveryRecord {
    DeliveryRecord {
        id: row.get("id"),
        status: row.get("status"),
        details: row.get("details"),
        output: row.get("output"),
        sent_with_ssl: row.get("sent_with_ssl"),
        created_at: row.get("created_at"),
    }
}

fn activity_event_from_row(row: &PgRow) -> ActivityEvent {
    ActivityEvent {
        ip_address: row.get("ip_address"),
        user_agent: row.get("user_agent"),
        url: row.get("url"),
        created_at: row.get("created_at"),
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
