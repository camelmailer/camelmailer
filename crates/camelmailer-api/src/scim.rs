//! SCIM 2.0 provisioning (RFC 7643/7644), Users core, under `/scim/v2`.
//!
//! A separate surface with its own conventions: authentication is a
//! static bearer token (`scim.bearer_token`, compared in constant
//! time), bodies and responses use `application/scim+json`, and errors
//! use the SCIM error schema — not the `{ status, time, … }` envelope
//! of the first-party APIs.
//!
//! `userName` is the account's email address. `active: false`
//! deactivates the account (it can no longer log in — password, OIDC or
//! SAML — or complete a password reset) and revokes its sessions;
//! `DELETE` deactivates rather than destroys, so audit history and
//! memberships survive an IdP-side offboarding.

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use camelmailer_core::{AuthStore, Id, StoreError, User};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use subtle::ConstantTimeEq;

use crate::app::ApiState;

const CONTENT_TYPE: &str = "application/scim+json";
const SCHEMA_USER: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
const SCHEMA_LIST: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
const SCHEMA_ERROR: &str = "urn:ietf:params:scim:api:messages:2.0:Error";
const SCHEMA_PATCH: &str = "urn:ietf:params:scim:api:messages:2.0:PatchOp";

fn scim_json(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        body.to_string(),
    )
        .into_response()
}

/// The SCIM error format (`urn:ietf:params:scim:api:messages:2.0:Error`).
fn scim_error(status: StatusCode, detail: &str, scim_type: Option<&str>) -> Response {
    let mut body = json!({
        "schemas": [SCHEMA_ERROR],
        "status": status.as_u16().to_string(),
        "detail": detail,
    });
    if let Some(scim_type) = scim_type {
        body["scimType"] = json!(scim_type);
    }
    scim_json(status, body)
}

fn store_error(error: StoreError) -> Response {
    match error {
        StoreError::Conflict(message) => {
            scim_error(StatusCode::CONFLICT, &message, Some("uniqueness"))
        }
        StoreError::Other(message) => {
            tracing::error!(%message, "storage error in the SCIM API");
            scim_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "An internal error occurred",
                None,
            )
        }
    }
}

/// Bearer-token authentication for the whole `/scim/v2` surface.
async fn scim_auth_middleware(
    State(state): State<Arc<ApiState>>,
    request: Request,
    next: Next,
) -> Response {
    if !state.config.scim.enabled {
        return scim_error(
            StatusCode::NOT_FOUND,
            "SCIM provisioning is not enabled on this instance",
            None,
        );
    }
    let presented = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or("");
    let configured = state.config.scim.bearer_token.as_deref().unwrap_or("");
    let valid = !presented.is_empty()
        && !configured.is_empty()
        && bool::from(configured.as_bytes().ct_eq(presented.as_bytes()));
    if !valid {
        return scim_error(
            StatusCode::UNAUTHORIZED,
            "Invalid or missing bearer token",
            None,
        );
    }
    next.run(request).await
}

fn auth_store(state: &ApiState) -> Result<&Arc<dyn AuthStore>, Box<Response>> {
    state.auth_store.as_ref().ok_or_else(|| {
        Box::new(scim_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "User accounts require persistent storage and are not enabled on this instance",
            None,
        ))
    })
}

async fn is_active(state: &ApiState, user_id: Id) -> Result<bool, Response> {
    let store = auth_store(state).map_err(|response| *response)?;
    let auth = store.user_auth(user_id).await.map_err(store_error)?;
    Ok(!auth.map(|auth| auth.disabled).unwrap_or(false))
}

fn user_resource(user: &User, active: bool) -> Value {
    json!({
        "schemas": [SCHEMA_USER],
        "id": user.id.to_string(),
        "externalId": user.uuid,
        "userName": user.email_address,
        "name": {
            "givenName": user.first_name,
            "familyName": user.last_name,
        },
        "emails": [{ "value": user.email_address, "primary": true }],
        "active": active,
        "meta": {
            "resourceType": "User",
            "location": format!("/scim/v2/Users/{}", user.id),
        },
    })
}

/// Accept SCIM booleans in the shapes IdPs actually send (`true`,
/// `"True"`, `"false"` …).
fn scim_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) => match text.to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn parse_body(body: &Bytes) -> Result<Value, Box<Response>> {
    serde_json::from_slice(body).map_err(|error| {
        Box::new(scim_error(
            StatusCode::BAD_REQUEST,
            &format!("invalid JSON body: {error}"),
            Some("invalidSyntax"),
        ))
    })
}

async fn record_event(state: &ApiState, user: &User, event: &str) {
    if let Some(store) = state.auth_store.as_ref() {
        let _ = store
            .record_auth_event(camelmailer_core::NewAuthEvent {
                user_id: Some(user.id),
                email_address: Some(user.email_address.clone()),
                event: event.into(),
                ip_address: None,
                user_agent: None,
            })
            .await;
    }
}

