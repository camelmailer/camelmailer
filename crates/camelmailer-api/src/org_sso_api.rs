//! Tenant SSO configuration API
//! (`/api/v2/admin/organizations/{permalink}/sso/…`): the email domains an
//! organization uses to route logins, and its OIDC / SAML / social
//! connections. Admin+ only (enforced centrally by `required_role`).
//!
//! Connection secrets (`client_secret`, `secret`) never leave the server
//! in clear: reads return them masked, and a write that echoes the mask
//! back keeps the stored secret.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use camelmailer_core::{
    token, Id, NewOrgEmailDomain, NewOrgSsoConnection, OrgSsoConnection, OrgSsoConnectionUpdate,
    OrgSsoStore, Organization, Role, SsoKind,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::app::{
    find_organization, render_deleted, render_error, render_not_found, render_store_error,
    render_success, render_validation_error, ApiResponse, ApiState, Principal, RequestStart,
};

const MASK: &str = "••••••••";
const SECRET_FIELDS: [&str; 2] = ["client_secret", "secret"];

fn sso_store(
    state: &ApiState,
    start: &RequestStart,
) -> Result<Arc<dyn OrgSsoStore>, Box<ApiResponse>> {
    state.org_sso_store.clone().ok_or_else(|| {
        Box::new(render_error(
            Some(start),
            StatusCode::SERVICE_UNAVAILABLE,
            "AccountsUnavailable",
            "Single sign-on configuration requires persistent storage and is not enabled on this instance",
        ))
    })
}

async fn require_org(
    state: &ApiState,
    start: &RequestStart,
    permalink: &str,
) -> Result<Organization, Box<ApiResponse>> {
    match find_organization(state, permalink).await {
        Ok(Some(org)) => Ok(org),
        Ok(None) => Err(Box::new(render_not_found(Some(start)))),
        Err(error) => Err(Box::new(render_store_error(Some(start), error))),
    }
}

/// Replace non-empty secret values with a mask, for reads.
fn mask_config(config: &Value) -> Value {
    let mut config = config.clone();
    if let Some(object) = config.as_object_mut() {
        for field in SECRET_FIELDS {
            if let Some(value) = object.get_mut(field) {
                if value.as_str().is_some_and(|s| !s.is_empty()) {
                    *value = Value::String(MASK.into());
                }
            }
        }
    }
    config
}

/// Merge an incoming (possibly masked) config over the stored one: a secret
/// field left empty, absent, or still at the mask keeps the stored secret.
fn merge_secrets(mut incoming: Value, stored: &Value) -> Value {
    if let (Some(object), Some(stored_object)) = (incoming.as_object_mut(), stored.as_object()) {
        for field in SECRET_FIELDS {
            let keep = match object.get(field) {
                None => true,
                Some(Value::String(s)) => s.is_empty() || s == MASK,
                _ => false,
            };
            if keep {
                match stored_object.get(field) {
                    Some(secret) => {
                        object.insert(field.to_string(), secret.clone());
                    }
                    None => {
                        object.remove(field);
                    }
                }
            }
        }
    }
    incoming
}

fn domain_json(domain: &camelmailer_core::OrgEmailDomain, dns: &camelmailer_config::Dns) -> Value {
    json!({
        "id": domain.id,
        "domain": domain.domain,
        "verified": domain.verified,
        "created_at": domain.created_at,
        "dns_record": {
            "name": format!("{}.{}", dns.verification_record_label, domain.domain),
            "type": "TXT",
            "value": format!("{}={}", dns.verification_value_prefix, domain.verification_token),
        },
    })
}

fn connection_json(connection: &OrgSsoConnection) -> Value {
    json!({
        "id": connection.id,
        "kind": connection.kind.as_str(),
        "name": connection.name,
        "enabled": connection.enabled,
        "config": mask_config(&connection.config),
        "default_role": connection.default_role.as_str(),
        "auto_provision": connection.auto_provision,
        "created_at": connection.created_at,
    })
}

// -------------------------------------------------------------- domains

