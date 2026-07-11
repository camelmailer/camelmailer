//! `/api/v2/auth/webauthn` — passkey registration, listing, deletion and
//! login, end to end against the router with the `webauthn-rs` soft
//! authenticator (`webauthn-authenticator-rs` `SoftPasskey`), plus the
//! `/api/v2/auth/features` discovery endpoint.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, ApiState};
use camelmailer_core::auth;
use camelmailer_core::{AuthStore, MemoryStore, NewUser};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::OnceLock;
use tower::ServiceExt;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_rs_proto::{CreationChallengeResponse, RequestChallengeResponse};

const PASSWORD: &str = "correct-horse-battery";
const RP_ORIGIN: &str = "https://app.example.com";

fn password_digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| auth::hash_password(PASSWORD).unwrap())
}

fn webauthn_config() -> camelmailer_config::Config {
    let mut config = camelmailer_config::Config::default();
    config.auth.webauthn.enabled = true;
    config.auth.webauthn.rp_id = "app.example.com".into();
    config.auth.webauthn.rp_origin = RP_ORIGIN.into();
    config.validate().unwrap();
    config
}

async fn build_app_with(
    config: camelmailer_config::Config,
) -> (Router, Arc<MemoryStore>, Arc<ApiState>) {
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::full(store.clone(), None, Some(store.clone()), None, config);
    (build_auth_router(state.clone()), store, state)
}

