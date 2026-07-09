//! Admin API v2 resource handlers: domains, credentials, routes, webhooks,
//! suppressions (server-scoped), plus users and IP pools (global). Ports of
//! the corresponding controllers in `app/controllers/admin_api/`.

use crate::app::{
    find_server, paginate, render_deleted, render_not_found, render_parameter_missing,
    render_store_error, render_success, render_validation_error, ApiResponse, ApiState,
    PaginationParams, RequestStart,
};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use camelmailer_core::{
    Credential, CredentialType, Domain, IpAddress, IpPool, NewCredential, NewIpAddress, NewRoute,
    NewSuppression, NewUser, NewWebhook, Route, RouteMode, Server, StoreError, Suppression, User,
    Webhook,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Resolve the org/server path segments or produce the 404/500 response.
async fn require_server(
    state: &ApiState,
    start: &RequestStart,
    org_permalink: &str,
    server_permalink: &str,
) -> Result<Server, ApiResponse> {
    match find_server(state, org_permalink, server_permalink).await {
        Ok(Some(server)) => Ok(server),
        Ok(None) => Err(render_not_found(Some(start))),
        Err(error) => Err(render_store_error(Some(start), error)),
    }
}

fn ok(start: &RequestStart, data: Value) -> ApiResponse {
    render_success(Some(start), StatusCode::OK, data)
}

fn created(start: &RequestStart, data: Value) -> ApiResponse {
    render_success(Some(start), StatusCode::CREATED, data)
}

fn from_result<T>(
    start: &RequestStart,
    result: Result<T, StoreError>,
    render: impl FnOnce(T) -> ApiResponse,
) -> ApiResponse {
    match result {
        Ok(value) => render(value),
        Err(error) => render_store_error(Some(start), error),
    }
}

// ----------------------------------------------------------------- domains

fn domain_json(domain: &Domain) -> Value {
    json!({
        "id": domain.id,
        "uuid": domain.uuid,
        "name": domain.name,
        "verified": domain.verified,
    })
}

pub(crate) async fn domains_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    from_result(&start, state.store.list_domains(server.id).await, |domains| {
        let result = paginate(&domains, &params);
        ok(
            &start,
            json!({
                "domains": result.items.iter().map(domain_json).collect::<Vec<_>>(),
                "pagination": result.pagination,
            }),
        )
    })
}

#[derive(Deserialize)]
pub(crate) struct CreateDomain {
    name: Option<String>,
}

pub(crate) async fn domains_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Json(body): Json<CreateDomain>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: name",
        );
    };
    from_result(
        &start,
        state.store.create_server_domain(server.id, &name).await,
        |domain| created(&start, json!({ "domain": domain_json(&domain) })),
    )
}

async fn require_domain(
    state: &ApiState,
    start: &RequestStart,
    org: &str,
    server: &str,
    name: &str,
) -> Result<Domain, ApiResponse> {
    let server = require_server(state, start, org, server).await?;
    match state.store.domain_by_name(server.id, name).await {
        Ok(Some(domain)) => Ok(domain),
        Ok(None) => Err(render_not_found(Some(start))),
        Err(error) => Err(render_store_error(Some(start), error)),
    }
}

pub(crate) async fn domains_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, name)): Path<(String, String, String)>,
) -> ApiResponse {
    match require_domain(&state, &start, &org, &server, &name).await {
        Ok(domain) => ok(&start, json!({ "domain": domain_json(&domain) })),
        Err(response) => response,
    }
}

pub(crate) async fn domains_verify(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, name)): Path<(String, String, String)>,
) -> ApiResponse {
    match require_domain(&state, &start, &org, &server, &name).await {
        Ok(mut domain) => {
            if let Err(error) = state.store.set_domain_verified(domain.id, true).await {
                return render_store_error(Some(&start), error);
            }
            domain.verified = true;
            ok(&start, json!({ "domain": domain_json(&domain) }))
        }
        Err(response) => response,
    }
}

pub(crate) async fn domains_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, name)): Path<(String, String, String)>,
) -> ApiResponse {
    match require_domain(&state, &start, &org, &server, &name).await {
        Ok(domain) => from_result(&start, state.store.delete_domain(domain.id).await, |_| {
            render_deleted(Some(&start))
        }),
        Err(response) => response,
    }
}

