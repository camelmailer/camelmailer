//! Organization people management: member roles and invitations
//! (`/api/v2/admin/organizations/{permalink}/members|invitations`), plus
//! the global auth audit feed.
//!
//! Role requirements are enforced by the admin auth middleware; the
//! owner-specific guards live here (only owners touch owners, the last
//! owner is immovable).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use camelmailer_core::auth::{self, NewAuthEvent, NewInvitation};
use camelmailer_core::{AuthStore, Role};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::app::{
    find_organization, render_error, render_not_found, render_parameter_missing,
    render_store_error, render_success, render_validation_error, ActingRole, ApiResponse, ApiState,
    PaginationParams, RequestStart,
};
use crate::auth_api::user_json;

fn auth_store(state: &ApiState, start: &RequestStart) -> Result<Arc<dyn AuthStore>, ApiResponse> {
    state.auth_store.clone().ok_or_else(|| {
        render_error(
            Some(start),
            StatusCode::SERVICE_UNAVAILABLE,
            "AccountsUnavailable",
            "User accounts require persistent storage and are not enabled on this instance",
        )
    })
}

fn forbidden(start: &RequestStart, message: &str) -> ApiResponse {
    render_error(Some(start), StatusCode::FORBIDDEN, "Forbidden", message)
}

fn membership_json(membership: &camelmailer_core::OrganizationMembership, user: &Value) -> Value {
    json!({
        "role": membership.role.as_str(),
        "created_at": membership.created_at,
        "user": user,
    })
}

fn invitation_json(invitation: &camelmailer_core::Invitation) -> Value {
    json!({
        "id": invitation.id,
        "uuid": invitation.uuid,
        "email_address": invitation.email_address,
        "role": invitation.role.as_str(),
        "expires_at": invitation.expires_at,
        "accepted_at": invitation.accepted_at,
    })
}

/// How many owners does the organization currently have?
async fn owner_count(store: &Arc<dyn AuthStore>, organization_id: camelmailer_core::Id) -> u64 {
    store
        .memberships_for_organization(organization_id)
        .await
        .map(|members| {
            members
                .iter()
                .filter(|(membership, _)| membership.role == Role::Owner)
                .count() as u64
        })
        .unwrap_or(0)
}

// ------------------------------------------------------------- members