async fn create_user(store: &Arc<MemoryStore>, email: &str) -> camelmailer_core::User {
    use camelmailer_core::AdminStore;
    let user = store
        .create_user(NewUser {
            email_address: email.into(),
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            admin: false,
        })
        .await
        .unwrap();
    store
        .set_password_digest(user.id, password_digest())
        .await
        .unwrap();
    user
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn session_for(app: &Router, email: &str) -> String {
    let (status, body) = request(
        app,
        "POST",
        "/api/v2/auth/login",
        None,
        Some(json!({ "email_address": email, "password": PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["data"]["session_token"].as_str().unwrap().to_string()
}

fn origin() -> webauthn_rs::prelude::Url {
    webauthn_rs::prelude::Url::parse(RP_ORIGIN).unwrap()
}

/// Register a passkey through the two-step ceremony with the soft token.
async fn register_passkey(
    app: &Router,
    token: &str,
    authenticator: &mut SoftPasskey,
    name: &str,
) -> (StatusCode, Value) {
    let (status, body) = request(
        app,
        "POST",
        "/api/v2/auth/webauthn/register/start",
        Some(token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register/start failed: {body}");
    let options: CreationChallengeResponse = serde_json::from_value(body["data"].clone()).unwrap();
    let credential = authenticator.do_registration(origin(), options).unwrap();
    request(
        app,
        "POST",
        "/api/v2/auth/webauthn/register/finish",
        Some(token),
        Some(json!({ "name": name, "credential": credential })),
    )
    .await
}

// ------------------------------------------------------------ features

#[tokio::test]
async fn features_reflect_the_configuration() {
    let (app, _, _) = build_app_with(camelmailer_config::Config::default()).await;
    let (status, body) = request(&app, "GET", "/api/v2/auth/features", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["webauthn"], false);
    assert_eq!(body["data"]["registration"], false);
    assert_eq!(body["data"]["oidc"]["enabled"], false);

    let mut config = webauthn_config();
    config.auth.allow_registration = true;
    config.oidc.enabled = true;
    config.oidc.name = "Okta".into();
    config.oidc.issuer = "https://idp.example.com".into();
    config.oidc.identifier = Some("client-1".into());
    let (app, _, _) = build_app_with(config).await;
    let (status, body) = request(&app, "GET", "/api/v2/auth/features", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["webauthn"], true);
    assert_eq!(body["data"]["registration"], true);
    assert_eq!(
        body["data"]["oidc"],
        json!({ "enabled": true, "name": "Okta" })
    );
}

// ------------------------------------------------------------ disabled

#[tokio::test]
async fn every_webauthn_endpoint_answers_webauthn_disabled_when_off() {
    let (app, store, _) = build_app_with(camelmailer_config::Config::default()).await;
    create_user(&store, "ada@example.com").await;
    let token = session_for(&app, "ada@example.com").await;

    for (method, path, bearer, body) in [
        (
            "POST",
            "/api/v2/auth/webauthn/register/start",
            Some(&token),
            Some(json!({})),
        ),
        (
            "POST",
            "/api/v2/auth/webauthn/register/finish",
            Some(&token),
            Some(json!({ "name": "x", "credential": {} })),
        ),
        (
            "GET",
            "/api/v2/auth/webauthn/credentials",
            Some(&token),
            None,
        ),
        (
            "DELETE",
            "/api/v2/auth/webauthn/credentials/1",
            Some(&token),
            None,
        ),
        (
            "POST",
            "/api/v2/auth/webauthn/login/start",
            None,
            Some(json!({ "email_address": "ada@example.com" })),
        ),
        (
            "POST",
            "/api/v2/auth/webauthn/login/finish",
            None,
            Some(json!({ "credential": {} })),
        ),
    ] {
        let (status, body) = request(&app, method, path, bearer.map(|t| t.as_str()), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
        assert_eq!(body["error"]["code"], "WebAuthnDisabled", "{method} {path}");
    }
}

// ----------------------------------------------------- full happy path

#[tokio::test]
async fn register_list_login_and_use_the_session() {
    let (app, store, _) = build_app_with(webauthn_config()).await;
    create_user(&store, "ada@example.com").await;
    let token = session_for(&app, "ada@example.com").await;
    let mut authenticator = SoftPasskey::new(true);

    // register
    let (status, body) = register_passkey(&app, &token, &mut authenticator, "MacBook").await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "register/finish failed: {body}"
    );
    assert_eq!(body["data"]["credential"]["name"], "MacBook");
    let credential_row_id = body["data"]["credential"]["id"].as_u64().unwrap();
    assert!(body["data"]["credential"]["last_used_at"].is_null());

    // listed
    let (status, body) = request(
        &app,
        "GET",
        "/api/v2/auth/webauthn/credentials",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let credentials = body["data"]["credentials"].as_array().unwrap();
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0]["id"].as_u64().unwrap(), credential_row_id);
    assert_eq!(credentials[0]["name"], "MacBook");
    assert!(credentials[0]["created_at"].is_string());
    // no key material leaks through the listing
    assert!(credentials[0].get("credential_json").is_none());
    assert!(credentials[0].get("credential_id").is_none());

    // login start -> soft token -> finish
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/login/start",
        None,
        Some(json!({ "email_address": "ada@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let options: RequestChallengeResponse = serde_json::from_value(body["data"].clone()).unwrap();
    let assertion = authenticator.do_authentication(origin(), options).unwrap();
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/login/finish",
        None,
        Some(json!({ "credential": assertion })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "login/finish failed: {body}");
    // same response structure as a password login
    let session_token = body["data"]["session_token"].as_str().unwrap().to_string();
    assert_eq!(session_token.len(), 43);
    assert!(body["data"]["expires_at"].is_string());
    assert_eq!(body["data"]["user"]["email_address"], "ada@example.com");

    // the session works against /me
    let (status, body) = request(&app, "GET", "/api/v2/auth/me", Some(&session_token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["user"]["email_address"], "ada@example.com");

    // last_used_at is stamped and the signature counter was persisted
    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/auth/webauthn/credentials",
        Some(&token),
        None,
    )
    .await;
    assert!(body["data"]["credentials"][0]["last_used_at"].is_string());

    // audit trail: registration + passkey login
    let events = store.list_auth_events(10).await.unwrap();
    let names: Vec<&str> = events.iter().map(|event| event.event.as_str()).collect();
    assert!(names.contains(&"webauthn.register"), "{names:?}");
    assert!(names.contains(&"webauthn.login"), "{names:?}");
}

#[tokio::test]
async fn the_signature_counter_is_persisted_across_logins() {
    let (app, store, _) = build_app_with(webauthn_config()).await;
    let user = create_user(&store, "ada@example.com").await;
    let token = session_for(&app, "ada@example.com").await;
    let mut authenticator = SoftPasskey::new(true);
    let (status, _) = register_passkey(&app, &token, &mut authenticator, "Key").await;
    assert_eq!(status, StatusCode::CREATED);

    let counter = |store: &Arc<MemoryStore>| {
        let store = store.clone();
        async move {
            let credentials = AuthStore::list_webauthn_credentials(store.as_ref(), user.id)
                .await
                .unwrap();
            let passkey: Value = serde_json::from_str(&credentials[0].credential_json).unwrap();
            passkey["cred"]["counter"].as_u64().unwrap()
        }
    };
    let initial = counter(&store).await;

    for _ in 0..2 {
        let (_, body) = request(
            &app,
            "POST",
            "/api/v2/auth/webauthn/login/start",
            None,
            Some(json!({ "email_address": "ada@example.com" })),
        )
        .await;
        let options: RequestChallengeResponse =
            serde_json::from_value(body["data"].clone()).unwrap();
        let assertion = authenticator.do_authentication(origin(), options).unwrap();
        let (status, body) = request(
            &app,
            "POST",
            "/api/v2/auth/webauthn/login/finish",
            None,
            Some(json!({ "credential": assertion })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    // each use increments the soft token's counter and the store keeps up
    assert!(counter(&store).await > initial);
}

// ------------------------------------------------- enumeration hygiene

#[tokio::test]
async fn login_start_answers_generically_for_unknown_accounts() {
    let (app, store, _) = build_app_with(webauthn_config()).await;
    create_user(&store, "ada@example.com").await; // exists, but has no passkey

    for email in ["nobody@example.com", "ada@example.com"] {
        let (status, body) = request(
            &app,
            "POST",
            "/api/v2/auth/webauthn/login/start",
            None,
            Some(json!({ "email_address": email })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{email}");
        // same shape as a real challenge…
        assert!(
            body["data"]["publicKey"]["challenge"].is_string(),
            "{email}"
        );
        assert_eq!(body["data"]["publicKey"]["rpId"], "app.example.com");
        assert_eq!(body["data"]["publicKey"]["userVerification"], "required");
        // allowCredentials mirrors a believable account: possibly empty
        // (an account without passkeys), otherwise proper descriptors
        let allowed = body["data"]["publicKey"]["allowCredentials"]
            .as_array()
            .unwrap();
        for descriptor in allowed {
            assert_eq!(descriptor["type"], "public-key", "{email}");
            assert!(descriptor["id"].is_string(), "{email}");
        }
    }

    // …and deterministic per address: repeated probes see the same
    // credential ids (only the challenge changes)
    let probe = |app: Router| async move {
        let (_, body) = request(
            &app,
            "POST",
            "/api/v2/auth/webauthn/login/start",
            None,
            Some(json!({ "email_address": "nobody@example.com" })),
        )
        .await;
        body["data"]["publicKey"]["allowCredentials"].clone()
    };
    let first = probe(app.clone()).await;
    let second = probe(app.clone()).await;
    assert_eq!(first, second);
}

#[tokio::test]
async fn login_finish_without_a_ceremony_is_generically_rejected() {
    let (app, store, _) = build_app_with(webauthn_config()).await;
    create_user(&store, "ada@example.com").await;

    // a syntactically valid credential that belongs to no started ceremony
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/login/finish",
        None,
        Some(json!({ "credential": {
            "id": "AAAA",
            "rawId": "AAAA",
            "type": "public-key",
            "response": {
                "authenticatorData": "AAAA",
                "clientDataJSON": "eyJjaGFsbGVuZ2UiOiJBQUFBIn0",
                "signature": "AAAA",
                "userHandle": null
            },
            "clientExtensionResults": {}
        } })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "InvalidCredentials");

    // garbage that does not even parse gets the same answer
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/login/finish",
        None,
        Some(json!({ "credential": { "nonsense": true } })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "InvalidCredentials");
}

// ------------------------------------------------------ wrong RP/origin

#[tokio::test]
async fn a_credential_for_another_origin_is_rejected() {
    let (app, store, _) = build_app_with(webauthn_config()).await;
    create_user(&store, "ada@example.com").await;
    let token = session_for(&app, "ada@example.com").await;
    let mut authenticator = SoftPasskey::new(true);

    // Registration: the client answers for a different origin than the
    // server's configured rp_origin — the attestation must be rejected.
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/register/start",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let options: CreationChallengeResponse = serde_json::from_value(body["data"].clone()).unwrap();
    let evil = webauthn_rs::prelude::Url::parse("https://evil.example.com").unwrap();
    // (the client itself may refuse the cross-origin ceremony — equally fine)
    if let Ok(credential) = authenticator.do_registration(evil.clone(), options) {
        let (status, body) = request(
            &app,
            "POST",
            "/api/v2/auth/webauthn/register/finish",
            Some(&token),
            Some(json!({ "name": "Evil", "credential": credential })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["code"], "WebAuthnError");
    }
    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/auth/webauthn/credentials",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(body["data"]["credentials"].as_array().unwrap().len(), 0);

    // Login: register properly, then answer the challenge from the wrong
    // origin — the assertion must be rejected and no session issued.
    let (status, _) = register_passkey(&app, &token, &mut authenticator, "Key").await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/login/start",
        None,
        Some(json!({ "email_address": "ada@example.com" })),
    )
    .await;
    let options: RequestChallengeResponse = serde_json::from_value(body["data"].clone()).unwrap();
    if let Ok(assertion) = authenticator.do_authentication(evil, options) {
        let (status, body) = request(
            &app,
            "POST",
            "/api/v2/auth/webauthn/login/finish",
            None,
            Some(json!({ "credential": assertion })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(body["error"]["code"], "InvalidCredentials");
    }
}

// -------------------------------------------------- ceremony hygiene

#[tokio::test]
async fn registration_challenges_are_single_use_and_owner_bound() {
    let (app, store, _) = build_app_with(webauthn_config()).await;
    create_user(&store, "ada@example.com").await;
    create_user(&store, "eve@example.com").await;
    let ada = session_for(&app, "ada@example.com").await;
    let eve = session_for(&app, "eve@example.com").await;
    let mut authenticator = SoftPasskey::new(true);

    // Eve must not be able to finish a ceremony Ada started.
    let (_, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/register/start",
        Some(&ada),
        Some(json!({})),
    )
    .await;
    let options: CreationChallengeResponse = serde_json::from_value(body["data"].clone()).unwrap();
    let credential = authenticator.do_registration(origin(), options).unwrap();
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/register/finish",
        Some(&eve),
        Some(json!({ "name": "Stolen", "credential": credential.clone() })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "WebAuthnError");

    // The cross-user attempt consumed the state: Ada's own (legitimate)
    // finish now fails too — challenges are strictly single-use.
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/register/finish",
        Some(&ada),
        Some(json!({ "name": "Mine", "credential": credential })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "WebAuthnError");

    // a name is required
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/register/finish",
        Some(&ada),
        Some(json!({ "credential": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "ParameterMissing");
}

// ------------------------------------------------------------ deletion

#[tokio::test]
async fn deleting_the_passkey_removes_the_login_ability() {
    let (app, store, _) = build_app_with(webauthn_config()).await;
    create_user(&store, "ada@example.com").await;
    let token = session_for(&app, "ada@example.com").await;
    let mut authenticator = SoftPasskey::new(true);
    let (status, body) = register_passkey(&app, &token, &mut authenticator, "Key").await;
    assert_eq!(status, StatusCode::CREATED);
    let credential_row_id = body["data"]["credential"]["id"].as_u64().unwrap();

    // start a login while the passkey still exists…
    let (_, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/login/start",
        None,
        Some(json!({ "email_address": "ada@example.com" })),
    )
    .await;
    let options: RequestChallengeResponse = serde_json::from_value(body["data"].clone()).unwrap();

    // …delete it (allowed even as the last passkey — the password remains)…
    let (status, body) = request(
        &app,
        "DELETE",
        &format!("/api/v2/auth/webauthn/credentials/{credential_row_id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["deleted"], true);
    // deleting again → 404; deletion requires a session at all
    let (status, _) = request(
        &app,
        "DELETE",
        &format!("/api/v2/auth/webauthn/credentials/{credential_row_id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(
        &app,
        "DELETE",
        &format!("/api/v2/auth/webauthn/credentials/{credential_row_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // …and the in-flight assertion no longer signs anyone in.
    let assertion = authenticator.do_authentication(origin(), options).unwrap();
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/login/finish",
        None,
        Some(json!({ "credential": assertion })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "InvalidCredentials");

    // subsequent login/start falls back to the generic (fake) options
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/login/start",
        None,
        Some(json!({ "email_address": "ada@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["publicKey"]["challenge"].is_string());

    // the password login still works — a factor always remains
    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/auth/login",
        None,
        Some(json!({ "email_address": "ada@example.com", "password": PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

// -------------------------------------------- registration exclusions

#[tokio::test]
async fn register_start_excludes_already_registered_credentials() {
    let (app, store, _) = build_app_with(webauthn_config()).await;
    create_user(&store, "ada@example.com").await;
    let token = session_for(&app, "ada@example.com").await;
    let mut authenticator = SoftPasskey::new(true);
    let (status, _) = register_passkey(&app, &token, &mut authenticator, "Key").await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/webauthn/register/start",
        Some(&token),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let excluded = body["data"]["publicKey"]["excludeCredentials"]
        .as_array()
        .unwrap();
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0]["type"], "public-key");
}
