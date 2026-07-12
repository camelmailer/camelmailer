//! Per-organization SSO (tenant-based OIDC / SAML / social sign-in),
//! configured through the dashboard. This supplements the instance-wide
//! config groups (`oidc`, `saml`, `auth.sso_providers`): an organization
//! verifies email domains it owns and attaches one or more SSO
//! connections. At login the user's email domain resolves to the owning
//! organization, and its enabled connections drive the sign-in.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::admin_store::StoreError;
use crate::auth::Role;
use crate::model::Id;

/// The protocol of a per-org SSO connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SsoKind {
    Oidc,
    Saml,
    Google,
    Microsoft,
    Github,
}

impl SsoKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SsoKind::Oidc => "oidc",
            SsoKind::Saml => "saml",
            SsoKind::Google => "google",
            SsoKind::Microsoft => "microsoft",
            SsoKind::Github => "github",
        }
    }

    pub fn parse(value: &str) -> Option<SsoKind> {
        match value {
            "oidc" => Some(SsoKind::Oidc),
            "saml" => Some(SsoKind::Saml),
            "google" => Some(SsoKind::Google),
            "microsoft" => Some(SsoKind::Microsoft),
            "github" => Some(SsoKind::Github),
            _ => None,
        }
    }
}

/// An email domain an organization has claimed for SSO login routing. A
/// domain routes logins only once `verified` is true, and a verified
/// domain belongs to exactly one organization (enforced by the store), so
/// no tenant can capture another's sign-ins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrgEmailDomain {
    pub id: Id,
    pub organization_id: Id,
    pub domain: String,
    pub verified: bool,
    /// DNS TXT token proving control of the domain; surfaced until verified.
    pub verification_token: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewOrgEmailDomain {
    pub organization_id: Id,
    pub domain: String,
    pub verification_token: String,
}

/// One tenant SSO connection: an OIDC or SAML provider, or a social
/// button, scoped to a single organization. `config` holds the
/// protocol-specific fields (issuer, client id/secret, IdP URL plus
/// certificate) as JSON so the shape can vary by `kind`; the API layer
/// validates it and redacts secrets on read. `enabled` gates whether the
/// connection drives login.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrgSsoConnection {
    pub id: Id,
    pub organization_id: Id,
    pub kind: SsoKind,
    pub name: String,
    pub enabled: bool,
    pub config: Value,
    /// Role granted to a member auto-provisioned through this connection.
    pub default_role: Role,
    /// Create the account and membership on first login through this
    /// connection. When false, only existing members may use it.
    pub auto_provision: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewOrgSsoConnection {
    pub organization_id: Id,
    pub kind: SsoKind,
    pub name: String,
    pub enabled: bool,
    pub config: Value,
    pub default_role: Role,
    pub auto_provision: bool,
}

/// Fields that may be changed on an existing connection. `None` leaves the
/// current value untouched (secrets in `config` are preserved when the API
/// layer sends the redacted form back unchanged).
#[derive(Debug, Clone, Default)]
pub struct OrgSsoConnectionUpdate {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub config: Option<Value>,
    pub default_role: Option<Role>,
    pub auto_provision: Option<bool>,
}

/// Storage for per-organization SSO configuration. Implemented in lockstep
/// by `MemoryStore` and the Postgres store.
#[async_trait]
pub trait OrgSsoStore: Send + Sync {
    // -- email domains

    async fn create_org_email_domain(
        &self,
        new: NewOrgEmailDomain,
    ) -> Result<OrgEmailDomain, StoreError>;

    async fn list_org_email_domains(
        &self,
        organization_id: Id,
    ) -> Result<Vec<OrgEmailDomain>, StoreError>;

    async fn org_email_domain(&self, id: Id) -> Result<Option<OrgEmailDomain>, StoreError>;

    /// Mark a claimed domain verified. Fails with `Conflict` if another
    /// organization already verified the same domain.
    async fn mark_org_email_domain_verified(&self, id: Id) -> Result<(), StoreError>;

    async fn delete_org_email_domain(&self, id: Id) -> Result<bool, StoreError>;

    /// The organization that owns a *verified* email domain
    /// (case-insensitive). Used to route a login to the right tenant.
    async fn organization_for_verified_email_domain(
        &self,
        domain: &str,
    ) -> Result<Option<Id>, StoreError>;

    // -- connections

    async fn create_org_sso_connection(
        &self,
        new: NewOrgSsoConnection,
    ) -> Result<OrgSsoConnection, StoreError>;

