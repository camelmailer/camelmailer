//! The async storage interface behind the Admin API — implemented by
//! [`crate::MemoryStore`] for tests and by the Postgres store in
//! `camelmailer-db` for production.

use crate::model::*;
use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A uniqueness/validation conflict (maps to 422 in the API).
    #[error("{0}")]
    Conflict(String),
    #[error("storage error: {0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct NewOrganization {
    pub name: String,
    pub permalink: String,
}

#[derive(Debug, Clone)]
pub struct NewServer {
    pub organization_id: Id,
    pub name: String,
    pub permalink: String,
    pub mode: ServerMode,
}

#[derive(Debug, Clone)]
pub struct NewCredential {
    pub server_id: Id,
    pub credential_type: CredentialType,
    pub name: String,
    /// Generated when absent (except for SMTP-IP, where it is the CIDR).
    pub key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewRoute {
    pub server_id: Id,
    pub domain_id: Option<Id>,
    pub name: String,
    pub mode: RouteMode,
    pub endpoint_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewWebhook {
    pub server_id: Id,
    pub name: String,
    pub url: String,
    pub all_events: bool,
    pub sign: bool,
}

#[derive(Debug, Clone)]
pub struct NewSuppression {
    pub server_id: Id,
    pub suppression_type: String,
    pub address: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewUser {
    pub email_address: String,
    pub first_name: String,
    pub last_name: String,
    pub admin: bool,
}

#[derive(Debug, Clone)]
pub struct NewIpAddress {
    pub ip_pool_id: Id,
    pub ipv4: String,
    pub ipv6: Option<String>,
    pub hostname: String,
    pub priority: i32,
}

/// A resolved tracking token: which tenant/message it belongs to and, for
/// click tokens, the original URL to redirect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackingTarget {
    pub kind: String,
    pub server_id: Id,
    pub message_id: i64,
    pub link_id: Option<Id>,
    pub target_url: Option<String>,
}

/// The storage behind the public click/open tracking endpoints. Kept
/// separate from [`AdminStore`] because the tracking HTTP server is
/// unauthenticated and only needs token resolution + recording.
#[async_trait]
pub trait TrackingStore: Send + Sync {
    async fn resolve_token(&self, token: &str) -> Result<Option<TrackingTarget>, StoreError>;
    /// Record a click on a resolved token (ip/user-agent for the audit row).
    async fn record_click(
        &self,
        target: &TrackingTarget,
        ip_address: &str,
        user_agent: &str,
    ) -> Result<(), StoreError>;
    async fn record_open(
        &self,
        target: &TrackingTarget,
        ip_address: &str,
        user_agent: &str,
    ) -> Result<(), StoreError>;
}

#[async_trait]
pub trait AdminStore: Send + Sync {
    async fn list_organizations(&self) -> Result<Vec<Organization>, StoreError>;
    async fn organization_by_permalink(
        &self,
        permalink: &str,
    ) -> Result<Option<Organization>, StoreError>;
    async fn create_organization(
        &self,
        new: NewOrganization,
    ) -> Result<Organization, StoreError>;
    async fn delete_organization(&self, id: Id) -> Result<bool, StoreError>;

    async fn servers_for_organization(&self, organization_id: Id)
        -> Result<Vec<Server>, StoreError>;
    async fn server_by_permalink(
        &self,
        organization_id: Id,
        permalink: &str,
    ) -> Result<Option<Server>, StoreError>;
    async fn create_server(&self, new: NewServer) -> Result<Server, StoreError>;
    async fn update_server(&self, server: Server) -> Result<Server, StoreError>;
    async fn delete_server(&self, id: Id) -> Result<bool, StoreError>;

    /// Is this a valid database-backed admin API key? Implementations also
    /// record the use (`last_used_at`).
    async fn admin_api_key_valid(&self, key: &str) -> Result<bool, StoreError>;
    async fn create_admin_api_key(&self, name: &str, key: &str) -> Result<(), StoreError>;

    // domains (server-scoped; addressed by name)
    async fn list_domains(&self, server_id: Id) -> Result<Vec<Domain>, StoreError>;
    async fn domain_by_name(&self, server_id: Id, name: &str)
        -> Result<Option<Domain>, StoreError>;
    async fn create_server_domain(&self, server_id: Id, name: &str)
        -> Result<Domain, StoreError>;
    async fn set_domain_verified(&self, domain_id: Id, verified: bool)
        -> Result<(), StoreError>;
    async fn delete_domain(&self, domain_id: Id) -> Result<bool, StoreError>;

    // credentials
    async fn list_credentials(&self, server_id: Id) -> Result<Vec<Credential>, StoreError>;
    async fn credential_by_id(&self, server_id: Id, id: Id)
        -> Result<Option<Credential>, StoreError>;
    async fn create_credential_record(&self, new: NewCredential)
        -> Result<Credential, StoreError>;
    async fn update_credential(&self, credential: Credential) -> Result<Credential, StoreError>;
    async fn delete_credential(&self, id: Id) -> Result<bool, StoreError>;

    // routes
    async fn list_routes(&self, server_id: Id) -> Result<Vec<Route>, StoreError>;
    async fn route_by_id(&self, server_id: Id, id: Id) -> Result<Option<Route>, StoreError>;
    async fn create_route_record(&self, new: NewRoute) -> Result<Route, StoreError>;
    async fn update_route(&self, route: Route) -> Result<Route, StoreError>;
    async fn delete_route(&self, id: Id) -> Result<bool, StoreError>;

    // webhooks
    async fn list_webhooks(&self, server_id: Id) -> Result<Vec<Webhook>, StoreError>;
    async fn webhook_by_id(&self, server_id: Id, id: Id) -> Result<Option<Webhook>, StoreError>;
    async fn create_webhook(&self, new: NewWebhook) -> Result<Webhook, StoreError>;
    async fn update_webhook(&self, webhook: Webhook) -> Result<Webhook, StoreError>;
    async fn delete_webhook(&self, id: Id) -> Result<bool, StoreError>;

    // suppressions (tenant-scoped)
    async fn list_suppressions(&self, server_id: Id) -> Result<Vec<Suppression>, StoreError>;
    async fn create_suppression(&self, new: NewSuppression) -> Result<Suppression, StoreError>;
    async fn delete_suppression(&self, server_id: Id, address: &str)
        -> Result<bool, StoreError>;

    // users (global)
    async fn list_users(&self) -> Result<Vec<User>, StoreError>;
    async fn user_by_id(&self, id: Id) -> Result<Option<User>, StoreError>;
    async fn create_user(&self, new: NewUser) -> Result<User, StoreError>;
    async fn update_user(&self, user: User) -> Result<User, StoreError>;
    async fn delete_user(&self, id: Id) -> Result<bool, StoreError>;

    // IP pools (global) + nested addresses
    async fn list_ip_pools(&self) -> Result<Vec<IpPool>, StoreError>;
    async fn ip_pool_by_id(&self, id: Id) -> Result<Option<IpPool>, StoreError>;
    async fn create_ip_pool(&self, name: &str, default: bool) -> Result<IpPool, StoreError>;
    async fn update_ip_pool(&self, pool: IpPool) -> Result<IpPool, StoreError>;
    async fn delete_ip_pool(&self, id: Id) -> Result<bool, StoreError>;
    async fn list_ip_addresses(&self, ip_pool_id: Id) -> Result<Vec<IpAddress>, StoreError>;
    async fn ip_address_by_id(&self, ip_pool_id: Id, id: Id)
        -> Result<Option<IpAddress>, StoreError>;
    async fn create_ip_address(&self, new: NewIpAddress) -> Result<IpAddress, StoreError>;
    async fn delete_ip_address(&self, id: Id) -> Result<bool, StoreError>;

    /// Assign (or clear) a server's outbound IP pool.
    async fn set_server_ip_pool(
        &self,
        server_id: Id,
        ip_pool_id: Option<Id>,
    ) -> Result<(), StoreError>;

    /// A per-server API token (`credentials` with type=API) resolves to
    /// exactly one server. Returns the server if the token is valid and the
    /// credential is not on hold; records the use (`last_used_at`).
    async fn server_for_api_token(&self, key: &str) -> Result<Option<Server>, StoreError>;

    /// The id of a verified domain named `domain_name` owned by the server
    /// or its organization — the From-domain authorization for HTTP send.
    async fn authenticated_domain(
        &self,
        server_id: Id,
        domain_name: &str,
    ) -> Result<Option<Id>, StoreError>;

    // admin API key management (returns display records; never the secret)
    async fn list_admin_api_keys(&self) -> Result<Vec<AdminApiKey>, StoreError>;
    async fn create_admin_api_key_record(
        &self,
        name: &str,
        key: &str,
    ) -> Result<AdminApiKey, StoreError>;
    async fn delete_admin_api_key(&self, id: Id) -> Result<bool, StoreError>;
}

#[async_trait]
impl AdminStore for crate::store::MemoryStore {
    async fn list_organizations(&self) -> Result<Vec<Organization>, StoreError> {
        Ok(self.organizations())
    }

    async fn organization_by_permalink(
        &self,
        permalink: &str,
    ) -> Result<Option<Organization>, StoreError> {
        Ok(self
            .organizations()
            .into_iter()
            .find(|o| o.permalink == permalink))
    }

    async fn create_organization(
        &self,
        new: NewOrganization,
    ) -> Result<Organization, StoreError> {
        if self
            .organizations()
            .iter()
            .any(|o| o.permalink == new.permalink)
        {
            return Err(StoreError::Conflict(
                "Permalink has already been taken".into(),
            ));
        }
        Ok(self.insert_organization(Organization {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            name: new.name,
            permalink: new.permalink,
        }))
    }

    async fn delete_organization(&self, id: Id) -> Result<bool, StoreError> {
        Ok(crate::store::MemoryStore::delete_organization(self, id))
    }

    async fn servers_for_organization(
        &self,
        organization_id: Id,
    ) -> Result<Vec<Server>, StoreError> {
        Ok(self
            .servers()
            .into_iter()
            .filter(|s| s.organization_id == organization_id)
            .collect())
    }

    async fn server_by_permalink(
        &self,
        organization_id: Id,
        permalink: &str,
    ) -> Result<Option<Server>, StoreError> {
        Ok(self
            .servers()
            .into_iter()
            .find(|s| s.organization_id == organization_id && s.permalink == permalink))
    }

    async fn create_server(&self, new: NewServer) -> Result<Server, StoreError> {
        if self
            .servers()
            .iter()
            .any(|s| s.organization_id == new.organization_id && s.permalink == new.permalink)
        {
            return Err(StoreError::Conflict(
                "Permalink has already been taken".into(),
            ));
        }
        let server = self.insert_server(Server {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            organization_id: new.organization_id,
            name: new.name,
            permalink: new.permalink,
            token: crate::token::generate_token(6),
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
            color: None,
            default_stream_id: None,
        });
        // Give every server a built-in transactional stream (parity with the
        // migration's backfill), and point default_stream_id at it.
        let stream = self.insert_stream(MessageStream {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            server_id: server.id,
            name: "Default Transactional Stream".into(),
            permalink: "outbound".into(),
            stream_type: "transactional".into(),
            archived: false,
        });
        Ok(self.insert_server(Server {
            default_stream_id: Some(stream.id),
            ..server
        }))
    }

    async fn update_server(&self, server: Server) -> Result<Server, StoreError> {
        Ok(self.insert_server(server))
    }

    async fn delete_server(&self, id: Id) -> Result<bool, StoreError> {
        Ok(crate::store::MemoryStore::delete_server(self, id))
    }

    async fn admin_api_key_valid(&self, key: &str) -> Result<bool, StoreError> {
        Ok(self.admin_api_key_exists(key))
    }

    async fn create_admin_api_key(&self, name: &str, key: &str) -> Result<(), StoreError> {
        self.insert_admin_api_key_named(name, key);
        Ok(())
    }

    async fn server_for_api_token(&self, key: &str) -> Result<Option<Server>, StoreError> {
        let inner = self.inner.read().unwrap();
        let credential = inner.credentials.values().find(|c| {
            c.credential_type == CredentialType::Api && c.key == key && !c.hold
        });
        Ok(credential.and_then(|c| inner.servers.get(&c.server_id).cloned()))
    }

    async fn authenticated_domain(
        &self,
        server_id: Id,
        domain_name: &str,
    ) -> Result<Option<Id>, StoreError> {
        let inner = self.inner.read().unwrap();
        let Some(server) = inner.servers.get(&server_id) else {
            return Ok(None);
        };
        Ok(inner
            .domains
            .values()
            .filter(|d| d.verified && d.name == domain_name)
            .find(|d| match d.owner {
                DomainOwner::Server(id) => id == server.id,
                DomainOwner::Organization(id) => id == server.organization_id,
            })
            .map(|d| d.id))
    }

    async fn list_admin_api_keys(&self) -> Result<Vec<AdminApiKey>, StoreError> {
        Ok(crate::store::MemoryStore::list_admin_api_keys(self))
    }

    async fn create_admin_api_key_record(
        &self,
        name: &str,
        key: &str,
    ) -> Result<AdminApiKey, StoreError> {
        Ok(self.insert_admin_api_key_named(name, key))
    }

    async fn delete_admin_api_key(&self, id: Id) -> Result<bool, StoreError> {
        Ok(crate::store::MemoryStore::delete_admin_api_key(self, id))
    }

    async fn list_domains(&self, server_id: Id) -> Result<Vec<Domain>, StoreError> {
        let mut domains: Vec<Domain> = self
            .inner
            .read()
            .unwrap()
            .domains
            .values()
            .filter(|d| d.owner == DomainOwner::Server(server_id))
            .cloned()
            .collect();
        domains.sort_by_key(|d| d.id);
        Ok(domains)
    }

    async fn domain_by_name(
        &self,
        server_id: Id,
        name: &str,
    ) -> Result<Option<Domain>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .domains
            .values()
            .find(|d| d.owner == DomainOwner::Server(server_id) && d.name == name)
            .cloned())
    }