/// Deactivate/reactivate + session revocation on deactivation.
async fn apply_active(state: &ApiState, user: &User, active: bool) -> Result<(), Response> {
    let store = auth_store(state).map_err(|response| *response)?;
    let was_active = is_active(state, user.id).await?;
    if was_active == active {
        return Ok(());
    }
    store
        .set_user_disabled(user.id, !active)
        .await
        .map_err(store_error)?;
    if !active {
        let _ = store.delete_sessions_for_user(user.id).await;
        record_event(state, user, "scim.deactivate").await;
    } else {
        record_event(state, user, "scim.reactivate").await;
    }
    Ok(())
}

// ---------------------------------------------------------- discovery

async fn service_provider_config() -> Response {
    scim_json(
        StatusCode::OK,
        json!({
            "schemas": ["urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig"],
            "documentationUri": "https://github.com/camelmailer/camelmailer/blob/main/docs/authentication.md",
            "patch": { "supported": true },
            "bulk": { "supported": false, "maxOperations": 0, "maxPayloadSize": 0 },
            "filter": { "supported": true, "maxResults": 100 },
            "changePassword": { "supported": false },
            "sort": { "supported": false },
            "etag": { "supported": false },
            "authenticationSchemes": [{
                "type": "oauthbearertoken",
                "name": "Bearer token",
                "description": "Authorization: Bearer <scim.bearer_token>",
            }],
            "meta": { "resourceType": "ServiceProviderConfig", "location": "/scim/v2/ServiceProviderConfig" },
        }),
    )
}

async fn resource_types() -> Response {
    scim_json(
        StatusCode::OK,
        json!({
            "schemas": [SCHEMA_LIST],
            "totalResults": 1,
            "startIndex": 1,
            "itemsPerPage": 1,
            "Resources": [{
                "schemas": ["urn:ietf:params:scim:schemas:core:2.0:ResourceType"],
                "id": "User",
                "name": "User",
                "endpoint": "/Users",
                "schema": SCHEMA_USER,
                "meta": { "resourceType": "ResourceType", "location": "/scim/v2/ResourceTypes/User" },
            }],
        }),
    )
}

async fn schemas() -> Response {
    scim_json(
        StatusCode::OK,
        json!({
            "schemas": [SCHEMA_LIST],
            "totalResults": 1,
            "startIndex": 1,
            "itemsPerPage": 1,
            "Resources": [{
                "id": SCHEMA_USER,
                "name": "User",
                "description": "User account",
                "attributes": [
                    { "name": "userName", "type": "string", "multiValued": false,
                      "required": true, "caseExact": false, "mutability": "readWrite",
                      "returned": "default", "uniqueness": "server",
                      "description": "The user's email address" },
                    { "name": "name", "type": "complex", "multiValued": false,
                      "required": false, "mutability": "readWrite", "returned": "default",
                      "subAttributes": [
                          { "name": "givenName", "type": "string", "multiValued": false,
                            "required": false, "caseExact": false, "mutability": "readWrite",
                            "returned": "default", "uniqueness": "none" },
                          { "name": "familyName", "type": "string", "multiValued": false,
                            "required": false, "caseExact": false, "mutability": "readWrite",
                            "returned": "default", "uniqueness": "none" },
                      ] },
                    { "name": "active", "type": "boolean", "multiValued": false,
                      "required": false, "mutability": "readWrite", "returned": "default",
                      "description": "False deactivates the account (login blocked, sessions revoked)" },
                ],
                "meta": {
                    "resourceType": "Schema",
                    "location": format!("/scim/v2/Schemas/{SCHEMA_USER}"),
                },
            }],
        }),
    )
}

// --------------------------------------------------------------- users

#[derive(Debug, Deserialize, Default)]
struct ListParams {
    #[serde(rename = "startIndex")]
    start_index: Option<i64>,
    count: Option<i64>,
    filter: Option<String>,
}

/// The one filter SCIM IdPs use for reconciliation: `userName eq "…"`.
fn parse_username_filter(filter: &str) -> Result<String, Box<Response>> {
    let invalid = || {
        Box::new(scim_error(
            StatusCode::BAD_REQUEST,
            "only the filter `userName eq \"value\"` is supported",
            Some("invalidFilter"),
        ))
    };
    let mut parts = filter.trim().splitn(3, char::is_whitespace);
    let attribute = parts.next().unwrap_or("");
    let operator = parts.next().unwrap_or("");
    let value = parts.next().unwrap_or("").trim();
    if !attribute.eq_ignore_ascii_case("username") || !operator.eq_ignore_ascii_case("eq") {
        return Err(invalid());
    }
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_string)
        .ok_or_else(invalid)
}

