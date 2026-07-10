//! Test fixtures shared across crates (the Rust counterpart of the
//! FactoryBot factories in `spec/factories`).

use crate::model::*;
use crate::store::MemoryStore;
use crate::token;
use std::sync::Arc;

/// Builds a [`MemoryStore`] pre-populated with one organization and one
/// server, plus helpers to add related records — mirroring
/// `create(:server)` / `create(:credential, ...)` in the Ruby specs.
pub struct Fixtures {
    store: Arc<MemoryStore>,
    organization: Organization,
    server: Server,
}

impl Default for Fixtures {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixtures {
    pub fn new() -> Self {
        let store = Arc::new(MemoryStore::new());
        let organization = store.insert_organization(Organization {
            id: store.next_id(),
            uuid: token::generate_uuid(),
            name: "Example Org".into(),
            permalink: "example-org".into(),
        });
        let server = store.insert_server(Server {
            id: store.next_id(),
            uuid: token::generate_uuid(),
            organization_id: organization.id,
            name: "Example Server".into(),
            permalink: "example-server".into(),
            token: token::generate_token(6),
            mode: ServerMode::Live,
            suspended: false,
            suspension_reason: None,
            privacy_mode: false,
            log_smtp_data: false,
            allow_sender: false,
            ip_pool_id: None,
        });
        Self {
            store,
            organization,
            server,
        }
    }

    pub fn store(&self) -> Arc<MemoryStore> {
        self.store.clone()
    }

    pub fn organization(&self) -> &Organization {
        &self.organization
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    pub fn server_id(&self) -> Id {
        self.server.id
    }

    pub fn suspend_server(&self) {
        let mut server = self.server.clone();
        server.suspended = true;
        server.suspension_reason = Some("Suspended for testing".into());
        self.store.insert_server(server);
    }

    pub fn set_privacy_mode(&self, enabled: bool) {
        let mut server = self.server.clone();
        server.privacy_mode = enabled;
        self.store.insert_server(server);
    }

    pub fn credential(&self, credential_type: CredentialType, key: &str) -> Credential {
        self.store.insert_credential(Credential {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            server_id: self.server.id,
            credential_type,
            name: "Test Credential".into(),
            key: key.into(),
            hold: false,
        })
    }

    pub fn verified_server_domain(&self, name: &str) -> Domain {
        self.store.insert_domain(Domain {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            owner: DomainOwner::Server(self.server.id),
            name: name.into(),
            verified: true,
        })
    }

    pub fn unverified_server_domain(&self, name: &str) -> Domain {
        self.store.insert_domain(Domain {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            owner: DomainOwner::Server(self.server.id),
            name: name.into(),
            verified: false,
        })
    }

    pub fn route(&self, name: &str, domain_id: Option<Id>, mode: RouteMode) -> Route {
        self.store.insert_route(Route {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            server_id: self.server.id,
            domain_id,
            name: name.into(),
            token: token::generate_token(8),
            mode,
            endpoint_url: None,
        })
    }

    pub fn route_with_endpoint(
        &self,
        name: &str,
        domain_id: Option<Id>,
        endpoint_url: &str,
    ) -> Route {
        self.store.insert_route(Route {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            server_id: self.server.id,
            domain_id,
            name: name.into(),
            token: token::generate_token(8),
            mode: RouteMode::Endpoint,
            endpoint_url: Some(endpoint_url.into()),
        })
    }
}
