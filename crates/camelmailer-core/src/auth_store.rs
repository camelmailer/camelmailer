//! The async storage interface behind accounts, sessions, RBAC memberships,
//! invitations and the auth audit log — implemented by
//! [`crate::MemoryStore`] for tests and by the Postgres store in
//! `camelmailer-db` for production.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::admin_store::StoreError;
use crate::auth::{
    AuthEvent, AuthSession, Invitation, NewAuthEvent, NewAuthSession, NewInvitation,
    NewWebAuthnCredential, OrganizationMembership, Role, UserAuth, WebAuthnCredential,
};
use crate::model::{Id, Organization, User};

#[async_trait]
pub trait AuthStore: Send + Sync {
    // -- accounts
    async fn user_by_email(&self, email: &str) -> Result<Option<User>, StoreError>;
    async fn user_auth(&self, user_id: Id) -> Result<Option<UserAuth>, StoreError>;
    async fn set_password_digest(&self, user_id: Id, digest: &str) -> Result<(), StoreError>;
    async fn set_totp(
        &self,
        user_id: Id,
        secret: Option<&str>,
        enabled: bool,
    ) -> Result<(), StoreError>;
    /// Update the login-throttling state (attempt counter, lockout, last
    /// successful login) in one write.
    async fn set_login_state(
        &self,
        user_id: Id,
        failed_attempts: u32,
        locked_until: Option<DateTime<Utc>>,
        last_login_at: Option<DateTime<Utc>>,
    ) -> Result<(), StoreError>;