// ------------------------------------------------------------- credentials

fn credential_json(credential: &Credential) -> Value {
    json!({
        "id": credential.id,
        "uuid": credential.uuid,
        "type": match credential.credential_type {
            CredentialType::Smtp => "SMTP",
            CredentialType::Api => "API",
            CredentialType::SmtpIp => "SMTP-IP",
        },
        "name": credential.name,
        "key": credential.key,
        "hold": credential.hold,
    })
}

pub(crate) async fn credentials_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    from_result(
        &start,
        state.store.list_credentials(server.id).await,
        |credentials| {
            let result = paginate(&credentials, &params);
            ok(
                &start,
                json!({
                    "credentials": result.items.iter().map(credential_json).collect::<Vec<_>>(),
                    "pagination": result.pagination,
                }),
            )
        },
    )
}

#[derive(Deserialize)]
pub(crate) struct CreateCredential {
    #[serde(rename = "type")]
    credential_type: Option<String>,
    name: Option<String>,
    key: Option<String>,
}

pub(crate) async fn credentials_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Json(body): Json<CreateCredential>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: name",
        );
    };
    let credential_type = match body.credential_type.as_deref() {
        None | Some("SMTP") => CredentialType::Smtp,
        Some("API") => CredentialType::Api,
        Some("SMTP-IP") => CredentialType::SmtpIp,
        Some(other) => {
            return render_validation_error(
                Some(&start),
                &format!("Type {other:?} is not a valid credential type"),
            )
        }
    };
    if credential_type == CredentialType::SmtpIp && body.key.is_none() {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: key (CIDR for SMTP-IP credentials)",
        );
    }
    from_result(
        &start,
        state
            .store
            .create_credential_record(NewCredential {
                server_id: server.id,
                credential_type,
                name,
                key: body.key,
            })
            .await,
        |credential| created(&start, json!({ "credential": credential_json(&credential) })),
    )
}

async fn require_credential(
    state: &ApiState,
    start: &RequestStart,
    org: &str,
    server: &str,
    id: u64,
) -> Result<Credential, ApiResponse> {
    let server = require_server(state, start, org, server).await?;
    match state.store.credential_by_id(server.id, id).await {
        Ok(Some(credential)) => Ok(credential),
        Ok(None) => Err(render_not_found(Some(start))),
        Err(error) => Err(render_store_error(Some(start), error)),
    }
}

pub(crate) async fn credentials_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    match require_credential(&state, &start, &org, &server, id).await {
        Ok(credential) => ok(&start, json!({ "credential": credential_json(&credential) })),
        Err(response) => response,
    }
}

#[derive(Deserialize)]
pub(crate) struct UpdateCredential {
    name: Option<String>,
    hold: Option<bool>,
}

pub(crate) async fn credentials_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
    Json(body): Json<UpdateCredential>,
) -> ApiResponse {
    match require_credential(&state, &start, &org, &server, id).await {
        Ok(mut credential) => {
            if let Some(name) = body.name {
                credential.name = name;
            }
            if let Some(hold) = body.hold {
                credential.hold = hold;
            }
            from_result(
                &start,
                state.store.update_credential(credential).await,
                |credential| ok(&start, json!({ "credential": credential_json(&credential) })),
            )
        }
        Err(response) => response,
    }
}

pub(crate) async fn credentials_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    match require_credential(&state, &start, &org, &server, id).await {
        Ok(credential) => from_result(
            &start,
            state.store.delete_credential(credential.id).await,
            |_| render_deleted(Some(&start)),
        ),
        Err(response) => response,
    }
}

// ------------------------------------------------------------------ routes

fn route_json(route: &Route) -> Value {
    json!({
        "id": route.id,
        "uuid": route.uuid,
        "name": route.name,
        "token": route.token,
        "domain_id": route.domain_id,
        "mode": match route.mode {
            RouteMode::Endpoint => "Endpoint",
            RouteMode::Accept => "Accept",
            RouteMode::Hold => "Hold",
            RouteMode::Bounce => "Bounce",
            RouteMode::Reject => "Reject",
        },
    })
}

