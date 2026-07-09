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
        Ok(self.insert_server(Server {
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

    async fn create_admin_api_key(&self, _name: &str, key: &str) -> Result<(), StoreError> {
        self.insert_admin_api_key(key);
        Ok(())
    }
}