    // -- sessions
    async fn create_session(&self, new: NewAuthSession) -> Result<AuthSession, StoreError>;
    /// Look up a live session by token hash, joined with its user.
    async fn session_with_user(
        &self,
        token_hash: &str,
    ) -> Result<Option<(AuthSession, User)>, StoreError>;
    /// Slide the session window forward on use.
    async fn touch_session(
        &self,
        session_id: Id,
        last_used_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    async fn delete_session(&self, token_hash: &str) -> Result<bool, StoreError>;
    async fn delete_sessions_for_user(&self, user_id: Id) -> Result<u64, StoreError>;

    // -- memberships (RBAC)
    async fn memberships_for_user(
        &self,
        user_id: Id,
    ) -> Result<Vec<(OrganizationMembership, Organization)>, StoreError>;
    async fn membership(
        &self,
        organization_id: Id,
        user_id: Id,
    ) -> Result<Option<OrganizationMembership>, StoreError>;
    async fn memberships_for_organization(
        &self,
        organization_id: Id,
    ) -> Result<Vec<(OrganizationMembership, User)>, StoreError>;
    async fn upsert_membership(
        &self,
        organization_id: Id,
        user_id: Id,
        role: Role,
    ) -> Result<OrganizationMembership, StoreError>;
    async fn delete_membership(&self, organization_id: Id, user_id: Id)
        -> Result<bool, StoreError>;

    // -- invitations
    async fn create_invitation(&self, new: NewInvitation) -> Result<Invitation, StoreError>;
    async fn list_invitations(&self, organization_id: Id) -> Result<Vec<Invitation>, StoreError>;
    async fn invitation_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<Invitation>, StoreError>;
    async fn mark_invitation_accepted(&self, invitation_id: Id) -> Result<(), StoreError>;
    async fn delete_invitation(
        &self,
        organization_id: Id,
        invitation_id: Id,
    ) -> Result<bool, StoreError>;

    // -- password resets
    async fn create_password_reset(
        &self,
        user_id: Id,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    /// Redeem a reset token: returns the user id and invalidates the token.
    /// Expired or already-used tokens return `None`.
    async fn consume_password_reset(
        &self,
        token_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Id>, StoreError>;

    // -- SSO (OIDC)
    async fn user_by_oidc_sub(&self, sub: &str) -> Result<Option<User>, StoreError>;
    async fn set_oidc_sub(&self, user_id: Id, sub: &str) -> Result<(), StoreError>;

    // -- social SSO identities. One account may hold several (at most one
    // -- per provider); independent of the enterprise `oidc_sub` link.
    async fn user_by_sso_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<User>, StoreError>;
    /// Link `(provider, subject)` to an account. Upserts per provider: a
    /// user re-linking the same provider replaces the previous subject. A
    /// subject already linked to a *different* user is a conflict.
    async fn link_sso_identity(
        &self,
        user_id: Id,
        provider: &str,
        subject: &str,
    ) -> Result<(), StoreError>;

    /// Persist an in-flight OIDC login (state → PKCE verifier + nonce).
    async fn create_oidc_state(
        &self,
        state: &str,
        pkce_verifier: &str,
        nonce: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    /// Redeem an OIDC state: returns `(pkce_verifier, nonce)` and removes
    /// it. Expired or unknown states return `None`.
    async fn consume_oidc_state(
        &self,
        state: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<(String, String)>, StoreError>;

    // -- WebAuthn (passkeys)
    /// Register a passkey. A `credential_id` that is already registered
    /// (on any account) returns [`StoreError::Conflict`].
    async fn add_webauthn_credential(
        &self,
        new: NewWebAuthnCredential,
    ) -> Result<WebAuthnCredential, StoreError>;
    /// A user's passkeys, oldest first.
    async fn list_webauthn_credentials(
        &self,
        user_id: Id,
    ) -> Result<Vec<WebAuthnCredential>, StoreError>;
    /// Look up a passkey by its (base64url) credential id — the login path.
    async fn webauthn_credential_by_credential_id(
        &self,
        credential_id: &str,
    ) -> Result<Option<WebAuthnCredential>, StoreError>;
    /// Persist post-authentication updates (signature counter / backup
    /// flags inside `credential_json`) and stamp `last_used_at`.
    async fn update_webauthn_credential(
        &self,
        credential_id: Id,
        credential_json: &str,
        last_used_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    /// Delete one of `user_id`'s passkeys; `false` when it does not exist
    /// or belongs to someone else.
    async fn delete_webauthn_credential(
        &self,
        user_id: Id,
        credential_id: Id,
    ) -> Result<bool, StoreError>;
    /// Persist an in-flight WebAuthn ceremony (challenge key → serialized
    /// registration/authentication state) — the same server-side,
    /// short-lived, single-use mechanism as
    /// [`create_oidc_state`](AuthStore::create_oidc_state). Re-using a key
    /// replaces the previous state.
    async fn create_webauthn_state(
        &self,
        key: &str,
        user_id: Option<Id>,
        state_json: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    /// Redeem a WebAuthn ceremony state: returns `(user_id, state_json)`
    /// and removes it. Expired or unknown keys return `None`.
    async fn consume_webauthn_state(
        &self,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<(Option<Id>, String)>, StoreError>;
    // -- SSO (SAML)
    /// Persist an in-flight SAML login (the AuthnRequest id the response
    /// must reference via `InResponseTo`).
    async fn create_saml_request(
        &self,
        request_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    /// Redeem a SAML request id — single use. Returns `false` for
    /// unknown, already-consumed or expired ids.
    async fn consume_saml_request(
        &self,
        request_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError>;
    /// Record a consumed SAML assertion id for replay protection until
    /// `expires_at`. Returns `true` when the id is fresh and `false`
    /// when it was already seen (a replay).
    async fn register_saml_assertion(
        &self,
        assertion_id: &str,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError>;

    // -- account state
    /// Deactivate/reactivate an account (SCIM `active`). Disabled
    /// accounts cannot log in or complete password resets.
    async fn set_user_disabled(&self, user_id: Id, disabled: bool) -> Result<(), StoreError>;

    // -- audit log
    async fn record_auth_event(&self, event: NewAuthEvent) -> Result<(), StoreError>;
    /// Most recent events first.
    async fn list_auth_events(&self, limit: u64) -> Result<Vec<AuthEvent>, StoreError>;
}

// ------------------------------------------------------- MemoryStore impl

use crate::store::MemoryStore;

#[async_trait]
impl AuthStore for MemoryStore {
    async fn user_by_email(&self, email: &str) -> Result<Option<User>, StoreError> {
        let inner = self.inner.read().unwrap();
        Ok(inner
            .users
            .values()
            .find(|u| u.email_address.eq_ignore_ascii_case(email))
            .cloned())
    }

    async fn user_auth(&self, user_id: Id) -> Result<Option<UserAuth>, StoreError> {
        let inner = self.inner.read().unwrap();
        if !inner.users.contains_key(&user_id) {
            return Ok(None);
        }
        Ok(Some(inner.user_auth.get(&user_id).cloned().unwrap_or(
            UserAuth {
                user_id,
                ..UserAuth::default()
            },
        )))
    }

    async fn set_password_digest(&self, user_id: Id, digest: &str) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let entry = inner.user_auth.entry(user_id).or_insert_with(|| UserAuth {
            user_id,
            ..UserAuth::default()
        });
        entry.password_digest = Some(digest.to_string());
        Ok(())
    }

    async fn set_totp(
        &self,
        user_id: Id,
        secret: Option<&str>,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let entry = inner.user_auth.entry(user_id).or_insert_with(|| UserAuth {
            user_id,
            ..UserAuth::default()
        });
        entry.totp_secret = secret.map(str::to_string);
        entry.totp_enabled = enabled;
        Ok(())
    }

    async fn set_login_state(
        &self,
        user_id: Id,
        failed_attempts: u32,
        locked_until: Option<DateTime<Utc>>,
        last_login_at: Option<DateTime<Utc>>,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let entry = inner.user_auth.entry(user_id).or_insert_with(|| UserAuth {
            user_id,
            ..UserAuth::default()
        });
        entry.failed_login_attempts = failed_attempts;
        entry.locked_until = locked_until;
        if last_login_at.is_some() {
            entry.last_login_at = last_login_at;
        }
        Ok(())
    }

    async fn create_session(&self, new: NewAuthSession) -> Result<AuthSession, StoreError> {
        let id = self.next_id();
        let now = Utc::now();
        let session = AuthSession {
            id,
            user_id: new.user_id,
            token_hash: new.token_hash,
            created_at: now,
            expires_at: new.expires_at,
            last_used_at: now,
            ip_address: new.ip_address,
            user_agent: new.user_agent,
        };
        self.inner
            .write()
            .unwrap()
            .auth_sessions
            .insert(id, session.clone());
        Ok(session)
    }

    async fn session_with_user(
        &self,
        token_hash: &str,
    ) -> Result<Option<(AuthSession, User)>, StoreError> {
        let inner = self.inner.read().unwrap();
        let Some(session) = inner
            .auth_sessions
            .values()
            .find(|s| s.token_hash == token_hash)
            .cloned()
        else {
            return Ok(None);
        };
        let Some(user) = inner.users.get(&session.user_id).cloned() else {
            return Ok(None);
        };
        Ok(Some((session, user)))
    }

    async fn touch_session(
        &self,
        session_id: Id,
        last_used_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        if let Some(session) = inner.auth_sessions.get_mut(&session_id) {
            session.last_used_at = last_used_at;
            session.expires_at = expires_at;
        }
        Ok(())
    }

    async fn delete_session(&self, token_hash: &str) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let id = inner
            .auth_sessions
            .iter()
            .find(|(_, s)| s.token_hash == token_hash)
            .map(|(id, _)| *id);
        Ok(match id {
            Some(id) => inner.auth_sessions.remove(&id).is_some(),
            None => false,
        })
    }

    async fn delete_sessions_for_user(&self, user_id: Id) -> Result<u64, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let before = inner.auth_sessions.len();
        inner.auth_sessions.retain(|_, s| s.user_id != user_id);
        Ok((before - inner.auth_sessions.len()) as u64)
    }

    async fn memberships_for_user(
        &self,
        user_id: Id,
    ) -> Result<Vec<(OrganizationMembership, Organization)>, StoreError> {
        let inner = self.inner.read().unwrap();
        let mut result: Vec<_> = inner
            .memberships
            .values()
            .filter(|m| m.user_id == user_id)
            .filter_map(|m| {
                inner
                    .organizations
                    .get(&m.organization_id)
                    .map(|org| (m.clone(), org.clone()))
            })
            .collect();
        result.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        Ok(result)
    }

    async fn membership(
        &self,
        organization_id: Id,
        user_id: Id,
    ) -> Result<Option<OrganizationMembership>, StoreError> {
        let inner = self.inner.read().unwrap();
        Ok(inner
            .memberships
            .values()
            .find(|m| m.organization_id == organization_id && m.user_id == user_id)
            .cloned())
    }

    async fn memberships_for_organization(
        &self,
        organization_id: Id,
    ) -> Result<Vec<(OrganizationMembership, User)>, StoreError> {
        let inner = self.inner.read().unwrap();
        let mut result: Vec<_> = inner
            .memberships
            .values()
            .filter(|m| m.organization_id == organization_id)
            .filter_map(|m| {
                inner
                    .users
                    .get(&m.user_id)
                    .map(|user| (m.clone(), user.clone()))
            })
            .collect();
        result.sort_by(|a, b| a.1.email_address.cmp(&b.1.email_address));
        Ok(result)
    }

    async fn upsert_membership(
        &self,
        organization_id: Id,
        user_id: Id,
        role: Role,
    ) -> Result<OrganizationMembership, StoreError> {
        {
            let mut inner = self.inner.write().unwrap();
            if let Some(existing) = inner
                .memberships
                .values_mut()
                .find(|m| m.organization_id == organization_id && m.user_id == user_id)
            {
                existing.role = role;
                return Ok(existing.clone());
            }
        }
        let id = self.next_id();
        let membership = OrganizationMembership {
            id,
            organization_id,
            user_id,
            role,
            created_at: Utc::now(),
        };
        self.inner
            .write()
            .unwrap()
            .memberships
            .insert(id, membership.clone());
        Ok(membership)
    }

    async fn delete_membership(
        &self,
        organization_id: Id,
        user_id: Id,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let before = inner.memberships.len();
        inner
            .memberships
            .retain(|_, m| !(m.organization_id == organization_id && m.user_id == user_id));
        Ok(inner.memberships.len() < before)
    }

    async fn create_invitation(&self, new: NewInvitation) -> Result<Invitation, StoreError> {
        {
            let inner = self.inner.read().unwrap();
            let duplicate = inner.invitations.values().any(|i| {
                i.organization_id == new.organization_id
                    && i.email_address.eq_ignore_ascii_case(&new.email_address)
                    && i.accepted_at.is_none()
            });
            if duplicate {
                return Err(StoreError::Conflict(
                    "An invitation for this email address is already pending".into(),
                ));
            }
        }
        let id = self.next_id();
        let invitation = Invitation {
            id,
            uuid: crate::token::generate_uuid(),
            organization_id: new.organization_id,
            email_address: new.email_address,
            role: new.role,
            token_hash: new.token_hash,
            invited_by_user_id: new.invited_by_user_id,
            expires_at: new.expires_at,
            accepted_at: None,
        };
        self.inner
            .write()
            .unwrap()
            .invitations
            .insert(id, invitation.clone());
        Ok(invitation)
    }

    async fn list_invitations(&self, organization_id: Id) -> Result<Vec<Invitation>, StoreError> {
        let inner = self.inner.read().unwrap();
        let mut result: Vec<_> = inner
            .invitations
            .values()
            .filter(|i| i.organization_id == organization_id)
            .cloned()
            .collect();
        result.sort_by_key(|i| i.id);
        Ok(result)
    }

    async fn invitation_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<Invitation>, StoreError> {
        let inner = self.inner.read().unwrap();
        Ok(inner
            .invitations
            .values()
            .find(|i| i.token_hash == token_hash)
            .cloned())
    }

    async fn mark_invitation_accepted(&self, invitation_id: Id) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        if let Some(invitation) = inner.invitations.get_mut(&invitation_id) {
            invitation.accepted_at = Some(Utc::now());
        }
        Ok(())
    }

    async fn delete_invitation(
        &self,
        organization_id: Id,
        invitation_id: Id,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        match inner.invitations.get(&invitation_id) {
            Some(invitation) if invitation.organization_id == organization_id => {
                inner.invitations.remove(&invitation_id);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn create_password_reset(
        &self,
        user_id: Id,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner.write().unwrap().password_resets.push((
            user_id,
            token_hash.to_string(),
            expires_at,
        ));
        Ok(())
    }

    async fn consume_password_reset(
        &self,
        token_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Id>, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let position = inner
            .password_resets
            .iter()
            .position(|(_, hash, _)| hash == token_hash);
        let Some(position) = position else {
            return Ok(None);
        };
        let (user_id, _, expires_at) = inner.password_resets.remove(position);
        if expires_at < now {
            return Ok(None);
        }
        Ok(Some(user_id))
    }

    async fn user_by_oidc_sub(&self, sub: &str) -> Result<Option<User>, StoreError> {
        let inner = self.inner.read().unwrap();
        let user_id = inner
            .user_auth
            .values()
            .find(|a| a.oidc_sub.as_deref() == Some(sub))
            .map(|a| a.user_id);
        Ok(user_id.and_then(|id| inner.users.get(&id).cloned()))
    }

    async fn set_oidc_sub(&self, user_id: Id, sub: &str) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let entry = inner.user_auth.entry(user_id).or_insert_with(|| UserAuth {
            user_id,
            ..UserAuth::default()
        });
        entry.oidc_sub = Some(sub.to_string());
        Ok(())
    }

    async fn user_by_sso_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<User>, StoreError> {
        let inner = self.inner.read().unwrap();
        Ok(inner
            .sso_identities
            .get(&(provider.to_string(), subject.to_string()))
            .and_then(|user_id| inner.users.get(user_id).cloned()))
    }

    async fn link_sso_identity(
        &self,
        user_id: Id,
        provider: &str,
        subject: &str,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let key = (provider.to_string(), subject.to_string());
        if matches!(inner.sso_identities.get(&key), Some(other) if *other != user_id) {
            return Err(StoreError::Conflict(
                "This SSO identity is already linked to another user".into(),
            ));
        }
        // upsert per provider: drop the user's previous subject first
        inner
            .sso_identities
            .retain(|(existing_provider, _), existing_user| {
                !(*existing_user == user_id && existing_provider == provider)
            });
        inner.sso_identities.insert(key, user_id);
        Ok(())
    }

    async fn create_oidc_state(
        &self,
        state: &str,
        pkce_verifier: &str,
        nonce: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner.write().unwrap().oidc_states.insert(
            state.to_string(),
            (pkce_verifier.to_string(), nonce.to_string(), expires_at),
        );
        Ok(())
    }

    async fn consume_oidc_state(
        &self,
        state: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<(String, String)>, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let Some((verifier, nonce, expires_at)) = inner.oidc_states.remove(state) else {
            return Ok(None);
        };
        if expires_at < now {
            return Ok(None);
        }
        Ok(Some((verifier, nonce)))
    }

    async fn add_webauthn_credential(
        &self,
        new: NewWebAuthnCredential,
    ) -> Result<WebAuthnCredential, StoreError> {
        {
            let inner = self.inner.read().unwrap();
            let duplicate = inner
                .webauthn_credentials
                .values()
                .any(|credential| credential.credential_id == new.credential_id);
            if duplicate {
                return Err(StoreError::Conflict(
                    "This passkey is already registered".into(),
                ));
            }
        }
        let id = self.next_id();
        let credential = WebAuthnCredential {
            id,
            user_id: new.user_id,
            name: new.name,
            credential_id: new.credential_id,
            credential_json: new.credential_json,
            created_at: Utc::now(),
            last_used_at: None,
        };
        self.inner
            .write()
            .unwrap()
            .webauthn_credentials
            .insert(id, credential.clone());
        Ok(credential)
    }

    async fn list_webauthn_credentials(
        &self,
        user_id: Id,
    ) -> Result<Vec<WebAuthnCredential>, StoreError> {
        let inner = self.inner.read().unwrap();
        let mut result: Vec<_> = inner
            .webauthn_credentials
            .values()
            .filter(|credential| credential.user_id == user_id)
            .cloned()
            .collect();
        result.sort_by_key(|credential| credential.id);
        Ok(result)
    }

    async fn webauthn_credential_by_credential_id(
        &self,
        credential_id: &str,
    ) -> Result<Option<WebAuthnCredential>, StoreError> {
        let inner = self.inner.read().unwrap();
        Ok(inner
            .webauthn_credentials
            .values()
            .find(|credential| credential.credential_id == credential_id)
            .cloned())
    }

    async fn update_webauthn_credential(
        &self,
        credential_id: Id,
        credential_json: &str,
        last_used_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        if let Some(credential) = inner.webauthn_credentials.get_mut(&credential_id) {
            credential.credential_json = credential_json.to_string();
            credential.last_used_at = Some(last_used_at);
        }
        Ok(())
    }

    async fn delete_webauthn_credential(
        &self,
        user_id: Id,
        credential_id: Id,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        match inner.webauthn_credentials.get(&credential_id) {
            Some(credential) if credential.user_id == user_id => {
                inner.webauthn_credentials.remove(&credential_id);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn create_webauthn_state(
        &self,
        key: &str,
        user_id: Option<Id>,
        state_json: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner.write().unwrap().webauthn_states.insert(
            key.to_string(),
            (user_id, state_json.to_string(), expires_at),
        );
        Ok(())
    }

    async fn consume_webauthn_state(
        &self,
        key: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<(Option<Id>, String)>, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let Some((user_id, state_json, expires_at)) = inner.webauthn_states.remove(key) else {
            return Ok(None);
        };
        if expires_at < now {
            return Ok(None);
        }
        Ok(Some((user_id, state_json)))
    }
    async fn create_saml_request(
        &self,
        request_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.inner
            .write()
            .unwrap()
            .saml_requests
            .insert(request_id.to_string(), expires_at);
        Ok(())
    }

    async fn consume_saml_request(
        &self,
        request_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        match inner.saml_requests.remove(request_id) {
            Some(expires_at) => Ok(expires_at >= now),
            None => Ok(false),
        }
    }

    async fn register_saml_assertion(
        &self,
        assertion_id: &str,
        expires_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        inner.saml_assertions.retain(|_, expiry| *expiry >= now);
        if inner.saml_assertions.contains_key(assertion_id) {
            return Ok(false);
        }
        inner
            .saml_assertions
            .insert(assertion_id.to_string(), expires_at);
        Ok(true)
    }

    async fn set_user_disabled(&self, user_id: Id, disabled: bool) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let entry = inner.user_auth.entry(user_id).or_insert_with(|| UserAuth {
            user_id,
            ..UserAuth::default()
        });
        entry.disabled = disabled;
        Ok(())
    }
    async fn record_auth_event(&self, event: NewAuthEvent) -> Result<(), StoreError> {
        let id = self.next_id();
        self.inner.write().unwrap().auth_events.push(AuthEvent {
            id,
            user_id: event.user_id,
            email_address: event.email_address,
            event: event.event,
            ip_address: event.ip_address,
            user_agent: event.user_agent,
            created_at: Utc::now(),
        });
        Ok(())
    }

    async fn list_auth_events(&self, limit: u64) -> Result<Vec<AuthEvent>, StoreError> {
        let inner = self.inner.read().unwrap();
        Ok(inner
            .auth_events
            .iter()
            .rev()
            .take(limit as usize)
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::User;
    use chrono::Duration;

    fn store_with_user(email: &str) -> (MemoryStore, User) {
        let store = MemoryStore::new();
        let id = store.next_id();
        let user = User {
            id,
            uuid: crate::token::generate_uuid(),
            email_address: email.to_string(),
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            admin: false,
        };
        store.inner.write().unwrap().users.insert(id, user.clone());
        (store, user)
    }

    fn org(store: &MemoryStore, name: &str) -> Organization {
        let id = store.next_id();
        let organization = Organization {
            id,
            uuid: crate::token::generate_uuid(),
            name: name.to_string(),
            permalink: name.to_lowercase(),
        };
        store
            .inner
            .write()
            .unwrap()
            .organizations
            .insert(id, organization.clone());
        organization
    }

    #[tokio::test]
    async fn sso_identities_link_look_up_and_upsert_per_provider() {
        let (store, user) = store_with_user("ada@example.com");
        assert!(store
            .user_by_sso_identity("google", "g-1")
            .await
            .unwrap()
            .is_none());

        // one identity per provider, several providers per account
        store
            .link_sso_identity(user.id, "google", "g-1")
            .await
            .unwrap();
        store
            .link_sso_identity(user.id, "github", "h-1")
            .await
            .unwrap();
        assert_eq!(
            store
                .user_by_sso_identity("google", "g-1")
                .await
                .unwrap()
                .unwrap()
                .id,
            user.id
        );
        assert_eq!(
            store
                .user_by_sso_identity("github", "h-1")
                .await
                .unwrap()
                .unwrap()
                .id,
            user.id
        );
        // providers are separate namespaces
        assert!(store
            .user_by_sso_identity("github", "g-1")
            .await
            .unwrap()
            .is_none());

        // re-linking the same provider replaces the old subject
        store
            .link_sso_identity(user.id, "google", "g-2")
            .await
            .unwrap();
        assert!(store
            .user_by_sso_identity("google", "g-1")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .user_by_sso_identity("google", "g-2")
                .await
                .unwrap()
                .unwrap()
                .id,
            user.id
        );
        // linking is idempotent for the same pair
        store
            .link_sso_identity(user.id, "google", "g-2")
            .await
            .unwrap();

        // a subject already linked to another account is a conflict
        let other_id = store.next_id();
        let error = store
            .link_sso_identity(other_id, "google", "g-2")
            .await
            .unwrap_err();
        assert!(matches!(error, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn user_lookup_is_case_insensitive_and_auth_defaults_exist() {
        let (store, user) = store_with_user("Ada@Example.com");
        let found = store.user_by_email("ada@example.COM").await.unwrap();
        assert_eq!(found.unwrap().id, user.id);
        let auth = store.user_auth(user.id).await.unwrap().unwrap();
        assert_eq!(auth.password_digest, None);
        assert!(!auth.totp_enabled);
        assert_eq!(store.user_auth(9999).await.unwrap(), None);
    }

    #[tokio::test]
    async fn password_and_totp_state_round_trip() {
        let (store, user) = store_with_user("a@example.com");
        store
            .set_password_digest(user.id, "$digest$")
            .await
            .unwrap();
        store.set_totp(user.id, Some("SECRET"), true).await.unwrap();
        let auth = store.user_auth(user.id).await.unwrap().unwrap();
        assert_eq!(auth.password_digest.as_deref(), Some("$digest$"));
        assert_eq!(auth.totp_secret.as_deref(), Some("SECRET"));
        assert!(auth.totp_enabled);
        store.set_totp(user.id, None, false).await.unwrap();
        let auth = store.user_auth(user.id).await.unwrap().unwrap();
        assert_eq!(auth.totp_secret, None);
    }

    #[tokio::test]
    async fn sessions_create_look_up_and_delete() {
        let (store, user) = store_with_user("a@example.com");
        let expires = Utc::now() + Duration::days(14);
        let session = store
            .create_session(NewAuthSession {
                user_id: user.id,
                token_hash: "hash-1".into(),
                expires_at: expires,
                ip_address: Some("10.0.0.1".into()),
                user_agent: Some("test".into()),
            })
            .await
            .unwrap();
        let (found, found_user) = store.session_with_user("hash-1").await.unwrap().unwrap();
        assert_eq!(found.id, session.id);
        assert_eq!(found_user.id, user.id);
        assert_eq!(store.session_with_user("nope").await.unwrap(), None);

        let new_expiry = expires + Duration::days(1);
        store
            .touch_session(session.id, Utc::now(), new_expiry)
            .await
            .unwrap();
        let (touched, _) = store.session_with_user("hash-1").await.unwrap().unwrap();
        assert_eq!(touched.expires_at, new_expiry);

        assert!(store.delete_session("hash-1").await.unwrap());
        assert!(!store.delete_session("hash-1").await.unwrap());
    }

    #[tokio::test]
    async fn delete_sessions_for_user_removes_all() {
        let (store, user) = store_with_user("a@example.com");
        for n in 0..3 {
            store
                .create_session(NewAuthSession {
                    user_id: user.id,
                    token_hash: format!("hash-{n}"),
                    expires_at: Utc::now() + Duration::days(1),
                    ip_address: None,
                    user_agent: None,
                })
                .await
                .unwrap();
        }
        assert_eq!(store.delete_sessions_for_user(user.id).await.unwrap(), 3);
        assert_eq!(store.session_with_user("hash-0").await.unwrap(), None);
    }

    #[tokio::test]
    async fn memberships_upsert_and_scope() {
        let (store, user) = store_with_user("a@example.com");
        let acme = org(&store, "Acme");
        let beta = org(&store, "Beta");

        let membership = store
            .upsert_membership(acme.id, user.id, Role::Member)
            .await
            .unwrap();
        assert_eq!(membership.role, Role::Member);
        // upsert updates the role in place
        let updated = store
            .upsert_membership(acme.id, user.id, Role::Owner)
            .await
            .unwrap();
        assert_eq!(updated.id, membership.id);
        assert_eq!(updated.role, Role::Owner);

        store
            .upsert_membership(beta.id, user.id, Role::Viewer)
            .await
            .unwrap();
        let mine = store.memberships_for_user(user.id).await.unwrap();
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].1.name, "Acme");

        let members = store.memberships_for_organization(acme.id).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].1.id, user.id);

        assert!(store.membership(acme.id, user.id).await.unwrap().is_some());
        assert!(store.delete_membership(acme.id, user.id).await.unwrap());
        assert!(!store.delete_membership(acme.id, user.id).await.unwrap());
    }

    #[tokio::test]
    async fn invitations_conflict_on_pending_duplicates_and_accept() {
        let (store, user) = store_with_user("owner@example.com");
        let acme = org(&store, "Acme");
        let new = |email: &str, hash: &str| NewInvitation {
            organization_id: acme.id,
            email_address: email.to_string(),
            role: Role::Member,
            token_hash: hash.to_string(),
            invited_by_user_id: user.id,
            expires_at: Utc::now() + Duration::days(7),
        };
        let invitation = store
            .create_invitation(new("new@example.com", "h1"))
            .await
            .unwrap();
        assert!(matches!(
            store.create_invitation(new("NEW@example.com", "h2")).await,
            Err(StoreError::Conflict(_))
        ));
        let found = store.invitation_by_token_hash("h1").await.unwrap().unwrap();
        assert_eq!(found.id, invitation.id);
        store.mark_invitation_accepted(invitation.id).await.unwrap();
        // once accepted, a fresh invitation may be issued
        store
            .create_invitation(new("new@example.com", "h3"))
            .await
            .unwrap();
        assert_eq!(store.list_invitations(acme.id).await.unwrap().len(), 2);
        assert!(!store.delete_invitation(999, invitation.id).await.unwrap());
        assert!(store
            .delete_invitation(acme.id, invitation.id)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn password_resets_are_single_use_and_expire() {
        let (store, user) = store_with_user("a@example.com");
        store
            .create_password_reset(user.id, "reset-1", Utc::now() + Duration::hours(2))
            .await
            .unwrap();
        assert_eq!(
            store
                .consume_password_reset("reset-1", Utc::now())
                .await
                .unwrap(),
            Some(user.id)
        );
        // single use
        assert_eq!(
            store
                .consume_password_reset("reset-1", Utc::now())
                .await
                .unwrap(),
            None
        );
        // expired
        store
            .create_password_reset(user.id, "reset-2", Utc::now() - Duration::hours(1))
            .await
            .unwrap();
        assert_eq!(
            store
                .consume_password_reset("reset-2", Utc::now())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn oidc_sub_linking_and_states() {
        let (store, user) = store_with_user("a@example.com");
        assert_eq!(store.user_by_oidc_sub("sub-1").await.unwrap(), None);
        store.set_oidc_sub(user.id, "sub-1").await.unwrap();
        assert_eq!(
            store.user_by_oidc_sub("sub-1").await.unwrap().unwrap().id,
            user.id
        );

        store
            .create_oidc_state(
                "state-1",
                "verifier",
                "nonce",
                Utc::now() + Duration::minutes(10),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .consume_oidc_state("state-1", Utc::now())
                .await
                .unwrap(),
            Some(("verifier".into(), "nonce".into()))
        );
        // single use
        assert_eq!(
            store
                .consume_oidc_state("state-1", Utc::now())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn webauthn_credentials_add_list_lookup_update_delete() {
        let (store, user) = store_with_user("a@example.com");
        let other = {
            let id = store.next_id();
            let other = User {
                id,
                uuid: crate::token::generate_uuid(),
                email_address: "b@example.com".into(),
                first_name: "Grace".into(),
                last_name: "Hopper".into(),
                admin: false,
            };
            store.inner.write().unwrap().users.insert(id, other.clone());
            other
        };
        let new = |user_id, name: &str, credential_id: &str| NewWebAuthnCredential {
            user_id,
            name: name.to_string(),
            credential_id: credential_id.to_string(),
            credential_json: "{\"cred\":\"data\"}".to_string(),
        };

        let credential = store
            .add_webauthn_credential(new(user.id, "MacBook", "cred-a"))
            .await
            .unwrap();
        assert_eq!(credential.user_id, user.id);
        assert_eq!(credential.name, "MacBook");
        assert_eq!(credential.last_used_at, None);
        store
            .add_webauthn_credential(new(user.id, "YubiKey", "cred-b"))
            .await
            .unwrap();
        // duplicate credential id (even on another user) conflicts
        assert!(matches!(
            store
                .add_webauthn_credential(new(other.id, "Clone", "cred-a"))
                .await,
            Err(StoreError::Conflict(_))
        ));

        let mine = store.list_webauthn_credentials(user.id).await.unwrap();
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].name, "MacBook");
        assert_eq!(mine[1].name, "YubiKey");
        assert!(store
            .list_webauthn_credentials(other.id)
            .await
            .unwrap()
            .is_empty());

        let found = store
            .webauthn_credential_by_credential_id("cred-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.id, credential.id);
        assert_eq!(
            store
                .webauthn_credential_by_credential_id("nope")
                .await
                .unwrap(),
            None
        );

        let used_at = Utc::now();
        store
            .update_webauthn_credential(credential.id, "{\"cred\":\"updated\"}", used_at)
            .await
            .unwrap();
        let found = store
            .webauthn_credential_by_credential_id("cred-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.credential_json, "{\"cred\":\"updated\"}");
        assert_eq!(found.last_used_at, Some(used_at));

        // deletion is scoped to the owner
        assert!(!store
            .delete_webauthn_credential(other.id, credential.id)
            .await
            .unwrap());
        assert!(store
            .delete_webauthn_credential(user.id, credential.id)
            .await
            .unwrap());
        assert!(!store
            .delete_webauthn_credential(user.id, credential.id)
            .await
            .unwrap());
        assert_eq!(
            store
                .list_webauthn_credentials(user.id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn webauthn_states_are_single_use_expiring_and_replaceable() {
        let (store, user) = store_with_user("a@example.com");
        store
            .create_webauthn_state(
                "login:chal-1",
                Some(user.id),
                "{\"state\":1}",
                Utc::now() + Duration::minutes(5),
            )
            .await
            .unwrap();
        // re-using the key replaces the state
        store
            .create_webauthn_state(
                "login:chal-1",
                Some(user.id),
                "{\"state\":2}",
                Utc::now() + Duration::minutes(5),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .consume_webauthn_state("login:chal-1", Utc::now())
                .await
                .unwrap(),
            Some((Some(user.id), "{\"state\":2}".into()))
        );
        // single use
        assert_eq!(
            store
                .consume_webauthn_state("login:chal-1", Utc::now())
                .await
                .unwrap(),
            None
        );
        // expired
        store
            .create_webauthn_state("reg:chal-2", None, "{}", Utc::now() - Duration::minutes(1))
            .await
            .unwrap();
        assert_eq!(
            store
                .consume_webauthn_state("reg:chal-2", Utc::now())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn saml_requests_are_single_use_and_expire() {
        let (store, _user) = store_with_user("a@example.com");
        store
            .create_saml_request("_req-1", Utc::now() + Duration::minutes(10))
            .await
            .unwrap();
        assert!(store
            .consume_saml_request("_req-1", Utc::now())
            .await
            .unwrap());
        // single use
        assert!(!store
            .consume_saml_request("_req-1", Utc::now())
            .await
            .unwrap());
        // unknown
        assert!(!store
            .consume_saml_request("_nope", Utc::now())
            .await
            .unwrap());
        // expired
        store
            .create_saml_request("_req-2", Utc::now() - Duration::minutes(1))
            .await
            .unwrap();
        assert!(!store
            .consume_saml_request("_req-2", Utc::now())
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn saml_assertion_replay_cache_rejects_repeats() {
        let (store, _user) = store_with_user("a@example.com");
        let expires = Utc::now() + Duration::minutes(5);
        assert!(store
            .register_saml_assertion("_a1", expires, Utc::now())
            .await
            .unwrap());
        // replay
        assert!(!store
            .register_saml_assertion("_a1", expires, Utc::now())
            .await
            .unwrap());
        // a different assertion is fine
        assert!(store
            .register_saml_assertion("_a2", expires, Utc::now())
            .await
            .unwrap());
        // once the original expires, the id may be seen again
        assert!(store
            .register_saml_assertion("_a1", expires, Utc::now() + Duration::minutes(6))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn user_disabled_flag_round_trips() {
        let (store, user) = store_with_user("a@example.com");
        assert!(!store.user_auth(user.id).await.unwrap().unwrap().disabled);
        store.set_user_disabled(user.id, true).await.unwrap();
        assert!(store.user_auth(user.id).await.unwrap().unwrap().disabled);
        store.set_user_disabled(user.id, false).await.unwrap();
        assert!(!store.user_auth(user.id).await.unwrap().unwrap().disabled);
    }

    #[tokio::test]
    async fn auth_events_list_most_recent_first() {
        let (store, user) = store_with_user("a@example.com");
        for event in ["login.success", "login.failure", "logout"] {
            store
                .record_auth_event(NewAuthEvent {
                    user_id: Some(user.id),
                    email_address: Some(user.email_address.clone()),
                    event: event.into(),
                    ip_address: None,
                    user_agent: None,
                })
                .await
                .unwrap();
        }
        let events = store.list_auth_events(2).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, "logout");
        assert_eq!(events[1].event, "login.failure");
    }
}