    async fn list_org_sso_connections(
        &self,
        organization_id: Id,
    ) -> Result<Vec<OrgSsoConnection>, StoreError>;

    async fn org_sso_connection(&self, id: Id) -> Result<Option<OrgSsoConnection>, StoreError>;

    async fn update_org_sso_connection(
        &self,
        id: Id,
        update: OrgSsoConnectionUpdate,
    ) -> Result<Option<OrgSsoConnection>, StoreError>;

    async fn delete_org_sso_connection(&self, id: Id) -> Result<bool, StoreError>;
}

// ------------------------------------------------------------- MemoryStore

use crate::store::MemoryStore;

fn normalize_domain(domain: &str) -> String {
    domain.trim().trim_start_matches('@').to_ascii_lowercase()
}

#[async_trait]
impl OrgSsoStore for MemoryStore {
    async fn create_org_email_domain(
        &self,
        new: NewOrgEmailDomain,
    ) -> Result<OrgEmailDomain, StoreError> {
        let domain = normalize_domain(&new.domain);
        if domain.is_empty() || !domain.contains('.') {
            return Err(StoreError::Conflict("Enter a valid domain".into()));
        }
        {
            let inner = self.inner.read().unwrap();
            let duplicate = inner
                .org_email_domains
                .values()
                .any(|d| d.organization_id == new.organization_id && d.domain == domain);
            if duplicate {
                return Err(StoreError::Conflict(
                    "This domain is already claimed by the organization".into(),
                ));
            }
        }
        let id = self.next_id();
        let record = OrgEmailDomain {
            id,
            organization_id: new.organization_id,
            domain,
            verified: false,
            verification_token: new.verification_token,
            created_at: Utc::now(),
        };
        self.inner
            .write()
            .unwrap()
            .org_email_domains
            .insert(id, record.clone());
        Ok(record)
    }

    async fn list_org_email_domains(
        &self,
        organization_id: Id,
    ) -> Result<Vec<OrgEmailDomain>, StoreError> {
        let inner = self.inner.read().unwrap();
        let mut result: Vec<_> = inner
            .org_email_domains
            .values()
            .filter(|d| d.organization_id == organization_id)
            .cloned()
            .collect();
        result.sort_by_key(|d| d.id);
        Ok(result)
    }