async fn users_index(
    State(state): State<Arc<ApiState>>,
    Query(params): Query<ListParams>,
) -> Response {
    let mut users = match state.store.list_users().await {
        Ok(users) => users,
        Err(error) => return store_error(error),
    };
    users.sort_by_key(|user| user.id);
    if let Some(filter) = params.filter.as_deref() {
        let user_name = match parse_username_filter(filter) {
            Ok(user_name) => user_name,
            Err(response) => return *response,
        };
        users.retain(|user| user.email_address.eq_ignore_ascii_case(&user_name));
    }

    let total = users.len() as i64;
    let start_index = params.start_index.unwrap_or(1).max(1);
    let count = params.count.unwrap_or(100).clamp(0, 100);
    let page: Vec<User> = users
        .into_iter()
        .skip((start_index - 1) as usize)
        .take(count as usize)
        .collect();

    let mut resources = Vec::with_capacity(page.len());
    for user in &page {
        let active = match is_active(&state, user.id).await {
            Ok(active) => active,
            Err(response) => return response,
        };
        resources.push(user_resource(user, active));
    }
    scim_json(
        StatusCode::OK,
        json!({
            "schemas": [SCHEMA_LIST],
            "totalResults": total,
            "startIndex": start_index,
            "itemsPerPage": resources.len(),
            "Resources": resources,
        }),
    )
}

async fn find_user(state: &ApiState, id: &str) -> Result<User, Response> {
    let not_found = || scim_error(StatusCode::NOT_FOUND, "User not found", None);
    let id: Id = id.parse().map_err(|_| not_found())?;
    match state.store.user_by_id(id).await {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(not_found()),
        Err(error) => Err(store_error(error)),
    }
}

async fn users_show(State(state): State<Arc<ApiState>>, Path(id): Path<String>) -> Response {
    let user = match find_user(&state, &id).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    match is_active(&state, user.id).await {
        Ok(active) => scim_json(StatusCode::OK, user_resource(&user, active)),
        Err(response) => response,
    }
}

