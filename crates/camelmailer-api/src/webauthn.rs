//! WebAuthn / passkeys (`/api/v2/auth/webauthn/*`).
//!
//! Signed-in users register passkeys (`register/start` → browser
//! `navigator.credentials.create()` → `register/finish`), list them and
//! delete them; anyone signs in with one (`login/start` →
//! `navigator.credentials.get()` → `login/finish`, which issues the same
//! session as a password login). Built on the `webauthn-rs` crate; all
//! binary fields travel as unpadded base64url exactly as its JSON types
//! serialize them.
//!
//! In-flight ceremonies are held server-side, keyed by their challenge —
//! the same short-lived, single-use mechanism `oidc_states` uses for OIDC
//! logins. The finish endpoints recover the challenge from the signed
//! `clientDataJSON`, so no cookie or session is needed mid-ceremony.
//!
//! `login/start` answers with the same generic shape whether or not the
//! account exists: unknown addresses (and accounts without passkeys)
//! receive a deterministic set of fake credential ids, so the endpoint
//! cannot be used for user enumeration.
//!
//! While `auth.webauthn.enabled` is off every endpoint answers
//! `403 WebAuthnDisabled`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use camelmailer_core::{Id, NewWebAuthnCredential, WebAuthnCredential};
use chrono::{Duration, Utc};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::OnceLock;
use webauthn_rs::fake::{FakePasskeyDistribution, WebauthnFakeCredentialGenerator};
use webauthn_rs::prelude::{
    Passkey, PasskeyAuthentication, PasskeyRegistration, Url, Uuid, Webauthn, WebauthnBuilder,
};
use webauthn_rs_proto::{
    AllowCredentials, PublicKeyCredential, PublicKeyCredentialRequestOptions,
    RegisterPublicKeyCredential, RequestChallengeResponse, UserVerificationPolicy,
};

use crate::app::{
    render_deleted, render_error, render_not_found, render_parameter_missing, render_store_error,
    render_success, ApiResponse, ApiState, RequestStart,
};
use crate::auth_api::{audit, auth_store_or_unavailable, issue_session, user_json, CurrentUser};

/// How long a started ceremony may take before the finish call is refused.
const STATE_TTL_MINUTES: i64 = 5;

/// The authenticator interaction timeout advertised in fake login options —
/// must match `webauthn_rs::DEFAULT_AUTHENTICATOR_TIMEOUT` so real and fake
/// responses are indistinguishable.
const FAKE_TIMEOUT_MS: u32 = 300_000;

fn disabled(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::FORBIDDEN,
        "WebAuthnDisabled",
        "WebAuthn / passkeys are not enabled on this instance",
    )
}

fn webauthn_error(start: Option<&RequestStart>, message: &str) -> ApiResponse {
    render_error(
        start,
        StatusCode::UNPROCESSABLE_ENTITY,
        "WebAuthnError",
        message,
    )
}

fn misconfigured(start: Option<&RequestStart>, message: &str) -> ApiResponse {
    tracing::error!(%message, "webauthn is misconfigured");
    render_error(
        start,
        StatusCode::INTERNAL_SERVER_ERROR,
        "InternalServerError",
        "WebAuthn is misconfigured on this instance",
    )
}

/// The relying party, built from `auth.webauthn` (validated at startup:
/// `enabled` requires `rp_id` + `rp_origin`).
fn build_webauthn(state: &ApiState) -> Result<Webauthn, String> {
    let settings = &state.config.auth.webauthn;
    let origin = Url::parse(&settings.rp_origin)
        .map_err(|error| format!("auth.webauthn.rp_origin is not a valid URL: {error}"))?;
    WebauthnBuilder::new(&settings.rp_id, &origin)
        .and_then(|builder| builder.rp_name(&settings.rp_name).build())
        .map_err(|error| format!("invalid auth.webauthn configuration: {error}"))
}

fn credential_view(credential: &WebAuthnCredential) -> Value {
    json!({
        "id": credential.id,
        "name": credential.name,
        "created_at": credential.created_at,
        "last_used_at": credential.last_used_at,
    })
}

