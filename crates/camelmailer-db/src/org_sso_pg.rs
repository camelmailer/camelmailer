//! The PostgreSQL implementation of [`OrgSsoStore`]: per-organization SSO
//! connections and the email domains that route logins to a tenant.

use async_trait::async_trait;
use camelmailer_core::{
    Id, NewOrgEmailDomain, NewOrgSsoConnection, OrgEmailDomain, OrgSsoConnection,
    OrgSsoConnectionUpdate, OrgSsoStore, Role, SsoKind, StoreError,
};
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::pg_store::PgStore;

fn sqlx_error(error: sqlx::Error) -> StoreError {
    if let sqlx::Error::Database(db_error) = &error {
        if db_error.code().as_deref() == Some("23505") {
            let message = match db_error.constraint() {
                Some("organization_email_domains_org_domain_key") => {
                    "This domain is already claimed by the organization"
                }
                Some("idx_org_email_domains_verified_domain") => {
                    "This domain is already verified by another organization"
                }
                _ => "Record is not unique",
            };
            return StoreError::Conflict(message.into());
        }
    }
    StoreError::Other(error.to_string())
}

fn normalize_domain(domain: &str) -> String {
    domain.trim().trim_start_matches('@').to_ascii_lowercase()
}

fn email_domain_from_row(row: &PgRow) -> Result<OrgEmailDomain, StoreError> {
    Ok(OrgEmailDomain {
        id: row.get::<i64, _>("id") as Id,
        organization_id: row.get::<i64, _>("organization_id") as Id,
        domain: row.get("domain"),
        verified: row.get("verified"),
        verification_token: row.get("verification_token"),
        created_at: row.get("created_at"),
    })
}

fn connection_from_row(row: &PgRow) -> Result<OrgSsoConnection, StoreError> {
    let kind = SsoKind::parse(row.get::<String, _>("kind").as_str())
        .ok_or_else(|| StoreError::Other("unknown sso connection kind".into()))?;
    let default_role = Role::parse(row.get::<String, _>("default_role").as_str())
        .ok_or_else(|| StoreError::Other("unknown default role".into()))?;
    Ok(OrgSsoConnection {
        id: row.get::<i64, _>("id") as Id,
        organization_id: row.get::<i64, _>("organization_id") as Id,
        kind,
        name: row.get("name"),
        enabled: row.get("enabled"),
        config: row.get::<serde_json::Value, _>("config"),
        default_role,
        auto_provision: row.get("auto_provision"),
        created_at: row.get("created_at"),
    })
}

#[async_trait]
impl OrgSsoStore for PgStore {
    async fn create_org_email_domain(
        &self,
        new: NewOrgEmailDomain,
    ) -> Result<OrgEmailDomain, StoreError> {
        let domain = normalize_domain(&new.domain);
        if domain.is_empty() || !domain.contains('.') {
            return Err(StoreError::Conflict("Enter a valid domain".into()));
        }
        sqlx::query(
            "INSERT INTO organization_email_domains
                 (organization_id, domain, verification_token)
             VALUES ($1, $2, $3) RETURNING *",
        )
        .bind(new.organization_id as i64)
        .bind(&domain)
        .bind(&new.verification_token)
        .fetch_one(self.pool())
        .await
        .map_err(sqlx_error)
        .and_then(|row| email_domain_from_row(&row))
    }

    async fn list_org_email_domains(
        &self,
        organization_id: Id,
    ) -> Result<Vec<OrgEmailDomain>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM organization_email_domains WHERE organization_id = $1 ORDER BY id",
        )
        .bind(organization_id as i64)
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        rows.iter().map(email_domain_from_row).collect()
    }

    async fn org_email_domain(&self, id: Id) -> Result<Option<OrgEmailDomain>, StoreError> {
        let row = sqlx::query("SELECT * FROM organization_email_domains WHERE id = $1")
            .bind(id as i64)
            .fetch_optional(self.pool())
            .await
            .map_err(sqlx_error)?;
        row.as_ref().map(email_domain_from_row).transpose()
    }

    async fn mark_org_email_domain_verified(&self, id: Id) -> Result<(), StoreError> {
        sqlx::query("UPDATE organization_email_domains SET verified = true WHERE id = $1")
            .bind(id as i64)
            .execute(self.pool())
            .await
            .map(|_| ())
            .map_err(sqlx_error)
    }

    async fn delete_org_email_domain(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM organization_email_domains WHERE id = $1")
            .bind(id as i64)
            .execute(self.pool())
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(sqlx_error)
    }

    async fn organization_for_verified_email_domain(
        &self,
        domain: &str,
    ) -> Result<Option<Id>, StoreError> {
        let domain = normalize_domain(domain);
        let row = sqlx::query(
            "SELECT organization_id FROM organization_email_domains
             WHERE verified AND domain = $1",
        )
        .bind(&domain)
        .fetch_optional(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(row.map(|row| row.get::<i64, _>("organization_id") as Id))
    }

    async fn create_org_sso_connection(
        &self,
        new: NewOrgSsoConnection,
    ) -> Result<OrgSsoConnection, StoreError> {
        sqlx::query(
            "INSERT INTO organization_sso_connections
                 (organization_id, kind, name, enabled, config, default_role, auto_provision)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
        )
        .bind(new.organization_id as i64)
        .bind(new.kind.as_str())
        .bind(&new.name)
        .bind(new.enabled)
        .bind(&new.config)
        .bind(new.default_role.as_str())
        .bind(new.auto_provision)
        .fetch_one(self.pool())
        .await
        .map_err(sqlx_error)
        .and_then(|row| connection_from_row(&row))
    }

    async fn list_org_sso_connections(
        &self,
        organization_id: Id,
    ) -> Result<Vec<OrgSsoConnection>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM organization_sso_connections WHERE organization_id = $1 ORDER BY id",
        )
        .bind(organization_id as i64)
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        rows.iter().map(connection_from_row).collect()
    }

    async fn org_sso_connection(&self, id: Id) -> Result<Option<OrgSsoConnection>, StoreError> {
        let row = sqlx::query("SELECT * FROM organization_sso_connections WHERE id = $1")
            .bind(id as i64)
            .fetch_optional(self.pool())
            .await
            .map_err(sqlx_error)?;
        row.as_ref().map(connection_from_row).transpose()
    }

    async fn update_org_sso_connection(
        &self,
        id: Id,
        update: OrgSsoConnectionUpdate,
    ) -> Result<Option<OrgSsoConnection>, StoreError> {
        // COALESCE keeps the current value where the caller passed None, so
        // an untouched secret in `config` survives an edit.
        let row = sqlx::query(
            "UPDATE organization_sso_connections SET
                 name = COALESCE($2, name),
                 enabled = COALESCE($3, enabled),
                 config = COALESCE($4, config),
                 default_role = COALESCE($5, default_role),
                 auto_provision = COALESCE($6, auto_provision)
             WHERE id = $1 RETURNING *",
        )
        .bind(id as i64)
        .bind(update.name)
        .bind(update.enabled)
        .bind(update.config)
        .bind(update.default_role.map(|role| role.as_str().to_string()))
        .bind(update.auto_provision)
        .fetch_optional(self.pool())
        .await
        .map_err(sqlx_error)?;
        row.as_ref().map(connection_from_row).transpose()
    }

    async fn delete_org_sso_connection(&self, id: Id) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM organization_sso_connections WHERE id = $1")
            .bind(id as i64)
            .execute(self.pool())
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(sqlx_error)
    }
}