pub(crate) async fn sso_domains_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
) -> ApiResponse {
    let store = match sso_store(&state, &start) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let org = match require_org(&state, &start, &permalink).await {
        Ok(org) => org,
        Err(response) => return *response,
    };
    match store.list_org_email_domains(org.id).await {
        Ok(domains) => render_success(
            Some(&start),
            StatusCode::OK,
            json!({ "domains": domains.iter().map(|d| domain_json(d, &state.config.dns)).collect::<Vec<_>>() }),
        ),
        Err(error) => render_store_error(Some(&start), error),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateDomain {
    domain: String,
}

pub(crate) async fn sso_domains_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
    Json(body): Json<CreateDomain>,
) -> ApiResponse {
    let store = match sso_store(&state, &start) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let org = match require_org(&state, &start, &permalink).await {
        Ok(org) => org,
        Err(response) => return *response,
    };
    let new = NewOrgEmailDomain {
        organization_id: org.id,
        domain: body.domain,
        verification_token: token::generate_token(32),
    };
    match store.create_org_email_domain(new).await {
        Ok(domain) => render_success(
            Some(&start),
            StatusCode::CREATED,
            json!({ "domain": domain_json(&domain, &state.config.dns) }),
        ),
        Err(error) => render_store_error(Some(&start), error),
    }
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct VerifyDomain {
    force: Option<bool>,
}

pub(crate) async fn sso_domains_verify(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path((permalink, id)): Path<(String, Id)>,
    body: Option<Json<VerifyDomain>>,
) -> ApiResponse {
    let store = match sso_store(&state, &start) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let org = match require_org(&state, &start, &permalink).await {
        Ok(org) => org,
        Err(response) => return *response,
    };
    let domain = match store.org_email_domain(id).await {
        Ok(Some(domain)) if domain.organization_id == org.id => domain,
        Ok(_) => return render_not_found(Some(&start)),
        Err(error) => return render_store_error(Some(&start), error),
    };

    let force = body
        .map(|Json(b)| b.force.unwrap_or(false))
        .unwrap_or(false);
    if force {
        if !matches!(principal.0, Principal::AdminKey(_)) {
            return render_error(
                Some(&start),
                StatusCode::FORBIDDEN,
                "Forbidden",
                "Forced verification requires the X-Admin-API-Key machine key",
            );
        }
    } else {
        let dns = &state.config.dns;
        let record_name = format!("{}.{}", dns.verification_record_label, domain.domain);
        let expected = format!(
            "{}={}",
            dns.verification_value_prefix, domain.verification_token
        );
        match state.dns_resolver.txt_records(&record_name).await {
            Ok(records) if records.iter().any(|record| record.trim() == expected) => {}
            Ok(_) => {
                return render_validation_error(
                    Some(&start),
                    &format!(
                        "Domain ownership is not proven yet: publish a TXT record at \
                         {record_name} with the value \"{expected}\", wait for DNS to \
                         propagate, then retry"
                    ),
                )
            }
            Err(error) => {
                return render_validation_error(
                    Some(&start),
                    &format!("Could not check the TXT record at {record_name}: {error}"),
                )
            }
        }
    }

    if let Err(error) = store.mark_org_email_domain_verified(domain.id).await {
        return render_store_error(Some(&start), error);
    }
    let refreshed = store
        .org_email_domain(domain.id)
        .await
        .ok()
        .flatten()
        .unwrap_or(domain);
    render_success(
        Some(&start),
        StatusCode::OK,
        json!({ "domain": domain_json(&refreshed, &state.config.dns) }),
    )
}

pub(crate) async fn sso_domains_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((permalink, id)): Path<(String, Id)>,
) -> ApiResponse {
    let store = match sso_store(&state, &start) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let org = match require_org(&state, &start, &permalink).await {
        Ok(org) => org,
        Err(response) => return *response,
    };
    match store.org_email_domain(id).await {
        Ok(Some(domain)) if domain.organization_id == org.id => {}
        Ok(_) => return render_not_found(Some(&start)),
        Err(error) => return render_store_error(Some(&start), error),
    }
    match store.delete_org_email_domain(id).await {
        Ok(_) => render_deleted(Some(&start)),
        Err(error) => render_store_error(Some(&start), error),
    }
}

// ----------------------------------------------------------- connections

pub(crate) async fn sso_connections_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
) -> ApiResponse {
    let store = match sso_store(&state, &start) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let org = match require_org(&state, &start, &permalink).await {
        Ok(org) => org,
        Err(response) => return *response,
    };
    match store.list_org_sso_connections(org.id).await {
        Ok(connections) => render_success(
            Some(&start),
            StatusCode::OK,
            json!({ "connections": connections.iter().map(connection_json).collect::<Vec<_>>() }),
        ),
        Err(error) => render_store_error(Some(&start), error),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateConnection {
    kind: String,
    name: String,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    config: Value,
    #[serde(default)]
    default_role: Option<String>,
    #[serde(default)]
    auto_provision: Option<bool>,
}

fn parse_role(value: Option<&str>) -> Result<Role, String> {
    match value {
        None => Ok(Role::Member),
        Some(raw) => Role::parse(raw).ok_or_else(|| format!("Unknown role \"{raw}\"")),
    }
}

pub(crate) async fn sso_connections_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
    Json(body): Json<CreateConnection>,
) -> ApiResponse {
    let store = match sso_store(&state, &start) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let org = match require_org(&state, &start, &permalink).await {
        Ok(org) => org,
        Err(response) => return *response,
    };
    let Some(kind) = SsoKind::parse(&body.kind) else {
        return render_validation_error(
            Some(&start),
            "kind must be one of oidc, saml, google, microsoft, github",
        );
    };
    if body.name.trim().is_empty() {
        return render_validation_error(Some(&start), "name is required");
    }
    let default_role = match parse_role(body.default_role.as_deref()) {
        Ok(role) => role,
        Err(message) => return render_validation_error(Some(&start), &message),
    };
    if let Some(message) = validate_config(kind, &body.config) {
        return render_validation_error(Some(&start), &message);
    }
    let new = NewOrgSsoConnection {
        organization_id: org.id,
        kind,
        name: body.name,
        enabled: body.enabled.unwrap_or(true),
        config: body.config,
        default_role,
        auto_provision: body.auto_provision.unwrap_or(true),
    };
    match store.create_org_sso_connection(new).await {
        Ok(connection) => render_success(
            Some(&start),
            StatusCode::CREATED,
            json!({ "connection": connection_json(&connection) }),
        ),
        Err(error) => render_store_error(Some(&start), error),
    }
}

/// The fields each protocol needs before it can drive a login.
fn validate_config(kind: SsoKind, config: &Value) -> Option<String> {
    let has = |key: &str| {
        config
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    };
    let missing = |fields: &[&str]| {
        fields
            .iter()
            .find(|field| !has(field))
            .map(|field| format!("{} requires a non-empty \"{field}\"", kind.as_str()))
    };
    match kind {
        SsoKind::Oidc => missing(&["issuer", "client_id"]),
        SsoKind::Saml => missing(&["idp_sso_url"]),
        SsoKind::Google | SsoKind::Microsoft | SsoKind::Github => missing(&["client_id"]),
    }
}

async fn require_connection(
    store: &Arc<dyn OrgSsoStore>,
    start: &RequestStart,
    org_id: Id,
    id: Id,
) -> Result<OrgSsoConnection, Box<ApiResponse>> {
    match store.org_sso_connection(id).await {
        Ok(Some(connection)) if connection.organization_id == org_id => Ok(connection),
        Ok(_) => Err(Box::new(render_not_found(Some(start)))),
        Err(error) => Err(Box::new(render_store_error(Some(start), error))),
    }
}

pub(crate) async fn sso_connections_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((permalink, id)): Path<(String, Id)>,
) -> ApiResponse {
    let store = match sso_store(&state, &start) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let org = match require_org(&state, &start, &permalink).await {
        Ok(org) => org,
        Err(response) => return *response,
    };
    match require_connection(&store, &start, org.id, id).await {
        Ok(connection) => render_success(
            Some(&start),
            StatusCode::OK,
            json!({ "connection": connection_json(&connection) }),
        ),
        Err(response) => *response,
    }
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct UpdateConnection {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    default_role: Option<String>,
    #[serde(default)]
    auto_provision: Option<bool>,
}

pub(crate) async fn sso_connections_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((permalink, id)): Path<(String, Id)>,
    Json(body): Json<UpdateConnection>,
) -> ApiResponse {
    let store = match sso_store(&state, &start) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let org = match require_org(&state, &start, &permalink).await {
        Ok(org) => org,
        Err(response) => return *response,
    };
    let existing = match require_connection(&store, &start, org.id, id).await {
        Ok(connection) => connection,
        Err(response) => return *response,
    };
    let default_role = match body.default_role.as_deref() {
        None => None,
        Some(raw) => match Role::parse(raw) {
            Some(role) => Some(role),
            None => {
                return render_validation_error(Some(&start), &format!("Unknown role \"{raw}\""))
            }
        },
    };
    // Merge secrets so an unchanged, masked config keeps its stored values,
    // then validate the effective config.
    let merged_config = match body.config {
        Some(incoming) => {
            let merged = merge_secrets(incoming, &existing.config);
            if let Some(message) = validate_config(existing.kind, &merged) {
                return render_validation_error(Some(&start), &message);
            }
            Some(merged)
        }
        None => None,
    };
    let update = OrgSsoConnectionUpdate {
        name: body.name,
        enabled: body.enabled,
        config: merged_config,
        default_role,
        auto_provision: body.auto_provision,
    };
    match store.update_org_sso_connection(id, update).await {
        Ok(Some(connection)) => render_success(
            Some(&start),
            StatusCode::OK,
            json!({ "connection": connection_json(&connection) }),
        ),
        Ok(None) => render_not_found(Some(&start)),
        Err(error) => render_store_error(Some(&start), error),
    }
}

pub(crate) async fn sso_connections_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((permalink, id)): Path<(String, Id)>,
) -> ApiResponse {
    let store = match sso_store(&state, &start) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let org = match require_org(&state, &start, &permalink).await {
        Ok(org) => org,
        Err(response) => return *response,
    };
    if let Err(response) = require_connection(&store, &start, org.id, id).await {
        return *response;
    }
    match store.delete_org_sso_connection(id).await {
        Ok(_) => render_deleted(Some(&start)),
        Err(error) => render_store_error(Some(&start), error),
    }
}
