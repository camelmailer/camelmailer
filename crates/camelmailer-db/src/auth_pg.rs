//! The PostgreSQL implementation of [`AuthStore`] (accounts, sessions,
//! RBAC memberships, invitations, password resets, OIDC state, WebAuthn
//! credentials, audit log).

use async_trait::async_trait;
use camelmailer_core::{
    token, AuthEvent, AuthSession, AuthStore, Id, Invitation, NewAuthEvent, NewAuthSession,
    NewInvitation, NewWebAuthnCredential, Organization, OrganizationMembership, Role, StoreError,
    User, UserAuth, WebAuthnCredential,
};
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::pg_store::PgStore;

fn sqlx_error(error: sqlx::Error) -> StoreError {
    if let sqlx::Error::Database(db_error) = &error {
        if db_error.code().as_deref() == Some("23505") {
            let message = match db_error.constraint() {
                Some("idx_invitations_pending") => {
                    "An invitation for this email address is already pending"
                }
                Some("organization_memberships_organization_id_user_id_key") => {
                    "User is already a member of this organization"
                }
                Some("user_auth_oidc_sub_key") | Some("sso_identities_pkey") => {
                    "This SSO identity is already linked to another user"
                }
                Some("webauthn_credentials_credential_id_key") => {
                    "This passkey is already registered"
                }
                _ => "Record is not unique",
            };
            return StoreError::Conflict(message.into());
        }
    }
    StoreError::Other(error.to_string())
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

fn session_from_row(row: &PgRow) -> AuthSession {
    AuthSession {
        id: row.get::<i64, _>("id") as Id,
        user_id: row.get::<i64, _>("user_id") as Id,
        token_hash: row.get("token_hash"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
        last_used_at: row.get("last_used_at"),
        ip_address: row.get("ip_address"),
        user_agent: row.get("user_agent"),
    }
}

fn membership_from_row(row: &PgRow) -> Result<OrganizationMembership, StoreError> {
    let role: String = row.get("role");
    Ok(OrganizationMembership {
        id: row.get::<i64, _>("id") as Id,
        organization_id: row.get::<i64, _>("organization_id") as Id,
        user_id: row.get::<i64, _>("user_id") as Id,
        role: Role::parse(&role)
            .ok_or_else(|| StoreError::Other(format!("unknown role {role:?} in database")))?,
        created_at: row.get("created_at"),
    })
}

fn invitation_from_row(row: &PgRow) -> Result<Invitation, StoreError> {
    let role: String = row.get("role");
    Ok(Invitation {
        id: row.get::<i64, _>("id") as Id,
        uuid: row.get("uuid"),
        organization_id: row.get::<i64, _>("organization_id") as Id,
        email_address: row.get("email_address"),
        role: Role::parse(&role)
            .ok_or_else(|| StoreError::Other(format!("unknown role {role:?} in database")))?,
        token_hash: row.get("token_hash"),
        invited_by_user_id: row.get::<i64, _>("invited_by_user_id") as Id,
        expires_at: row.get("expires_at"),
        accepted_at: row.get("accepted_at"),
    })
}

fn webauthn_credential_from_row(row: &PgRow) -> WebAuthnCredential {
    WebAuthnCredential {
        id: row.get::<i64, _>("id") as Id,
        user_id: row.get::<i64, _>("user_id") as Id,
        name: row.get("name"),
        credential_id: row.get("credential_id"),
        credential_json: row.get("credential"),
        created_at: row.get("created_at"),
        last_used_at: row.get("last_used_at"),
    }
}

#[async_trait]
impl AuthStore for PgStore {
    async fn user_by_email(&self, email: &str) -> Result<Option<User>, StoreError> {
        sqlx::query("SELECT * FROM users WHERE lower(email_address) = lower($1)")
            .bind(email)
            .fetch_optional(self.pool())
            .await
            .map(|row| row.as_ref().map(user_from_row))
            .map_err(sqlx_error)
    }

    async fn user_auth(&self, user_id: Id) -> Result<Option<UserAuth>, StoreError> {
        let exists = sqlx::query("SELECT 1 FROM users WHERE id = $1")
            .bind(user_id as i64)
            .fetch_optional(self.pool())
            .await
            .map_err(sqlx_error)?;
        if exists.is_none() {
            return Ok(None);
        }
        let row = sqlx::query("SELECT * FROM user_auth WHERE user_id = $1")
            .bind(user_id as i64)
            .fetch_optional(self.pool())
            .await
            .map_err(sqlx_error)?;
        Ok(Some(match row {
            Some(row) => UserAuth {
                user_id,
                password_digest: row.get("password_digest"),
                totp_secret: row.get("totp_secret"),
                totp_enabled: row.get("totp_enabled"),
                failed_login_attempts: row.get::<i32, _>("failed_login_attempts") as u32,
                locked_until: row.get("locked_until"),
                last_login_at: row.get("last_login_at"),
                oidc_sub: row.get("oidc_sub"),
                disabled: row.get("disabled"),
            },
            None => UserAuth {
                user_id,
                ..UserAuth::default()
            },
        }))
    }

    async fn set_password_digest(&self, user_id: Id, digest: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO user_auth (user_id, password_digest, updated_at)
             VALUES ($1, $2, now())
             ON CONFLICT (user_id)
             DO UPDATE SET password_digest = $2, updated_at = now()",
        )
        .bind(user_id as i64)
        .bind(digest)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn set_totp(
        &self,
        user_id: Id,
        secret: Option<&str>,
        enabled: bool,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO user_auth (user_id, totp_secret, totp_enabled, updated_at)
             VALUES ($1, $2, $3, now())
             ON CONFLICT (user_id)
             DO UPDATE SET totp_secret = $2, totp_enabled = $3, updated_at = now()",
        )
        .bind(user_id as i64)
        .bind(secret)
        .bind(enabled)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn set_login_state(
        &self,
        user_id: Id,
        failed_attempts: u32,
        locked_until: Option<DateTime<Utc>>,
        last_login_at: Option<DateTime<Utc>>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO user_auth (user_id, failed_login_attempts, locked_until, last_login_at, updated_at)
             VALUES ($1, $2, $3, $4, now())
             ON CONFLICT (user_id)
             DO UPDATE SET failed_login_attempts = $2, locked_until = $3,
                           last_login_at = COALESCE($4, user_auth.last_login_at),
                           updated_at = now()",
        )
        .bind(user_id as i64)
        .bind(failed_attempts as i32)
        .bind(locked_until)
        .bind(last_login_at)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn create_session(&self, new: NewAuthSession) -> Result<AuthSession, StoreError> {
        sqlx::query(
            "INSERT INTO auth_sessions (user_id, token_hash, expires_at, ip_address, user_agent)
             VALUES ($1, $2, $3, $4, $5) RETURNING *",
        )
        .bind(new.user_id as i64)
        .bind(&new.token_hash)
        .bind(new.expires_at)
        .bind(&new.ip_address)
        .bind(&new.user_agent)
        .fetch_one(self.pool())
        .await
        .map(|row| session_from_row(&row))
        .map_err(sqlx_error)
    }

    async fn session_with_user(
        &self,
        token_hash: &str,
    ) -> Result<Option<(AuthSession, User)>, StoreError> {
        let row = sqlx::query(
            "SELECT s.id, s.user_id, s.token_hash, s.created_at, s.expires_at,
                    s.last_used_at, s.ip_address, s.user_agent,
                    u.id AS u_id, u.uuid AS u_uuid, u.email_address AS u_email,
                    u.first_name AS u_first, u.last_name AS u_last, u.admin AS u_admin
             FROM auth_sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = $1",
        )
        .bind(token_hash)
        .fetch_optional(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(row.map(|row| {
            (
                session_from_row(&row),
                User {
                    id: row.get::<i64, _>("u_id") as Id,
                    uuid: row.get("u_uuid"),
                    email_address: row.get("u_email"),
                    first_name: row.get("u_first"),
                    last_name: row.get("u_last"),
                    admin: row.get("u_admin"),
                },
            )
        }))
    }

    async fn touch_session(
        &self,
        session_id: Id,
        last_used_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE auth_sessions SET last_used_at = $2, expires_at = $3 WHERE id = $1")
            .bind(session_id as i64)
            .bind(last_used_at)
            .bind(expires_at)
            .execute(self.pool())
            .await
            .map(|_| ())
            .map_err(sqlx_error)
    }

    async fn delete_session(&self, token_hash: &str) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM auth_sessions WHERE token_hash = $1")
            .bind(token_hash)
            .execute(self.pool())
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(sqlx_error)
    }

    async fn delete_sessions_for_user(&self, user_id: Id) -> Result<u64, StoreError> {
        sqlx::query("DELETE FROM auth_sessions WHERE user_id = $1")
            .bind(user_id as i64)
            .execute(self.pool())
            .await
            .map(|result| result.rows_affected())
            .map_err(sqlx_error)
    }

    async fn memberships_for_user(
        &self,
        user_id: Id,
    ) -> Result<Vec<(OrganizationMembership, Organization)>, StoreError> {
        let rows = sqlx::query(
            "SELECT m.id, m.organization_id, m.user_id, m.role, m.created_at,
                    o.id AS o_id, o.uuid AS o_uuid, o.name AS o_name, o.permalink AS o_permalink
             FROM organization_memberships m
             JOIN organizations o ON o.id = m.organization_id
             WHERE m.user_id = $1
             ORDER BY o.name",
        )
        .bind(user_id as i64)
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok((
                    membership_from_row(row)?,
                    Organization {
                        id: row.get::<i64, _>("o_id") as Id,
                        uuid: row.get("o_uuid"),
                        name: row.get("o_name"),
                        permalink: row.get("o_permalink"),
                    },
                ))
            })
            .collect()
    }

    async fn membership(
        &self,
        organization_id: Id,
        user_id: Id,
    ) -> Result<Option<OrganizationMembership>, StoreError> {
        let row = sqlx::query(
            "SELECT * FROM organization_memberships
             WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id as i64)
        .bind(user_id as i64)
        .fetch_optional(self.pool())
        .await
        .map_err(sqlx_error)?;
        row.as_ref().map(membership_from_row).transpose()
    }

    async fn memberships_for_organization(
        &self,
        organization_id: Id,
    ) -> Result<Vec<(OrganizationMembership, User)>, StoreError> {
        let rows = sqlx::query(
            "SELECT m.id, m.organization_id, m.user_id, m.role, m.created_at,
                    u.id AS u_id, u.uuid AS u_uuid, u.email_address AS u_email,
                    u.first_name AS u_first, u.last_name AS u_last, u.admin AS u_admin
             FROM organization_memberships m
             JOIN users u ON u.id = m.user_id
             WHERE m.organization_id = $1
             ORDER BY u.email_address",
        )
        .bind(organization_id as i64)
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        rows.iter()
            .map(|row| {
                Ok((
                    membership_from_row(row)?,
                    User {
                        id: row.get::<i64, _>("u_id") as Id,
                        uuid: row.get("u_uuid"),
                        email_address: row.get("u_email"),
                        first_name: row.get("u_first"),
                        last_name: row.get("u_last"),
                        admin: row.get("u_admin"),
                    },
                ))
            })
            .collect()
    }

    async fn upsert_membership(
        &self,
        organization_id: Id,
        user_id: Id,
        role: Role,
    ) -> Result<OrganizationMembership, StoreError> {
        sqlx::query(
            "INSERT INTO organization_memberships (organization_id, user_id, role)
             VALUES ($1, $2, $3)
             ON CONFLICT (organization_id, user_id) DO UPDATE SET role = $3
             RETURNING *",
        )
        .bind(organization_id as i64)
        .bind(user_id as i64)
        .bind(role.as_str())
        .fetch_one(self.pool())
        .await
        .map_err(sqlx_error)
        .and_then(|row| membership_from_row(&row))
    }

    async fn delete_membership(
        &self,
        organization_id: Id,
        user_id: Id,
    ) -> Result<bool, StoreError> {
        sqlx::query(
            "DELETE FROM organization_memberships WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id as i64)
        .bind(user_id as i64)
        .execute(self.pool())
        .await
        .map(|result| result.rows_affected() > 0)
        .map_err(sqlx_error)
    }

    async fn create_invitation(&self, new: NewInvitation) -> Result<Invitation, StoreError> {
        sqlx::query(
            "INSERT INTO invitations
                 (uuid, organization_id, email_address, role, token_hash,
                  invited_by_user_id, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
        )
        .bind(token::generate_uuid())
        .bind(new.organization_id as i64)
        .bind(&new.email_address)
        .bind(new.role.as_str())
        .bind(&new.token_hash)
        .bind(new.invited_by_user_id as i64)
        .bind(new.expires_at)
        .fetch_one(self.pool())
        .await
        .map_err(sqlx_error)
        .and_then(|row| invitation_from_row(&row))
    }

    async fn list_invitations(&self, organization_id: Id) -> Result<Vec<Invitation>, StoreError> {
        let rows = sqlx::query("SELECT * FROM invitations WHERE organization_id = $1 ORDER BY id")
            .bind(organization_id as i64)
            .fetch_all(self.pool())
            .await
            .map_err(sqlx_error)?;
        rows.iter().map(invitation_from_row).collect()
    }

    async fn invitation_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<Invitation>, StoreError> {
        let row = sqlx::query("SELECT * FROM invitations WHERE token_hash = $1")
            .bind(token_hash)
            .fetch_optional(self.pool())
            .await
            .map_err(sqlx_error)?;
        row.as_ref().map(invitation_from_row).transpose()
    }

    async fn mark_invitation_accepted(&self, invitation_id: Id) -> Result<(), StoreError> {
        sqlx::query("UPDATE invitations SET accepted_at = now() WHERE id = $1")
            .bind(invitation_id as i64)
            .execute(self.pool())
            .await
            .map(|_| ())
            .map_err(sqlx_error)
    }

    async fn delete_invitation(
        &self,
        organization_id: Id,
        invitation_id: Id,
    ) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM invitations WHERE id = $1 AND organization_id = $2")
            .bind(invitation_id as i64)
            .bind(organization_id as i64)
            .execute(self.pool())
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(sqlx_error)
    }

    async fn create_password_reset(
        &self,
        user_id: Id,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO password_resets (user_id, token_hash, expires_at) VALUES ($1, $2, $3)",
        )
        .bind(user_id as i64)
        .bind(token_hash)
        .bind(expires_at)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn consume_password_reset(
        &self,
        token_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Id>, StoreError> {
        let row = sqlx::query(
            "DELETE FROM password_resets WHERE token_hash = $1 RETURNING user_id, expires_at",
        )
        .bind(token_hash)
        .fetch_optional(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(row.and_then(|row| {
            let expires_at: DateTime<Utc> = row.get("expires_at");
            if expires_at < now {
                return None;
            }
            Some(row.get::<i64, _>("user_id") as Id)
        }))
    }

    async fn user_by_oidc_sub(&self, sub: &str) -> Result<Option<User>, StoreError> {
        sqlx::query(
            "SELECT u.* FROM users u JOIN user_auth a ON a.user_id = u.id WHERE a.oidc_sub = $1",
        )
        .bind(sub)
        .fetch_optional(self.pool())
        .await
        .map(|row| row.as_ref().map(user_from_row))
        .map_err(sqlx_error)
    }

    async fn set_oidc_sub(&self, user_id: Id, sub: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO user_auth (user_id, oidc_sub, updated_at) VALUES ($1, $2, now())
             ON CONFLICT (user_id) DO UPDATE SET oidc_sub = $2, updated_at = now()",
        )
        .bind(user_id as i64)
        .bind(sub)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn user_by_sso_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<User>, StoreError> {
        sqlx::query(
            "SELECT u.* FROM users u JOIN sso_identities i ON i.user_id = u.id
             WHERE i.provider = $1 AND i.subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(self.pool())
        .await
        .map(|row| row.as_ref().map(user_from_row))
        .map_err(sqlx_error)
    }

    async fn link_sso_identity(
        &self,
        user_id: Id,
        provider: &str,
        subject: &str,
    ) -> Result<(), StoreError> {
        // Upsert per (provider, user): re-linking replaces the subject. A
        // subject held by a *different* user trips the primary key and
        // surfaces as a conflict.
        sqlx::query(
            "INSERT INTO sso_identities (provider, subject, user_id) VALUES ($1, $2, $3)
             ON CONFLICT ON CONSTRAINT sso_identities_provider_user_key
             DO UPDATE SET subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .bind(user_id as i64)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn create_oidc_state(
        &self,
        state: &str,
        pkce_verifier: &str,
        nonce: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO oidc_states (state, pkce_verifier, nonce, expires_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(state)
        .bind(pkce_verifier)
        .bind(nonce)
        .bind(expires_at)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn consume_oidc_state(
        &self,
        state: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<(String, String)>, StoreError> {
        let row = sqlx::query(
            "DELETE FROM oidc_states WHERE state = $1
             RETURNING pkce_verifier, nonce, expires_at",
        )
        .bind(state)
        .fetch_optional(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(row.and_then(|row| {
            let expires_at: DateTime<Utc> = row.get("expires_at");
            if expires_at < now {
                return None;
            }
            Some((row.get("pkce_verifier"), row.get("nonce")))
        }))
    }

    async fn add_webauthn_credential(
        &self,
        new: NewWebAuthnCredential,
    ) -> Result<WebAuthnCredential, StoreError> {
        sqlx::query(
            "INSERT INTO webauthn_credentials (user_id, name, credential_id, credential)
             VALUES ($1, $2, $3, $4) RETURNING *",
        )
        .bind(new.user_id as i64)
        .bind(&new.name)
        .bind(&new.credential_id)
        .bind(&new.credential_json)
        .fetch_one(self.pool())
        .await
        .map(|row| webauthn_credential_from_row(&row))
        .map_err(sqlx_error)
    }

    async fn list_webauthn_credentials(
        &self,
        user_id: Id,
    ) -> Result<Vec<WebAuthnCredential>, StoreError> {
        let rows = sqlx::query("SELECT * FROM webauthn_credentials WHERE user_id = $1 ORDER BY id")
            .bind(user_id as i64)
            .fetch_all(self.pool())
            .await
            .map_err(sqlx_error)?;
        Ok(rows.iter().map(webauthn_credential_from_row).collect())
    }

    async fn webauthn_credential_by_credential_id(
        &self,
        credential_id: &str,
    ) -> Result<Option<WebAuthnCredential>, StoreError> {
        sqlx::query("SELECT * FROM webauthn_credentials WHERE credential_id = $1")
            .bind(credential_id)
            .fetch_optional(self.pool())
            .await
            .map(|row| row.as_ref().map(webauthn_credential_from_row))
            .map_err(sqlx_error)
    }

    async fn update_webauthn_credential(
        &self,
        credential_id: Id,
        credential_json: &str,
        last_used_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE webauthn_credentials SET credential = $2, last_used_at = $3 WHERE id = $1",
        )
        .bind(credential_id as i64)
        .bind(credential_json)
        .bind(last_used_at)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn delete_webauthn_credential(
        &self,
        user_id: Id,
        credential_id: Id,
    ) -> Result<bool, StoreError> {
        sqlx::query("DELETE FROM webauthn_credentials WHERE id = $1 AND user_id = $2")
            .bind(credential_id as i64)
            .bind(user_id as i64)
            .execute(self.pool())
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(sqlx_error)
    }

    async fn create_webauthn_state(
        &self,
        key: &str,
        user_id: Option<Id>,
        state_json: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO webauthn_states (key, user_id, state, expires_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (key)
             DO UPDATE SET user_id = $2, state = $3, expires_at = $4",
        )
        .bind(key)
        .bind(user_id.map(|id| id as i64))
        .bind(state_json)
        .bind(expires_at)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn create_saml_request(
        &self,
        request_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO saml_requests (request_id, expires_at) VALUES ($1, $2)")
            .bind(request_id)
            .bind(expires_at)
            .execute(self.pool())
            .await
            .map(|_| ())
            .map_err(sqlx_error)
    }

    async fn consume_saml_request(
        &self,
        request_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let row =
            sqlx::query("DELETE FROM saml_requests WHERE request_id = $1 RETURNING expires_at")
                .bind(request_id)
                .fetch_optional(self.pool())
                .await
                .map_err(sqlx_error)?;
        Ok(match row {
            Some(row) => row.get::<DateTime<Utc>, _>("expires_at") >= now,
            None => false,
        })
    }

    async fn register_saml_assertion(
        &self,
        assertion_id: &str,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        // Opportunistic cleanup keeps the replay cache from growing.
        sqlx::query("DELETE FROM saml_assertions WHERE expires_at < $1")
            .bind(now)
            .execute(self.pool())
            .await
            .map_err(sqlx_error)?;
        let result = sqlx::query(
            "INSERT INTO saml_assertions (assertion_id, expires_at) VALUES ($1, $2)
             ON CONFLICT (assertion_id) DO NOTHING",
        )
        .bind(assertion_id)
        .bind(expires_at)
        .execute(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_user_disabled(&self, user_id: Id, disabled: bool) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO user_auth (user_id, disabled, updated_at) VALUES ($1, $2, now())
             ON CONFLICT (user_id) DO UPDATE SET disabled = $2, updated_at = now()",
        )
        .bind(user_id as i64)
        .bind(disabled)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn consume_webauthn_state(
        &self,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<(Option<Id>, String)>, StoreError> {
        let row = sqlx::query(
            "DELETE FROM webauthn_states WHERE key = $1
             RETURNING user_id, state, expires_at",
        )
        .bind(key)
        .fetch_optional(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(row.and_then(|row| {
            let expires_at: DateTime<Utc> = row.get("expires_at");
            if expires_at < now {
                return None;
            }
            Some((
                row.get::<Option<i64>, _>("user_id").map(|id| id as Id),
                row.get("state"),
            ))
        }))
    }

    async fn record_auth_event(&self, event: NewAuthEvent) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO auth_events (user_id, email_address, event, ip_address, user_agent)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(event.user_id.map(|id| id as i64))
        .bind(&event.email_address)
        .bind(&event.event)
        .bind(&event.ip_address)
        .bind(&event.user_agent)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(sqlx_error)
    }

    async fn list_auth_events(&self, limit: u64) -> Result<Vec<AuthEvent>, StoreError> {
        let rows = sqlx::query("SELECT * FROM auth_events ORDER BY id DESC LIMIT $1")
            .bind(limit as i64)
            .fetch_all(self.pool())
            .await
            .map_err(sqlx_error)?;
        Ok(rows
            .iter()
            .map(|row| AuthEvent {
                id: row.get::<i64, _>("id") as Id,
                user_id: row.get::<Option<i64>, _>("user_id").map(|id| id as Id),
                email_address: row.get("email_address"),
                event: row.get("event"),
                ip_address: row.get("ip_address"),
                user_agent: row.get("user_agent"),
                created_at: row.get("created_at"),
            })
            .collect())
    }
}