pub(crate) async fn members_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
) -> ApiResponse {
    let store = match auth_store(&state, &start.0) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let organization = match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    match store.memberships_for_organization(organization.id).await {
        Ok(members) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "members": members
                    .iter()
                    .map(|(membership, user)| membership_json(membership, &user_json(user)))
                    .collect::<Vec<_>>(),
            }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct AddMember {
    email_address: Option<String>,
    role: Option<String>,
}

pub(crate) async fn members_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    acting: axum::Extension<ActingRole>,
    Path(permalink): Path<String>,
    Json(body): Json<AddMember>,
) -> ApiResponse {
    let store = match auth_store(&state, &start.0) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let organization = match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some(email) = body.email_address.filter(|e| !e.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: email_address",
        );
    };
    let Some(role) = body.role.as_deref().and_then(Role::parse) else {
        return render_validation_error(
            Some(&start.0),
            "role must be one of viewer, member, admin, owner",
        );
    };
    if role == Role::Owner && !acting.is_owner() {
        return forbidden(&start.0, "Only owners can grant the owner role");
    }
    let user = match store.user_by_email(&email).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            return render_validation_error(
                Some(&start.0),
                "No account exists for this email address — send an invitation instead",
            )
        }
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    if let Ok(Some(_)) = store.membership(organization.id, user.id).await {
        return render_validation_error(
            Some(&start.0),
            "User is already a member of this organization",
        );
    }
    match store
        .upsert_membership(organization.id, user.id, role)
        .await
    {
        Ok(membership) => render_success(
            Some(&start.0),
            StatusCode::CREATED,
            json!({ "member": membership_json(&membership, &user_json(&user)) }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateMember {
    role: Option<String>,
}

pub(crate) async fn members_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    acting: axum::Extension<ActingRole>,
    Path((permalink, user_id)): Path<(String, u64)>,
    Json(body): Json<UpdateMember>,
) -> ApiResponse {
    let store = match auth_store(&state, &start.0) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let organization = match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some(role) = body.role.as_deref().and_then(Role::parse) else {
        return render_validation_error(
            Some(&start.0),
            "role must be one of viewer, member, admin, owner",
        );
    };
    let existing = match store.membership(organization.id, user_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    // Owner transitions are reserved for owners.
    if (role == Role::Owner || existing.role == Role::Owner) && !acting.is_owner() {
        return forbidden(&start.0, "Only owners can change owner roles");
    }
    // The organization must always keep at least one owner.
    if existing.role == Role::Owner
        && role != Role::Owner
        && owner_count(&store, organization.id).await <= 1
    {
        return render_validation_error(
            Some(&start.0),
            "The organization must keep at least one owner",
        );
    }
    let _ = store
        .record_auth_event(NewAuthEvent {
            user_id: Some(user_id),
            email_address: None,
            event: format!("membership.role_change.{}", role.as_str()),
            ip_address: None,
            user_agent: None,
        })
        .await;
    match store
        .upsert_membership(organization.id, user_id, role)
        .await
    {
        Ok(membership) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "member": {
                    "role": membership.role.as_str(),
                    "created_at": membership.created_at,
                    "user_id": membership.user_id,
                },
            }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

pub(crate) async fn members_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    acting: axum::Extension<ActingRole>,
    Path((permalink, user_id)): Path<(String, u64)>,
) -> ApiResponse {
    let store = match auth_store(&state, &start.0) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let organization = match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let existing = match store.membership(organization.id, user_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    if existing.role == Role::Owner {
        if !acting.is_owner() {
            return forbidden(&start.0, "Only owners can remove an owner");
        }
        if owner_count(&store, organization.id).await <= 1 {
            return render_validation_error(
                Some(&start.0),
                "The organization must keep at least one owner",
            );
        }
    }
    match store.delete_membership(organization.id, user_id).await {
        Ok(true) => {
            let _ = store
                .record_auth_event(NewAuthEvent {
                    user_id: Some(user_id),
                    email_address: None,
                    event: "membership.removed".into(),
                    ip_address: None,
                    user_agent: None,
                })
                .await;
            render_success(Some(&start.0), StatusCode::OK, json!({ "deleted": true }))
        }
        Ok(false) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

// --------------------------------------------------------- invitations

pub(crate) async fn invitations_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
) -> ApiResponse {
    let store = match auth_store(&state, &start.0) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let organization = match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    match store.list_invitations(organization.id).await {
        Ok(invitations) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "invitations": invitations.iter().map(invitation_json).collect::<Vec<_>>(),
            }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateInvitation {
    email_address: Option<String>,
    role: Option<String>,
}

pub(crate) async fn invitations_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    acting: axum::Extension<ActingRole>,
    principal: axum::Extension<crate::app::Principal>,
    Path(permalink): Path<String>,
    Json(body): Json<CreateInvitation>,
) -> ApiResponse {
    let store = match auth_store(&state, &start.0) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let organization = match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some(email) = body
        .email_address
        .filter(|e| !e.is_empty() && e.contains('@'))
    else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: email_address",
        );
    };
    let role = match body.role.as_deref() {
        None => Role::Member,
        Some(value) => match Role::parse(value) {
            Some(role) => role,
            None => {
                return render_validation_error(
                    Some(&start.0),
                    "role must be one of viewer, member, admin, owner",
                )
            }
        },
    };
    if role == Role::Owner && !acting.is_owner() {
        return forbidden(&start.0, "Only owners can grant the owner role");
    }
    // Already a member? Point the caller at the members endpoint instead.
    if let Ok(Some(user)) = store.user_by_email(&email).await {
        if let Ok(Some(_)) = store.membership(organization.id, user.id).await {
            return render_validation_error(
                Some(&start.0),
                "User is already a member of this organization",
            );
        }
    }
    let token = auth::generate_auth_token();
    let invited_by = principal.user().map(|user| user.id).unwrap_or(0);
    let invitation = match store
        .create_invitation(NewInvitation {
            organization_id: organization.id,
            email_address: email.clone(),
            role,
            token_hash: auth::hash_token(&token),
            invited_by_user_id: invited_by,
            expires_at: Utc::now()
                + Duration::days(state.config.auth.invitation_expiry_days as i64),
        })
        .await
    {
        Ok(invitation) => invitation,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let _ = store
        .record_auth_event(NewAuthEvent {
            user_id: principal.user().map(|user| user.id),
            email_address: Some(email),
            event: "invitation.created".into(),
            ip_address: None,
            user_agent: None,
        })
        .await;
    // The token is returned exactly once, for the frontend to deliver
    // (it stays in the response even when the invitee is also emailed).
    let invite_url = state.config.auth.frontend_url.as_deref().map(|base| {
        format!(
            "{}/invitations/accept?token={}",
            base.trim_end_matches('/'),
            token
        )
    });
    let mut data = invitation_json(&invitation);
    data["invite_token"] = json!(token);
    if let Some(invite_url) = &invite_url {
        data["invite_url"] = json!(invite_url);
    }
    // Email the accept link to the invitee — a no-op unless app_mail is
    // enabled; a delivery failure is logged and never fails the request.
    if state.config.app_mail.enabled {
        match invite_url.as_deref() {
            Some(link) => {
                crate::app_mailer::deliver(
                    &state,
                    crate::app_mailer::invitation_mail(
                        &invitation.email_address,
                        &organization.name,
                        link,
                        state.config.auth.invitation_expiry_days,
                    ),
                )
                .await;
            }
            None => tracing::warn!(
                "app_mail is enabled but auth.frontend_url is not set; cannot email the invitation link"
            ),
        }
    }
    render_success(
        Some(&start.0),
        StatusCode::CREATED,
        json!({ "invitation": data }),
    )
}

pub(crate) async fn invitations_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((permalink, id)): Path<(String, u64)>,
) -> ApiResponse {
    let store = match auth_store(&state, &start.0) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let organization = match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    match store.delete_invitation(organization.id, id).await {
        Ok(true) => render_success(Some(&start.0), StatusCode::OK, json!({ "deleted": true })),
        Ok(false) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

// --------------------------------------------------------- audit feed

#[derive(Debug, Deserialize, Default)]
pub(crate) struct AuditQuery {
    limit: Option<u64>,
    #[serde(flatten)]
    _pagination: PaginationParams,
}

/// `GET /api/v2/admin/auth_events` — the authentication audit trail.
/// Root-only (enforced by the auth middleware's global-resource rule).
pub(crate) async fn auth_events_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(query): Query<AuditQuery>,
) -> ApiResponse {
    let store = match auth_store(&state, &start.0) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    match store.list_auth_events(limit).await {
        Ok(events) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "auth_events": events
                    .iter()
                    .map(|event| {
                        json!({
                            "id": event.id,
                            "user_id": event.user_id,
                            "email_address": event.email_address,
                            "event": event.event,
                            "ip_address": event.ip_address,
                            "user_agent": event.user_agent,
                            "created_at": event.created_at,
                        })
                    })
                    .collect::<Vec<_>>(),
            }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}