fn parse_route_mode(mode: Option<&str>) -> Result<RouteMode, String> {
    match mode {
        None | Some("Endpoint") => Ok(RouteMode::Endpoint),
        Some("Accept") => Ok(RouteMode::Accept),
        Some("Hold") => Ok(RouteMode::Hold),
        Some("Bounce") => Ok(RouteMode::Bounce),
        Some("Reject") => Ok(RouteMode::Reject),
        Some(other) => Err(format!("Mode {other:?} is not a valid route mode")),
    }
}

pub(crate) async fn routes_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    from_result(&start, state.store.list_routes(server.id).await, |routes| {
        let result = paginate(&routes, &params);
        ok(
            &start,
            json!({
                "routes": result.items.iter().map(route_json).collect::<Vec<_>>(),
                "pagination": result.pagination,
            }),
        )
    })
}

#[derive(Deserialize)]
pub(crate) struct CreateRoute {
    name: Option<String>,
    domain: Option<String>,
    mode: Option<String>,
}

pub(crate) async fn routes_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Json(body): Json<CreateRoute>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: name",
        );
    };
    let mode = match parse_route_mode(body.mode.as_deref()) {
        Ok(mode) => mode,
        Err(message) => return render_validation_error(Some(&start), &message),
    };
    let domain_id = match body.domain.filter(|d| !d.is_empty()) {
        Some(domain_name) => match state.store.domain_by_name(server.id, &domain_name).await {
            Ok(Some(domain)) => Some(domain.id),
            Ok(None) => {
                return render_validation_error(Some(&start), "Domain not found on this server")
            }
            Err(error) => return render_store_error(Some(&start), error),
        },
        None => None,
    };
    from_result(
        &start,
        state
            .store
            .create_route_record(NewRoute {
                server_id: server.id,
                domain_id,
                name,
                mode,
            })
            .await,
        |route| created(&start, json!({ "route": route_json(&route) })),
    )
}

async fn require_route(
    state: &ApiState,
    start: &RequestStart,
    org: &str,
    server: &str,
    id: u64,
) -> Result<Route, ApiResponse> {
    let server = require_server(state, start, org, server).await?;
    match state.store.route_by_id(server.id, id).await {
        Ok(Some(route)) => Ok(route),
        Ok(None) => Err(render_not_found(Some(start))),
        Err(error) => Err(render_store_error(Some(start), error)),
    }
}

pub(crate) async fn routes_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    match require_route(&state, &start, &org, &server, id).await {
        Ok(route) => ok(&start, json!({ "route": route_json(&route) })),
        Err(response) => response,
    }
}

#[derive(Deserialize)]
pub(crate) struct UpdateRoute {
    name: Option<String>,
    mode: Option<String>,
}

pub(crate) async fn routes_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
    Json(body): Json<UpdateRoute>,
) -> ApiResponse {
    match require_route(&state, &start, &org, &server, id).await {
        Ok(mut route) => {
            if let Some(name) = body.name {
                route.name = name;
            }
            if let Some(mode) = body.mode.as_deref() {
                match parse_route_mode(Some(mode)) {
                    Ok(mode) => route.mode = mode,
                    Err(message) => return render_validation_error(Some(&start), &message),
                }
            }
            from_result(&start, state.store.update_route(route).await, |route| {
                ok(&start, json!({ "route": route_json(&route) }))
            })
        }
        Err(response) => response,
    }
}

pub(crate) async fn routes_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    match require_route(&state, &start, &org, &server, id).await {
        Ok(route) => from_result(&start, state.store.delete_route(route.id).await, |_| {
            render_deleted(Some(&start))
        }),
        Err(response) => response,
    }
}

// ---------------------------------------------------------------- webhooks

fn webhook_json(webhook: &Webhook) -> Value {
    json!({
        "id": webhook.id,
        "uuid": webhook.uuid,
        "name": webhook.name,
        "url": webhook.url,
        "all_events": webhook.all_events,
        "enabled": webhook.enabled,
        "sign": webhook.sign,
    })
}