/// The base64url challenge inside a serialized options document
/// (`publicKey.challenge`) — the key the ceremony state is stored under.
fn challenge_of(options: &Value) -> Option<String> {
    options["publicKey"]["challenge"]
        .as_str()
        .map(str::to_string)
}

/// The challenge the client actually signed, straight out of the
/// (base64url-decoded) `clientDataJSON` — matches [`challenge_of`] for a
/// well-behaved client, letting finish find the state without a cookie.
fn client_challenge(client_data_json: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct ClientData {
        challenge: String,
    }
    serde_json::from_slice::<ClientData>(client_data_json)
        .ok()
        .map(|data| data.challenge)
}

// ------------------------------------------------------- registration

/// `POST /api/v2/auth/webauthn/register/start` (Bearer) — creation
/// options for `navigator.credentials.create()`. Existing passkeys are
/// excluded so the same authenticator cannot be registered twice.
pub(crate) async fn register_start(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
) -> ApiResponse {
    if !state.config.auth.webauthn.enabled {
        return disabled(Some(&start.0));
    }
    let store = match auth_store_or_unavailable(&state, Some(&start.0)) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let webauthn = match build_webauthn(&state) {
        Ok(webauthn) => webauthn,
        Err(message) => return misconfigured(Some(&start.0), &message),
    };
    let existing = match store.list_webauthn_credentials(current.user.id).await {
        Ok(existing) => existing,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let exclude: Vec<Vec<u8>> = existing
        .iter()
        .filter_map(|credential| URL_SAFE_NO_PAD.decode(&credential.credential_id).ok())
        .collect();
    let Ok(user_uuid) = Uuid::parse_str(&current.user.uuid) else {
        return misconfigured(Some(&start.0), "user uuid is not a valid UUID");
    };
    let display_name = format!("{} {}", current.user.first_name, current.user.last_name);
    let (options, registration) = match webauthn.start_passkey_registration(
        user_uuid,
        &current.user.email_address,
        display_name.trim(),
        Some(exclude),
    ) {
        Ok(pair) => pair,
        Err(error) => {
            return webauthn_error(
                Some(&start.0),
                &format!("could not start the registration: {error}"),
            )
        }
    };
    let options = match serde_json::to_value(&options) {
        Ok(options) => options,
        Err(error) => return misconfigured(Some(&start.0), &error.to_string()),
    };
    let Some(challenge) = challenge_of(&options) else {
        return misconfigured(Some(&start.0), "creation options carry no challenge");
    };
    let registration_json = match serde_json::to_string(&registration) {
        Ok(json) => json,
        Err(error) => return misconfigured(Some(&start.0), &error.to_string()),
    };
    if let Err(error) = store
        .create_webauthn_state(
            &format!("reg:{challenge}"),
            Some(current.user.id),
            &registration_json,
            Utc::now() + Duration::minutes(STATE_TTL_MINUTES),
        )
        .await
    {
        return render_store_error(Some(&start.0), error);
    }
    render_success(Some(&start.0), StatusCode::OK, options)
}

#[derive(Debug, Deserialize)]
pub(crate) struct RegisterFinishBody {
    name: Option<String>,
    credential: Option<Value>,
}

/// `POST /api/v2/auth/webauthn/register/finish` (Bearer) —
/// `{ name, credential }` where `credential` is the JSON-serialized
/// result of `navigator.credentials.create()`. Verifies the attestation
/// against the pending challenge and stores the passkey.
pub(crate) async fn register_finish(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RegisterFinishBody>,
) -> ApiResponse {
    if !state.config.auth.webauthn.enabled {
        return disabled(Some(&start.0));
    }
    let store = match auth_store_or_unavailable(&state, Some(&start.0)) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let Some(name) = body.name.filter(|name| !name.trim().is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: name",
        );
    };
    let name: String = name.trim().chars().take(100).collect();
    let Some(credential) = body.credential else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: credential",
        );
    };
    let credential: RegisterPublicKeyCredential = match serde_json::from_value(credential) {
        Ok(credential) => credential,
        Err(error) => {
            return webauthn_error(
                Some(&start.0),
                &format!("the credential could not be parsed: {error}"),
            )
        }
    };
    let invalid_challenge = |start: &RequestStart| {
        webauthn_error(
            Some(start),
            "the registration challenge is invalid or has expired",
        )
    };
    let Some(challenge) = client_challenge(&credential.response.client_data_json) else {
        return invalid_challenge(&start.0);
    };
    let state_row = match store
        .consume_webauthn_state(&format!("reg:{challenge}"), Utc::now())
        .await
    {
        Ok(state_row) => state_row,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    // The pending registration must belong to the caller's own session.
    let Some((Some(state_user_id), registration_json)) = state_row else {
        return invalid_challenge(&start.0);
    };
    if state_user_id != current.user.id {
        return invalid_challenge(&start.0);
    }
    let Ok(registration) = serde_json::from_str::<PasskeyRegistration>(&registration_json) else {
        return misconfigured(Some(&start.0), "stored registration state is corrupt");
    };
    let webauthn = match build_webauthn(&state) {
        Ok(webauthn) => webauthn,
        Err(message) => return misconfigured(Some(&start.0), &message),
    };
    let passkey = match webauthn.finish_passkey_registration(&credential, &registration) {
        Ok(passkey) => passkey,
        Err(error) => {
            return webauthn_error(
                Some(&start.0),
                &format!("the passkey could not be verified: {error}"),
            )
        }
    };
    let credential_id = URL_SAFE_NO_PAD.encode(passkey.cred_id());
    let credential_json = match serde_json::to_string(&passkey) {
        Ok(json) => json,
        Err(error) => return misconfigured(Some(&start.0), &error.to_string()),
    };
    let stored = match store
        .add_webauthn_credential(NewWebAuthnCredential {
            user_id: current.user.id,
            name,
            credential_id,
            credential_json,
        })
        .await
    {
        Ok(stored) => stored,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    audit(
        store,
        Some(current.user.id),
        Some(&current.user.email_address),
        "webauthn.register",
        &headers,
    )
    .await;
    render_success(
        Some(&start.0),
        StatusCode::CREATED,
        json!({ "credential": credential_view(&stored) }),
    )
}

// -------------------------------------------------- credential listing

/// `GET /api/v2/auth/webauthn/credentials` (Bearer) — the caller's
/// passkeys (name, created_at, last_used_at — never key material).
pub(crate) async fn credentials_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
) -> ApiResponse {
    if !state.config.auth.webauthn.enabled {
        return disabled(Some(&start.0));
    }
    let store = match auth_store_or_unavailable(&state, Some(&start.0)) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store.list_webauthn_credentials(current.user.id).await {
        Ok(credentials) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "credentials": credentials.iter().map(credential_view).collect::<Vec<_>>(),
            }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

/// `DELETE /api/v2/auth/webauthn/credentials/{id}` (Bearer) — remove one
/// of the caller's passkeys. The password always remains as a login
/// factor, so even the last passkey may be deleted.
pub(crate) async fn credentials_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
    headers: axum::http::HeaderMap,
    Path(credential_id): Path<Id>,
) -> ApiResponse {
    if !state.config.auth.webauthn.enabled {
        return disabled(Some(&start.0));
    }
    let store = match auth_store_or_unavailable(&state, Some(&start.0)) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match store
        .delete_webauthn_credential(current.user.id, credential_id)
        .await
    {
        Ok(true) => {
            audit(
                store,
                Some(current.user.id),
                Some(&current.user.email_address),
                "webauthn.credential.delete",
                &headers,
            )
            .await;
            render_deleted(Some(&start.0))
        }
        Ok(false) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

// -------------------------------------------------------------- login

#[derive(Debug, Deserialize)]
pub(crate) struct LoginStartBody {
    email_address: Option<String>,
}

/// `POST /api/v2/auth/webauthn/login/start` — request options for
/// `navigator.credentials.get()`. Unknown addresses (and accounts
/// without passkeys) receive the same response shape with deterministic
/// fake credential ids — no user enumeration.
pub(crate) async fn login_start(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Json(body): Json<LoginStartBody>,
) -> ApiResponse {
    if !state.config.auth.webauthn.enabled {
        return disabled(Some(&start.0));
    }
    let store = match auth_store_or_unavailable(&state, Some(&start.0)) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let Some(email) = body.email_address.filter(|email| !email.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: email_address",
        );
    };
    let webauthn = match build_webauthn(&state) {
        Ok(webauthn) => webauthn,
        Err(message) => return misconfigured(Some(&start.0), &message),
    };

    if let Ok(Some(user)) = store.user_by_email(&email).await {
        let credentials = store
            .list_webauthn_credentials(user.id)
            .await
            .unwrap_or_default();
        let passkeys: Vec<Passkey> = credentials
            .iter()
            .filter_map(|credential| serde_json::from_str(&credential.credential_json).ok())
            .collect();
        if !passkeys.is_empty() {
            match webauthn.start_passkey_authentication(&passkeys) {
                Ok((options, authentication)) => {
                    let (Ok(options), Ok(authentication_json)) = (
                        serde_json::to_value(&options),
                        serde_json::to_string(&authentication),
                    ) else {
                        return misconfigured(Some(&start.0), "could not serialize the ceremony");
                    };
                    let Some(challenge) = challenge_of(&options) else {
                        return misconfigured(Some(&start.0), "request options carry no challenge");
                    };
                    if let Err(error) = store
                        .create_webauthn_state(
                            &format!("login:{challenge}"),
                            Some(user.id),
                            &authentication_json,
                            Utc::now() + Duration::minutes(STATE_TTL_MINUTES),
                        )
                        .await
                    {
                        return render_store_error(Some(&start.0), error);
                    }
                    return render_success(Some(&start.0), StatusCode::OK, options);
                }
                Err(error) => {
                    // fall through to the generic response — an error here
                    // must not reveal anything about the account either
                    tracing::warn!(%error, "start_passkey_authentication failed");
                }
            }
        }
    }
    render_success(
        Some(&start.0),
        StatusCode::OK,
        fake_request_options(&state, &email),
    )
}

/// Request options for an address that cannot really sign in — same
/// shape as the real thing, with credential ids derived deterministically
/// (HMAC over the address) so repeated probes see stable answers. No
/// ceremony state is stored; any finish attempt fails generically.
fn fake_request_options(state: &ApiState, email: &str) -> Value {
    static FAKE_HMAC_KEY: OnceLock<Vec<u8>> = OnceLock::new();
    let key = FAKE_HMAC_KEY.get_or_init(|| {
        WebauthnFakeCredentialGenerator::<FakePasskeyDistribution>::new_hmac_key()
            .map(|key| AsRef::<[u8]>::as_ref(&key).to_vec())
            .unwrap_or_else(|_| {
                let mut bytes = vec![0u8; 32];
                rand::thread_rng().fill_bytes(&mut bytes);
                bytes
            })
    });
    let fake_ids = WebauthnFakeCredentialGenerator::<FakePasskeyDistribution>::new(key)
        .and_then(|generator| generator.generate(email.to_lowercase().as_bytes()))
        .unwrap_or_default();
    let mut challenge = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut challenge);
    let options = RequestChallengeResponse {
        public_key: PublicKeyCredentialRequestOptions {
            challenge,
            timeout: Some(FAKE_TIMEOUT_MS),
            rp_id: state.config.auth.webauthn.rp_id.clone(),
            allow_credentials: fake_ids
                .into_iter()
                .map(|id| AllowCredentials {
                    type_: "public-key".into(),
                    id,
                    transports: None,
                })
                .collect(),
            user_verification: UserVerificationPolicy::Required,
            hints: None,
            extensions: None,
        },
        mediation: None,
    };
    serde_json::to_value(&options).unwrap_or_else(|_| json!({}))
}