async fn users_create(State(state): State<Arc<ApiState>>, body: Bytes) -> Response {
    let body = match parse_body(&body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let Some(user_name) = body["userName"].as_str().filter(|value| !value.is_empty()) else {
        return scim_error(
            StatusCode::BAD_REQUEST,
            "userName is required",
            Some("invalidValue"),
        );
    };
    if !user_name.contains('@') {
        return scim_error(
            StatusCode::BAD_REQUEST,
            "userName must be an email address",
            Some("invalidValue"),
        );
    }
    // Explicit conflict check for a clean SCIM 409 (the store would also
    // refuse with a uniqueness conflict).
    let store = match auth_store(&state) {
        Ok(store) => store,
        Err(response) => return *response,
    };
    match store.user_by_email(user_name).await {
        Ok(Some(_)) => {
            return scim_error(
                StatusCode::CONFLICT,
                "A user with this userName already exists",
                Some("uniqueness"),
            )
        }
        Ok(None) => {}
        Err(error) => return store_error(error),
    }

    let active = body.get("active").and_then(scim_bool).unwrap_or(true);
    let user = match state
        .store
        .create_user(camelmailer_core::NewUser {
            email_address: user_name.to_string(),
            first_name: body["name"]["givenName"].as_str().unwrap_or("").to_string(),
            last_name: body["name"]["familyName"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            admin: false,
        })
        .await
    {
        Ok(user) => user,
        Err(error) => return store_error(error),
    };
    record_event(&state, &user, "scim.provision").await;
    if !active {
        if let Err(response) = apply_active(&state, &user, false).await {
            return response;
        }
    }
    let resource = user_resource(&user, active);
    (
        StatusCode::CREATED,
        [
            (header::CONTENT_TYPE, CONTENT_TYPE.to_string()),
            (header::LOCATION, format!("/scim/v2/Users/{}", user.id)),
        ],
        resource.to_string(),
    )
        .into_response()
}

async fn users_replace(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let body = match parse_body(&body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let mut user = match find_user(&state, &id).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Some(user_name) = body["userName"].as_str().filter(|value| !value.is_empty()) {
        if !user_name.contains('@') {
            return scim_error(
                StatusCode::BAD_REQUEST,
                "userName must be an email address",
                Some("invalidValue"),
            );
        }
        user.email_address = user_name.to_string();
    }
    // PUT is a full replace: absent name sub-attributes clear the field.
    user.first_name = body["name"]["givenName"].as_str().unwrap_or("").to_string();
    user.last_name = body["name"]["familyName"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let user = match state.store.update_user(user).await {
        Ok(user) => user,
        Err(error) => return store_error(error),
    };
    let active = body.get("active").and_then(scim_bool).unwrap_or(true);
    if let Err(response) = apply_active(&state, &user, active).await {
        return response;
    }
    scim_json(StatusCode::OK, user_resource(&user, active))
}

async fn users_patch(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let body = match parse_body(&body) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    if body["schemas"]
        .as_array()
        .map(|schemas| !schemas.iter().any(|schema| schema == SCHEMA_PATCH))
        .unwrap_or(true)
    {
        return scim_error(
            StatusCode::BAD_REQUEST,
            &format!("PATCH requires the {SCHEMA_PATCH} schema"),
            Some("invalidValue"),
        );
    }
    let mut user = match find_user(&state, &id).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let mut active: Option<bool> = None;
    let mut profile_changed = false;

    let operations = body["Operations"].as_array().cloned().unwrap_or_default();
    if operations.is_empty() {
        return scim_error(
            StatusCode::BAD_REQUEST,
            "Operations must be a non-empty array",
            Some("invalidValue"),
        );
    }
    for operation in &operations {
        let op = operation["op"].as_str().unwrap_or("").to_ascii_lowercase();
        if op != "replace" && op != "add" {
            return scim_error(
                StatusCode::BAD_REQUEST,
                &format!("unsupported PATCH op {op:?} (only add/replace)"),
                Some("invalidValue"),
            );
        }
        let value = &operation["value"];
        let invalid_value =
            |detail: &str| scim_error(StatusCode::BAD_REQUEST, detail, Some("invalidValue"));
        match operation["path"].as_str() {
            None => {
                // Whole-resource form: { "op": "replace", "value": {…} }
                if let Some(flag) = value.get("active") {
                    match scim_bool(flag) {
                        Some(flag) => active = Some(flag),
                        None => return invalid_value("active must be a boolean"),
                    }
                }
                if let Some(user_name) = value["userName"].as_str() {
                    user.email_address = user_name.to_string();
                    profile_changed = true;
                }
                if let Some(given) = value["name"]["givenName"].as_str() {
                    user.first_name = given.to_string();
                    profile_changed = true;
                }
                if let Some(family) = value["name"]["familyName"].as_str() {
                    user.last_name = family.to_string();
                    profile_changed = true;
                }
            }
            Some(path) if path.eq_ignore_ascii_case("active") => match scim_bool(value) {
                Some(flag) => active = Some(flag),
                None => return invalid_value("active must be a boolean"),
            },
            Some(path) if path.eq_ignore_ascii_case("username") => match value.as_str() {
                Some(user_name) => {
                    user.email_address = user_name.to_string();
                    profile_changed = true;
                }
                None => return invalid_value("userName must be a string"),
            },
            Some(path) if path.eq_ignore_ascii_case("name.givenname") => match value.as_str() {
                Some(given) => {
                    user.first_name = given.to_string();
                    profile_changed = true;
                }
                None => return invalid_value("name.givenName must be a string"),
            },
            Some(path) if path.eq_ignore_ascii_case("name.familyname") => match value.as_str() {
                Some(family) => {
                    user.last_name = family.to_string();
                    profile_changed = true;
                }
                None => return invalid_value("name.familyName must be a string"),
            },
            Some(path) => {
                return scim_error(
                    StatusCode::BAD_REQUEST,
                    &format!("unsupported PATCH path {path:?}"),
                    Some("invalidPath"),
                )
            }
        }
    }

    if profile_changed {
        user = match state.store.update_user(user).await {
            Ok(user) => user,
            Err(error) => return store_error(error),
        };
    }
    if let Some(active) = active {
        if let Err(response) = apply_active(&state, &user, active).await {
            return response;
        }
    }
    match is_active(&state, user.id).await {
        Ok(active) => scim_json(StatusCode::OK, user_resource(&user, active)),
        Err(response) => response,
    }
}

/// SCIM DELETE deactivates instead of destroying: audit history and
/// organization memberships survive an IdP-side offboarding.
async fn users_delete(State(state): State<Arc<ApiState>>, Path(id): Path<String>) -> Response {
    let user = match find_user(&state, &id).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = apply_active(&state, &user, false).await {
        return response;
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Build the `/scim/v2` router.
pub fn build_scim_router(state: Arc<ApiState>) -> Router {
    Router::new().nest(
        "/scim/v2",
        Router::new()
            .route("/ServiceProviderConfig", get(service_provider_config))
            .route("/ResourceTypes", get(resource_types))
            .route("/Schemas", get(schemas))
            .route("/Users", get(users_index).post(users_create))
            .route(
                "/Users/{id}",
                get(users_show)
                    .put(users_replace)
                    .patch(users_patch)
                    .delete(users_delete),
            )
            .layer(middleware::from_fn_with_state(
                state.clone(),
                scim_auth_middleware,
            ))
            .with_state(state),
    )
}