pub(crate) async fn webhooks_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    from_result(
        &start,
        state.store.list_webhooks(server.id).await,
        |webhooks| {
            let result = paginate(&webhooks, &params);
            ok(
                &start,
                json!({
                    "webhooks": result.items.iter().map(webhook_json).collect::<Vec<_>>(),
                    "pagination": result.pagination,
                }),
            )
        },
    )
}

#[derive(Deserialize)]
pub(crate) struct CreateWebhook {
    name: Option<String>,
    url: Option<String>,
    all_events: Option<bool>,
    sign: Option<bool>,
}

pub(crate) async fn webhooks_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Json(body): Json<CreateWebhook>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: name",
        );
    };
    let Some(url) = body.url.filter(|u| !u.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: url",
        );
    };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return render_validation_error(Some(&start), "Url must be a valid HTTP(S) URL");
    }
    from_result(
        &start,
        state
            .store
            .create_webhook(NewWebhook {
                server_id: server.id,
                name,
                url,
                all_events: body.all_events.unwrap_or(true),
                sign: body.sign.unwrap_or(true),
            })
            .await,
        |webhook| created(&start, json!({ "webhook": webhook_json(&webhook) })),
    )
}

async fn require_webhook(
    state: &ApiState,
    start: &RequestStart,
    org: &str,
    server: &str,
    id: u64,
) -> Result<Webhook, ApiResponse> {
    let server = require_server(state, start, org, server).await?;
    match state.store.webhook_by_id(server.id, id).await {
        Ok(Some(webhook)) => Ok(webhook),
        Ok(None) => Err(render_not_found(Some(start))),
        Err(error) => Err(render_store_error(Some(start), error)),
    }
}

pub(crate) async fn webhooks_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    match require_webhook(&state, &start, &org, &server, id).await {
        Ok(webhook) => ok(&start, json!({ "webhook": webhook_json(&webhook) })),
        Err(response) => response,
    }
}

pub(crate) async fn webhooks_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    match require_webhook(&state, &start, &org, &server, id).await {
        Ok(webhook) => from_result(&start, state.store.delete_webhook(webhook.id).await, |_| {
            render_deleted(Some(&start))
        }),
        Err(response) => response,
    }
}

async fn set_webhook_enabled(
    state: Arc<ApiState>,
    start: RequestStart,
    org: String,
    server: String,
    id: u64,
    enabled: bool,
) -> ApiResponse {
    match require_webhook(&state, &start, &org, &server, id).await {
        Ok(mut webhook) => {
            webhook.enabled = enabled;
            from_result(&start, state.store.update_webhook(webhook).await, |webhook| {
                ok(&start, json!({ "webhook": webhook_json(&webhook) }))
            })
        }
        Err(response) => response,
    }
}

pub(crate) async fn webhooks_enable(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    set_webhook_enabled(state, start.0, org, server, id, true).await
}

pub(crate) async fn webhooks_disable(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    set_webhook_enabled(state, start.0, org, server, id, false).await
}

// ------------------------------------------------------------ suppressions

fn suppression_json(suppression: &Suppression) -> Value {
    json!({
        "id": suppression.id,
        "type": suppression.suppression_type,
        "address": suppression.address,
        "reason": suppression.reason,
    })
}

pub(crate) async fn suppressions_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    from_result(
        &start,
        state.store.list_suppressions(server.id).await,
        |suppressions| {
            let result = paginate(&suppressions, &params);
            ok(
                &start,
                json!({
                    "suppressions": result.items.iter().map(suppression_json).collect::<Vec<_>>(),
                    "pagination": result.pagination,
                }),
            )
        },
    )
}

#[derive(Deserialize)]
pub(crate) struct CreateSuppression {
    address: Option<String>,
    #[serde(rename = "type")]
    suppression_type: Option<String>,
    reason: Option<String>,
}

pub(crate) async fn suppressions_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Json(body): Json<CreateSuppression>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let Some(address) = body.address.filter(|a| !a.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: address",
        );
    };
    from_result(
        &start,
        state
            .store
            .create_suppression(NewSuppression {
                server_id: server.id,
                suppression_type: body.suppression_type.unwrap_or_else(|| "recipient".into()),
                address,
                reason: body.reason,
            })
            .await,
        |suppression| created(&start, json!({ "suppression": suppression_json(&suppression) })),
    )
}