#[derive(Debug, Deserialize)]
pub(crate) struct LoginFinishBody {
    credential: Option<Value>,
}

/// `POST /api/v2/auth/webauthn/login/finish` — `{ credential }` from
/// `navigator.credentials.get()`. Verifies the assertion (including the
/// signature-counter clone detection of `webauthn-rs`), persists the
/// updated counter, and issues a session exactly like a password login.
/// Every failure mode answers the same generic `InvalidCredentials`.
pub(crate) async fn login_finish(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginFinishBody>,
) -> ApiResponse {
    if !state.config.auth.webauthn.enabled {
        return disabled(Some(&start.0));
    }
    let store = match auth_store_or_unavailable(&state, Some(&start.0)) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let invalid = |start: &RequestStart| {
        render_error(
            Some(start),
            StatusCode::UNAUTHORIZED,
            "InvalidCredentials",
            "The passkey could not be verified",
        )
    };
    let Some(credential) = body.credential else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: credential",
        );
    };
    let Ok(credential) = serde_json::from_value::<PublicKeyCredential>(credential) else {
        return invalid(&start.0);
    };
    let Some(challenge) = client_challenge(&credential.response.client_data_json) else {
        return invalid(&start.0);
    };
    let state_row = match store
        .consume_webauthn_state(&format!("login:{challenge}"), Utc::now())
        .await
    {
        Ok(state_row) => state_row,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some((Some(user_id), authentication_json)) = state_row else {
        return invalid(&start.0);
    };
    let Ok(authentication) = serde_json::from_str::<PasskeyAuthentication>(&authentication_json)
    else {
        return misconfigured(Some(&start.0), "stored authentication state is corrupt");
    };
    let user = match state.store.user_by_id(user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return invalid(&start.0),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    // A lockout from repeated password failures also blocks passkey logins.
    if let Ok(Some(user_auth)) = store.user_auth(user.id).await {
        if let Some(locked_until) = user_auth.locked_until {
            if locked_until > Utc::now() {
                audit(
                    store,
                    Some(user.id),
                    Some(&user.email_address),
                    "login.locked",
                    &headers,
                )
                .await;
                return render_error(
                    Some(&start.0),
                    StatusCode::FORBIDDEN,
                    "AccountLocked",
                    "This account is temporarily locked after repeated failed logins",
                );
            }
        }
    }
    let webauthn = match build_webauthn(&state) {
        Ok(webauthn) => webauthn,
        Err(message) => return misconfigured(Some(&start.0), &message),
    };
    // finish_passkey_authentication enforces origin/RP binding and rejects
    // regressed signature counters (possible cloned authenticator).
    let result = match webauthn.finish_passkey_authentication(&credential, &authentication) {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(user = %user.email_address, %error, "webauthn login failed");
            audit(
                store,
                Some(user.id),
                Some(&user.email_address),
                "webauthn.login.failure",
                &headers,
            )
            .await;
            return invalid(&start.0);
        }
    };
    let credential_id = URL_SAFE_NO_PAD.encode(result.cred_id());
    let stored = match store
        .webauthn_credential_by_credential_id(&credential_id)
        .await
    {
        Ok(stored) => stored,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    // The asserted credential must (still) exist and belong to the account
    // the ceremony was started for — a deleted passkey cannot sign in.
    let Some(stored) = stored.filter(|stored| stored.user_id == user.id) else {
        audit(
            store,
            Some(user.id),
            Some(&user.email_address),
            "webauthn.login.failure",
            &headers,
        )
        .await;
        return invalid(&start.0);
    };
    // Persist the updated signature counter / backup flags and the use.
    if let Ok(mut passkey) = serde_json::from_str::<Passkey>(&stored.credential_json) {
        let _ = passkey.update_credential(&result);
        if let Ok(updated_json) = serde_json::to_string(&passkey) {
            let _ = store
                .update_webauthn_credential(stored.id, &updated_json, Utc::now())
                .await;
        }
    }
    if let Err(error) = store
        .set_login_state(user.id, 0, None, Some(Utc::now()))
        .await
    {
        return render_store_error(Some(&start.0), error);
    }
    audit(
        store,
        Some(user.id),
        Some(&user.email_address),
        "webauthn.login",
        &headers,
    )
    .await;
    match issue_session(store, &state, &user, &headers).await {
        Ok((token, session)) => render_success(
            Some(&start.0),
            StatusCode::CREATED,
            json!({
                "session_token": token,
                "expires_at": session.expires_at,
                "user": user_json(&user),
            }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}