    async fn create_server_domain(
        &self,
        server_id: Id,
        name: &str,
    ) -> Result<Domain, StoreError> {
        if self.domain_by_name(server_id, name).await?.is_some() {
            return Err(StoreError::Conflict("Name has already been taken".into()));
        }
        Ok(self.insert_domain(Domain {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            owner: DomainOwner::Server(server_id),
            name: name.into(),
            verified: false,
        }))
    }

    async fn set_domain_verified(&self, domain_id: Id, verified: bool) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        if let Some(domain) = inner.domains.get_mut(&domain_id) {
            domain.verified = verified;
        }
        Ok(())
    }

    async fn delete_domain(&self, domain_id: Id) -> Result<bool, StoreError> {
        Ok(crate::store::MemoryStore::delete_domain(self, domain_id))
    }

    async fn list_credentials(&self, server_id: Id) -> Result<Vec<Credential>, StoreError> {
        let mut credentials: Vec<Credential> = self
            .inner
            .read()
            .unwrap()
            .credentials
            .values()
            .filter(|c| c.server_id == server_id)
            .cloned()
            .collect();
        credentials.sort_by_key(|c| c.id);
        Ok(credentials)
    }

    async fn credential_by_id(
        &self,
        server_id: Id,
        id: Id,
    ) -> Result<Option<Credential>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .credentials
            .get(&id)
            .filter(|c| c.server_id == server_id)
            .cloned())
    }

    async fn create_credential_record(
        &self,
        new: NewCredential,
    ) -> Result<Credential, StoreError> {
        let key = new.key.unwrap_or_else(crate::token::generate_key);
        Ok(self.insert_credential(Credential {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            server_id: new.server_id,
            credential_type: new.credential_type,
            name: new.name,
            key,
            hold: false,
        }))
    }

    async fn update_credential(&self, credential: Credential) -> Result<Credential, StoreError> {
        Ok(self.insert_credential(credential))
    }

    async fn delete_credential(&self, id: Id) -> Result<bool, StoreError> {
        Ok(self.inner.write().unwrap().credentials.remove(&id).is_some())
    }

    async fn list_routes(&self, server_id: Id) -> Result<Vec<Route>, StoreError> {
        let mut routes: Vec<Route> = self
            .inner
            .read()
            .unwrap()
            .routes
            .values()
            .filter(|r| r.server_id == server_id)
            .cloned()
            .collect();
        routes.sort_by_key(|r| r.id);
        Ok(routes)
    }

    async fn route_by_id(&self, server_id: Id, id: Id) -> Result<Option<Route>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .routes
            .get(&id)
            .filter(|r| r.server_id == server_id)
            .cloned())
    }

    async fn create_route_record(&self, new: NewRoute) -> Result<Route, StoreError> {
        Ok(self.insert_route(Route {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            server_id: new.server_id,
            domain_id: new.domain_id,
            name: new.name,
            token: crate::token::generate_token(8),
            mode: new.mode,
            endpoint_url: new.endpoint_url,
        }))
    }

    async fn update_route(&self, route: Route) -> Result<Route, StoreError> {
        Ok(self.insert_route(route))
    }

    async fn delete_route(&self, id: Id) -> Result<bool, StoreError> {
        Ok(self.inner.write().unwrap().routes.remove(&id).is_some())
    }

    async fn list_webhooks(&self, server_id: Id) -> Result<Vec<Webhook>, StoreError> {
        let mut webhooks: Vec<Webhook> = self
            .inner
            .read()
            .unwrap()
            .webhooks
            .values()
            .filter(|w| w.server_id == server_id)
            .cloned()
            .collect();
        webhooks.sort_by_key(|w| w.id);
        Ok(webhooks)
    }

    async fn webhook_by_id(&self, server_id: Id, id: Id) -> Result<Option<Webhook>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .webhooks
            .get(&id)
            .filter(|w| w.server_id == server_id)
            .cloned())
    }

    async fn create_webhook(&self, new: NewWebhook) -> Result<Webhook, StoreError> {
        let webhook = Webhook {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            server_id: new.server_id,
            name: new.name,
            url: new.url,
            all_events: new.all_events,
            enabled: true,
            sign: new.sign,
        };
        self.inner
            .write()
            .unwrap()
            .webhooks
            .insert(webhook.id, webhook.clone());
        Ok(webhook)
    }

    async fn update_webhook(&self, webhook: Webhook) -> Result<Webhook, StoreError> {
        self.inner
            .write()
            .unwrap()
            .webhooks
            .insert(webhook.id, webhook.clone());
        Ok(webhook)
    }

    async fn delete_webhook(&self, id: Id) -> Result<bool, StoreError> {
        Ok(self.inner.write().unwrap().webhooks.remove(&id).is_some())
    }

    async fn list_suppressions(&self, server_id: Id) -> Result<Vec<Suppression>, StoreError> {
        let mut suppressions: Vec<Suppression> = self
            .inner
            .read()
            .unwrap()
            .suppressions
            .values()
            .filter(|s| s.server_id == server_id)
            .cloned()
            .collect();
        suppressions.sort_by_key(|s| s.id);
        Ok(suppressions)
    }

    async fn create_suppression(&self, new: NewSuppression) -> Result<Suppression, StoreError> {
        {
            let inner = self.inner.read().unwrap();
            if inner
                .suppressions
                .values()
                .any(|s| s.server_id == new.server_id && s.address == new.address)
            {
                return Err(StoreError::Conflict(
                    "Address is already suppressed".into(),
                ));
            }
        }
        let suppression = Suppression {
            id: self.next_id(),
            server_id: new.server_id,
            suppression_type: new.suppression_type,
            address: new.address,
            reason: new.reason,
        };
        self.inner
            .write()
            .unwrap()
            .suppressions
            .insert(suppression.id, suppression.clone());
        Ok(suppression)
    }

    async fn delete_suppression(
        &self,
        server_id: Id,
        address: &str,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let id = inner
            .suppressions
            .values()
            .find(|s| s.server_id == server_id && s.address == address)
            .map(|s| s.id);
        Ok(id.map(|id| inner.suppressions.remove(&id)).is_some())
    }

    async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        let mut users: Vec<User> = self
            .inner
            .read()
            .unwrap()
            .users
            .values()
            .cloned()
            .collect();
        users.sort_by_key(|u| u.id);
        Ok(users)
    }

    async fn user_by_id(&self, id: Id) -> Result<Option<User>, StoreError> {
        Ok(self.inner.read().unwrap().users.get(&id).cloned())
    }

    async fn create_user(&self, new: NewUser) -> Result<User, StoreError> {
        {
            let inner = self.inner.read().unwrap();
            if inner
                .users
                .values()
                .any(|u| u.email_address == new.email_address)
            {
                return Err(StoreError::Conflict(
                    "Email address has already been taken".into(),
                ));
            }
        }
        let user = User {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            email_address: new.email_address,
            first_name: new.first_name,
            last_name: new.last_name,
            admin: new.admin,
        };
        self.inner.write().unwrap().users.insert(user.id, user.clone());
        Ok(user)
    }

    async fn update_user(&self, user: User) -> Result<User, StoreError> {
        self.inner.write().unwrap().users.insert(user.id, user.clone());
        Ok(user)
    }

    async fn delete_user(&self, id: Id) -> Result<bool, StoreError> {
        Ok(self.inner.write().unwrap().users.remove(&id).is_some())
    }

    async fn list_ip_pools(&self) -> Result<Vec<IpPool>, StoreError> {
        let mut pools: Vec<IpPool> = self
            .inner
            .read()
            .unwrap()
            .ip_pools
            .values()
            .cloned()
            .collect();
        pools.sort_by_key(|p| p.id);
        Ok(pools)
    }

    async fn ip_pool_by_id(&self, id: Id) -> Result<Option<IpPool>, StoreError> {
        Ok(self.inner.read().unwrap().ip_pools.get(&id).cloned())
    }

    async fn create_ip_pool(&self, name: &str, default: bool) -> Result<IpPool, StoreError> {
        let pool = IpPool {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            name: name.into(),
            default,
        };
        self.inner
            .write()
            .unwrap()
            .ip_pools
            .insert(pool.id, pool.clone());
        Ok(pool)
    }

    async fn update_ip_pool(&self, pool: IpPool) -> Result<IpPool, StoreError> {
        self.inner
            .write()
            .unwrap()
            .ip_pools
            .insert(pool.id, pool.clone());
        Ok(pool)
    }

    async fn delete_ip_pool(&self, id: Id) -> Result<bool, StoreError> {
        Ok(self.inner.write().unwrap().ip_pools.remove(&id).is_some())
    }

    async fn list_ip_addresses(&self, ip_pool_id: Id) -> Result<Vec<IpAddress>, StoreError> {
        let mut addresses: Vec<IpAddress> = self
            .inner
            .read()
            .unwrap()
            .ip_addresses
            .values()
            .filter(|a| a.ip_pool_id == ip_pool_id)
            .cloned()
            .collect();
        addresses.sort_by_key(|a| a.id);
        Ok(addresses)
    }

    async fn ip_address_by_id(
        &self,
        ip_pool_id: Id,
        id: Id,
    ) -> Result<Option<IpAddress>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .ip_addresses
            .get(&id)
            .filter(|a| a.ip_pool_id == ip_pool_id)
            .cloned())
    }

    async fn create_ip_address(&self, new: NewIpAddress) -> Result<IpAddress, StoreError> {
        let address = IpAddress {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            ip_pool_id: new.ip_pool_id,
            ipv4: new.ipv4,
            ipv6: new.ipv6,
            hostname: new.hostname,
            priority: new.priority,
        };
        self.inner
            .write()
            .unwrap()
            .ip_addresses
            .insert(address.id, address.clone());
        Ok(address)
    }

    async fn delete_ip_address(&self, id: Id) -> Result<bool, StoreError> {
        Ok(self
            .inner
            .write()
            .unwrap()
            .ip_addresses
            .remove(&id)
            .is_some())
    }

    async fn set_server_ip_pool(
        &self,
        server_id: Id,
        ip_pool_id: Option<Id>,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        if let Some(server) = inner.servers.get_mut(&server_id) {
            server.ip_pool_id = ip_pool_id;
        }
        Ok(())
    }
}