    async fn org_email_domain(&self, id: Id) -> Result<Option<OrgEmailDomain>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .org_email_domains
            .get(&id)
            .cloned())
    }

    async fn mark_org_email_domain_verified(&self, id: Id) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let Some(target) = inner.org_email_domains.get(&id).cloned() else {
            return Ok(());
        };
        let taken = inner.org_email_domains.values().any(|d| {
            d.id != id
                && d.verified
                && d.domain == target.domain
                && d.organization_id != target.organization_id
        });
        if taken {
            return Err(StoreError::Conflict(
                "This domain is already verified by another organization".into(),
            ));
        }
        if let Some(domain) = inner.org_email_domains.get_mut(&id) {
            domain.verified = true;
        }
        Ok(())
    }

    async fn delete_org_email_domain(&self, id: Id) -> Result<bool, StoreError> {
        Ok(self
            .inner
            .write()
            .unwrap()
            .org_email_domains
            .remove(&id)
            .is_some())
    }

    async fn organization_for_verified_email_domain(
        &self,
        domain: &str,
    ) -> Result<Option<Id>, StoreError> {
        let domain = normalize_domain(domain);
        let inner = self.inner.read().unwrap();
        Ok(inner
            .org_email_domains
            .values()
            .find(|d| d.verified && d.domain == domain)
            .map(|d| d.organization_id))
    }

    async fn create_org_sso_connection(
        &self,
        new: NewOrgSsoConnection,
    ) -> Result<OrgSsoConnection, StoreError> {
        let id = self.next_id();
        let record = OrgSsoConnection {
            id,
            organization_id: new.organization_id,
            kind: new.kind,
            name: new.name,
            enabled: new.enabled,
            config: new.config,
            default_role: new.default_role,
            auto_provision: new.auto_provision,
            created_at: Utc::now(),
        };
        self.inner
            .write()
            .unwrap()
            .org_sso_connections
            .insert(id, record.clone());
        Ok(record)
    }

    async fn list_org_sso_connections(
        &self,
        organization_id: Id,
    ) -> Result<Vec<OrgSsoConnection>, StoreError> {
        let inner = self.inner.read().unwrap();
        let mut result: Vec<_> = inner
            .org_sso_connections
            .values()
            .filter(|c| c.organization_id == organization_id)
            .cloned()
            .collect();
        result.sort_by_key(|c| c.id);
        Ok(result)
    }

    async fn org_sso_connection(&self, id: Id) -> Result<Option<OrgSsoConnection>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .org_sso_connections
            .get(&id)
            .cloned())
    }

    async fn update_org_sso_connection(
        &self,
        id: Id,
        update: OrgSsoConnectionUpdate,
    ) -> Result<Option<OrgSsoConnection>, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let Some(connection) = inner.org_sso_connections.get_mut(&id) else {
            return Ok(None);
        };
        if let Some(name) = update.name {
            connection.name = name;
        }
        if let Some(enabled) = update.enabled {
            connection.enabled = enabled;
        }
        if let Some(config) = update.config {
            connection.config = config;
        }
        if let Some(role) = update.default_role {
            connection.default_role = role;
        }
        if let Some(auto_provision) = update.auto_provision {
            connection.auto_provision = auto_provision;
        }
        Ok(Some(connection.clone()))
    }

    async fn delete_org_sso_connection(&self, id: Id) -> Result<bool, StoreError> {
        Ok(self
            .inner
            .write()
            .unwrap()
            .org_sso_connections
            .remove(&id)
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn oidc_conn(org: Id) -> NewOrgSsoConnection {
        NewOrgSsoConnection {
            organization_id: org,
            kind: SsoKind::Oidc,
            name: "Acme Okta".into(),
            enabled: true,
            config: json!({ "issuer": "https://acme.okta.com", "client_id": "abc" }),
            default_role: Role::Member,
            auto_provision: true,
        }
    }

    #[tokio::test]
    async fn a_verified_domain_routes_to_its_organization() {
        let store = MemoryStore::new();
        let domain = store
            .create_org_email_domain(NewOrgEmailDomain {
                organization_id: 7,
                domain: "Acme.COM".into(),
                verification_token: "tok".into(),
            })
            .await
            .unwrap();
        // normalized + not yet routable while unverified
        assert_eq!(domain.domain, "acme.com");
        assert!(!domain.verified);
        assert_eq!(
            store
                .organization_for_verified_email_domain("user@acme.com")
                .await
                .unwrap(),
            None
        );

        store
            .mark_org_email_domain_verified(domain.id)
            .await
            .unwrap();
        // case-insensitive lookup, with or without the local part stripped
        assert_eq!(
            store
                .organization_for_verified_email_domain("acme.com")
                .await
                .unwrap(),
            Some(7)
        );
    }

    #[tokio::test]
    async fn a_domain_cannot_be_verified_by_two_organizations() {
        let store = MemoryStore::new();
        let a = store
            .create_org_email_domain(NewOrgEmailDomain {
                organization_id: 1,
                domain: "shared.com".into(),
                verification_token: "t1".into(),
            })
            .await
            .unwrap();
        let b = store
            .create_org_email_domain(NewOrgEmailDomain {
                organization_id: 2,
                domain: "shared.com".into(),
                verification_token: "t2".into(),
            })
            .await
            .unwrap();
        store.mark_org_email_domain_verified(a.id).await.unwrap();
        let second = store.mark_org_email_domain_verified(b.id).await;
        assert!(matches!(second, Err(StoreError::Conflict(_))));
        assert_eq!(
            store
                .organization_for_verified_email_domain("shared.com")
                .await
                .unwrap(),
            Some(1)
        );
    }

    #[tokio::test]
    async fn connections_are_scoped_created_updated_and_deleted() {
        let store = MemoryStore::new();
        let created = store.create_org_sso_connection(oidc_conn(3)).await.unwrap();
        assert!(created.enabled);
        // scoped to the org
        assert_eq!(store.list_org_sso_connections(3).await.unwrap().len(), 1);
        assert_eq!(store.list_org_sso_connections(4).await.unwrap().len(), 0);

        let updated = store
            .update_org_sso_connection(
                created.id,
                OrgSsoConnectionUpdate {
                    enabled: Some(false),
                    name: Some("Renamed".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.name, "Renamed");
        // untouched fields survive
        assert_eq!(updated.config, created.config);
        assert_eq!(updated.default_role, Role::Member);

        assert!(store.delete_org_sso_connection(created.id).await.unwrap());
        assert!(store.list_org_sso_connections(3).await.unwrap().is_empty());
    }
}