pub(crate) async fn suppressions_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, address)): Path<(String, String, String)>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    match state.store.delete_suppression(server.id, &address).await {
        Ok(true) => render_deleted(Some(&start)),
        Ok(false) => render_not_found(Some(&start)),
        Err(error) => render_store_error(Some(&start), error),
    }
}

// ------------------------------------------------------------------- users

fn user_json(user: &User) -> Value {
    json!({
        "id": user.id,
        "uuid": user.uuid,
        "email_address": user.email_address,
        "first_name": user.first_name,
        "last_name": user.last_name,
        "admin": user.admin,
    })
}

pub(crate) async fn users_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    from_result(&start, state.store.list_users().await, |users| {
        let result = paginate(&users, &params);
        ok(
            &start,
            json!({
                "users": result.items.iter().map(user_json).collect::<Vec<_>>(),
                "pagination": result.pagination,
            }),
        )
    })
}

#[derive(Deserialize)]
pub(crate) struct CreateUser {
    email_address: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    admin: Option<bool>,
}

pub(crate) async fn users_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Json(body): Json<CreateUser>,
) -> ApiResponse {
    let Some(email_address) = body.email_address.filter(|e| !e.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: email_address",
        );
    };
    if !email_address.contains('@') {
        return render_validation_error(Some(&start), "Email address is invalid");
    }
    from_result(
        &start,
        state
            .store
            .create_user(NewUser {
                email_address,
                first_name: body.first_name.unwrap_or_default(),
                last_name: body.last_name.unwrap_or_default(),
                admin: body.admin.unwrap_or(false),
            })
            .await,
        |user| created(&start, json!({ "user": user_json(&user) })),
    )
}

async fn require_user(
    state: &ApiState,
    start: &RequestStart,
    id: u64,
) -> Result<User, ApiResponse> {
    match state.store.user_by_id(id).await {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(render_not_found(Some(start))),
        Err(error) => Err(render_store_error(Some(start), error)),
    }
}

pub(crate) async fn users_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(id): Path<u64>,
) -> ApiResponse {
    match require_user(&state, &start, id).await {
        Ok(user) => ok(&start, json!({ "user": user_json(&user) })),
        Err(response) => response,
    }
}

#[derive(Deserialize)]
pub(crate) struct UpdateUser {
    email_address: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    admin: Option<bool>,
}

pub(crate) async fn users_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(id): Path<u64>,
    Json(body): Json<UpdateUser>,
) -> ApiResponse {
    match require_user(&state, &start, id).await {
        Ok(mut user) => {
            if let Some(email_address) = body.email_address {
                user.email_address = email_address;
            }
            if let Some(first_name) = body.first_name {
                user.first_name = first_name;
            }
            if let Some(last_name) = body.last_name {
                user.last_name = last_name;
            }
            if let Some(admin) = body.admin {
                user.admin = admin;
            }
            from_result(&start, state.store.update_user(user).await, |user| {
                ok(&start, json!({ "user": user_json(&user) }))
            })
        }
        Err(response) => response,
    }
}

pub(crate) async fn users_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(id): Path<u64>,
) -> ApiResponse {
    match require_user(&state, &start, id).await {
        Ok(user) => from_result(&start, state.store.delete_user(user.id).await, |_| {
            render_deleted(Some(&start))
        }),
        Err(response) => response,
    }
}

// ---------------------------------------------------------------- IP pools

fn ip_pool_json(pool: &IpPool) -> Value {
    json!({
        "id": pool.id,
        "uuid": pool.uuid,
        "name": pool.name,
        "default": pool.default,
    })
}

fn ip_address_json(address: &IpAddress) -> Value {
    json!({
        "id": address.id,
        "uuid": address.uuid,
        "ipv4": address.ipv4,
        "ipv6": address.ipv6,
        "hostname": address.hostname,
        "priority": address.priority,
    })
}

pub(crate) async fn ip_pools_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    from_result(&start, state.store.list_ip_pools().await, |pools| {
        let result = paginate(&pools, &params);
        ok(
            &start,
            json!({
                "ip_pools": result.items.iter().map(ip_pool_json).collect::<Vec<_>>(),
                "pagination": result.pagination,
            }),
        )
    })
}

