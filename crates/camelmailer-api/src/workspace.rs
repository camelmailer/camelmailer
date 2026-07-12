//! Workspace bootstrap (`auth.bootstrap_workspace`, meant for the cloud):
//! when a brand-new account is created — via `POST /api/v2/auth/register`
//! or via SSO/SAML/OIDC auto-provisioning — it gets a ready-to-use
//! workspace: an organization "<FirstName>'s Team" with the user as owner
//! and two servers inside it, "production" (Live) and "development"
//! (Development), so the dashboard is usable without a manual setup step.
//!
//! Only the registration path also creates an API credential "default":
//! its response is the one channel where the key can be shown exactly
//! once (the "secrets are shown once" convention). SSO provisioning has
//! no such channel, so no credential — and no key that nobody ever saw —
//! is created there.
//!
//! Bootstrap failures never fail the account creation that triggered
//! them: they are logged via `tracing::warn!` and the user simply starts
//! without a workspace.

use camelmailer_core::{
    CredentialType, NewCredential, NewOrganization, NewServer, Organization, Role, Server,
    ServerMode, StoreError, User,
};

use crate::app::{permalink_from, ApiState};

/// How many permalink candidates (`slug`, `slug-2`, … `slug-50`) are tried
/// before the bootstrap gives up.
const MAX_SLUG_ATTEMPTS: u32 = 50;

pub(crate) struct BootstrappedWorkspace {
    pub(crate) organization: Organization,
    pub(crate) server: Server,
    /// The full API key of the "default" credential. Returned exactly
    /// once in the register response; `None` when bootstrap ran without a
    /// response channel (SSO provisioning).
    pub(crate) api_key: Option<String>,
}

/// Bootstrap a workspace for a freshly created account. Returns `None`
/// when the feature is off — and on any failure, which is logged and
/// deliberately not propagated: the account itself must survive.
pub(crate) async fn bootstrap_workspace(
    state: &ApiState,
    user: &User,
    include_api_key: bool,
) -> Option<BootstrappedWorkspace> {
    if !state.config.auth.bootstrap_workspace {
        return None;
    }
    match try_bootstrap(state, user, include_api_key).await {
        Ok(workspace) => Some(workspace),
        Err(error) => {
            tracing::warn!(
                user = %user.email_address,
                %error,
                "workspace bootstrap failed; the account was created without one"
            );
            None
        }
    }
}

async fn try_bootstrap(
    state: &ApiState,
    user: &User,
    include_api_key: bool,
) -> Result<BootstrappedWorkspace, StoreError> {
    let auth_store = state
        .auth_store
        .as_ref()
        .ok_or_else(|| StoreError::Other("account storage is not available".into()))?;

    // "<FirstName>'s Team" — SSO-provisioned accounts may lack a first
    // name; a name that slugs to nothing falls back to a plain "team".
    let first_name = user.first_name.trim();
    let name = if first_name.is_empty() {
        "My Team".to_string()
    } else {
        format!("{first_name}'s Team")
    };
    let base_slug = match permalink_from(&name) {
        slug if slug.is_empty() => "team".to_string(),
        slug => slug,
    };

    // Permalink collisions get a numeric suffix: slug, slug-2, slug-3, …
    let mut organization = None;
    for attempt in 1..=MAX_SLUG_ATTEMPTS {
        let permalink = if attempt == 1 {
            base_slug.clone()
        } else {
            format!("{base_slug}-{attempt}")
        };
        match state
            .store
            .create_organization(NewOrganization {
                name: name.clone(),
                permalink,
            })
            .await
        {
            Ok(created) => {
                organization = Some(created);
                break;
            }
            Err(StoreError::Conflict(_)) => continue,
            Err(error) => return Err(error),
        }
    }
    let organization = organization.ok_or_else(|| {
        StoreError::Other(format!(
            "no free organization permalink for {base_slug:?} after {MAX_SLUG_ATTEMPTS} attempts"
        ))
    })?;

    auth_store
        .upsert_membership(organization.id, user.id, Role::Owner)
        .await?;

    let server = state
        .store
        .create_server(NewServer {
            organization_id: organization.id,
            name: "production".into(),
            permalink: "production".into(),
            mode: ServerMode::Live,
        })
        .await?;

    // A companion "development" server, so testing and production are
    // separated from the first minute. Best effort: production (which
    // carries the API key and is the one the response points at) is the
    // workspace that must exist, so a dev-server hiccup is logged, not
    // propagated — it would otherwise discard the whole workspace.
    if let Err(error) = state
        .store
        .create_server(NewServer {
            organization_id: organization.id,
            name: "development".into(),
            permalink: "development".into(),
            mode: ServerMode::Development,
        })
        .await
    {
        tracing::warn!(
            organization = %organization.permalink,
            %error,
            "workspace bootstrap created production but not the development server"
        );
    }

    let api_key = if include_api_key {
        let credential = state
            .store
            .create_credential_record(NewCredential {
                server_id: server.id,
                credential_type: CredentialType::Api,
                name: "default".into(),
                key: None,
            })
            .await?;
        Some(credential.key)
    } else {
        None
    };

    Ok(BootstrappedWorkspace {
        organization,
        server,
        api_key,
    })
}