#[derive(Deserialize)]
pub(crate) struct CreateIpPool {
    name: Option<String>,
    default: Option<bool>,
}

pub(crate) async fn ip_pools_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Json(body): Json<CreateIpPool>,
) -> ApiResponse {
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: name",
        );
    };
    from_result(
        &start,
        state
            .store
            .create_ip_pool(&name, body.default.unwrap_or(false))
            .await,
        |pool| created(&start, json!({ "ip_pool": ip_pool_json(&pool) })),
    )
}

async fn require_ip_pool(
    state: &ApiState,
    start: &RequestStart,
    id: u64,
) -> Result<IpPool, ApiResponse> {
    match state.store.ip_pool_by_id(id).await {
        Ok(Some(pool)) => Ok(pool),
        Ok(None) => Err(render_not_found(Some(start))),
        Err(error) => Err(render_store_error(Some(start), error)),
    }
}

pub(crate) async fn ip_pools_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(id): Path<u64>,
) -> ApiResponse {
    match require_ip_pool(&state, &start, id).await {
        Ok(pool) => ok(&start, json!({ "ip_pool": ip_pool_json(&pool) })),
        Err(response) => response,
    }
}

pub(crate) async fn ip_pools_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(id): Path<u64>,
) -> ApiResponse {
    match require_ip_pool(&state, &start, id).await {
        Ok(pool) => from_result(&start, state.store.delete_ip_pool(pool.id).await, |_| {
            render_deleted(Some(&start))
        }),
        Err(response) => response,
    }
}

pub(crate) async fn ip_addresses_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(pool_id): Path<u64>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    if let Err(response) = require_ip_pool(&state, &start, pool_id).await {
        return response;
    }
    from_result(
        &start,
        state.store.list_ip_addresses(pool_id).await,
        |addresses| {
            let result = paginate(&addresses, &params);
            ok(
                &start,
                json!({
                    "ip_addresses": result.items.iter().map(ip_address_json).collect::<Vec<_>>(),
                    "pagination": result.pagination,
                }),
            )
        },
    )
}

#[derive(Deserialize)]
pub(crate) struct CreateIpAddress {
    ipv4: Option<String>,
    ipv6: Option<String>,
    hostname: Option<String>,
    priority: Option<i32>,
}

pub(crate) async fn ip_addresses_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(pool_id): Path<u64>,
    Json(body): Json<CreateIpAddress>,
) -> ApiResponse {
    if let Err(response) = require_ip_pool(&state, &start, pool_id).await {
        return response;
    }
    let Some(ipv4) = body.ipv4.filter(|i| !i.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: ipv4",
        );
    };
    if ipv4.parse::<std::net::Ipv4Addr>().is_err() {
        return render_validation_error(Some(&start), "Ipv4 is not a valid IPv4 address");
    }
    let Some(hostname) = body.hostname.filter(|h| !h.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: hostname",
        );
    };
    from_result(
        &start,
        state
            .store
            .create_ip_address(NewIpAddress {
                ip_pool_id: pool_id,
                ipv4,
                ipv6: body.ipv6,
                hostname,
                priority: body.priority.unwrap_or(100),
            })
            .await,
        |address| created(&start, json!({ "ip_address": ip_address_json(&address) })),
    )
}

pub(crate) async fn ip_addresses_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((pool_id, id)): Path<(u64, u64)>,
) -> ApiResponse {
    match state.store.ip_address_by_id(pool_id, id).await {
        Ok(Some(address)) => ok(&start, json!({ "ip_address": ip_address_json(&address) })),
        Ok(None) => render_not_found(Some(&start)),
        Err(error) => render_store_error(Some(&start), error),
    }
}

pub(crate) async fn ip_addresses_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((pool_id, id)): Path<(u64, u64)>,
) -> ApiResponse {
    match state.store.ip_address_by_id(pool_id, id).await {
        Ok(Some(address)) => from_result(
            &start,
            state.store.delete_ip_address(address.id).await,
            |_| render_deleted(Some(&start)),
        ),
        Ok(None) => render_not_found(Some(&start)),
        Err(error) => render_store_error(Some(&start), error),
    }
}
