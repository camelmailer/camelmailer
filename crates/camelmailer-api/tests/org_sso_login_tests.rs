//! Tenant SSO login flow, tested against a local mock OIDC identity
//! provider (discovery + JWKS + token endpoint issuing RSA-signed ID
//! tokens). The token endpoint echoes the authorization `code` back as the
//! `nonce` claim, which lets tests drive the flow without a browser.

use axum::body::Body;
use axum::extract::Form;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use camelmailer_api::{build_org_sso_login_router, ApiState};
use camelmailer_core::{
    AdminStore, AuthStore, MemoryStore, NewOrgEmailDomain, NewOrgSsoConnection, NewOrganization,
    OrgSsoStore, Role, SsoKind,
};
use http_body_util::BodyExt;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;

fn idp_key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap())
}

fn sign_id_token(claims: &Value) -> String {
    use rsa::pkcs1v15::SigningKey;
    use rsa::sha2::Sha256;
    use rsa::signature::{SignatureEncoding, Signer};
    let header = URL_SAFE_NO_PAD.encode(json!({ "alg": "RS256", "kid": "test" }).to_string());
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    let message = format!("{header}.{payload}");
    let signing_key = SigningKey::<Sha256>::new(idp_key().clone());
    let signature = signing_key.sign(message.as_bytes());
    format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}

/// Start a mock IdP that asserts the given email/name; returns its issuer.
async fn start_mock_idp(email: &'static str, name: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let issuer_for_discovery = issuer.clone();
    let issuer_for_token = issuer.clone();

    let app = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move || {
                let issuer = issuer_for_discovery.clone();
                async move {
                    Json(json!({
                        "issuer": issuer,
                        "authorization_endpoint": format!("{issuer}/authorize"),
                        "token_endpoint": format!("{issuer}/token"),
                        "jwks_uri": format!("{issuer}/jwks"),
                    }))
                }
            }),
        )
        .route(
            "/jwks",
            get(|| async {
                let public = idp_key().to_public_key();
                Json(json!({
                    "keys": [{
                        "kty": "RSA",
                        "kid": "test",
                        "alg": "RS256",
                        "use": "sig",
                        "n": URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
                        "e": URL_SAFE_NO_PAD.encode(public.e().to_bytes_be()),
                    }]
                }))
            }),
        )
        .route(
            "/token",
            post(move |Form(form): Form<Vec<(String, String)>>| {
                let issuer = issuer_for_token.clone();
                async move {
                    let field = |name: &str| {
                        form.iter()
                            .find(|(key, _)| key == name)
                            .map(|(_, value)| value.clone())
                            .unwrap_or_default()
                    };
                    assert_eq!(field("grant_type"), "authorization_code");
                    assert!(!field("code_verifier").is_empty(), "PKCE verifier missing");
                    let now = chrono::Utc::now().timestamp();
                    let id_token = sign_id_token(&json!({
                        "iss": issuer,
                        "aud": "client-1",
                        "sub": "sso-user-1",
                        "email": email,
                        "name": name,
                        "nonce": field("code"),
                        "iat": now,
                        "exp": now + 300,
                    }));
                    Json(json!({
                        "access_token": "at-1",
                        "token_type": "Bearer",
                        "id_token": id_token,
                    }))
                }
            }),
        );

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    issuer
}

struct Harness {
    app: Router,
    store: Arc<MemoryStore>,
    org_id: u64,
}

/// Build an app with one organization, a verified `acme.test` domain, and
/// an OIDC connection pointing at `issuer`.
async fn harness(issuer: &str) -> Harness {
    let store = Arc::new(MemoryStore::new());
    let org = store
        .create_organization(NewOrganization {
            name: "Acme".into(),
            permalink: "acme".into(),
        })
        .await
        .unwrap();
    let domain = store
        .create_org_email_domain(NewOrgEmailDomain {
            organization_id: org.id,
            domain: "acme.test".into(),
            verification_token: "tok".into(),
        })
        .await
        .unwrap();
    store
        .mark_org_email_domain_verified(domain.id)
        .await
        .unwrap();
    store
        .create_org_sso_connection(NewOrgSsoConnection {
            organization_id: org.id,
            kind: SsoKind::Oidc,
            name: "Acme Okta".into(),
            enabled: true,
            config: json!({
                "issuer": issuer,
                "client_id": "client-1",
                "client_secret": "client-secret",
            }),
            default_role: Role::Member,
            auto_provision: true,
        })
        .await
        .unwrap();

    let config = camelmailer_config::Config::default();
    let state = ApiState::full(store.clone(), None, Some(store.clone()), None, config)
        .with_org_sso_store(store.clone());
    let app = build_org_sso_login_router(state);
    Harness {
        app,
        store,
        org_id: org.id,
    }
}

async fn get_req(app: &Router, path: &str, accept_json: bool) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(path);
    if accept_json {
        builder = builder.header("accept", "application/json");
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn post_json(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn url_param(url: &str, name: &str) -> String {
    url.split(&format!("{name}="))
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn discover_routes_a_known_email_to_its_connections() {
    let issuer = start_mock_idp("alice@acme.test", "Alice Example").await;
    let h = harness(&issuer).await;

    let (status, body) = post_json(
        &h.app,
        "/api/v2/auth/org-sso/discover",
        json!({ "email": "alice@acme.test" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let connections = body["data"]["connections"].as_array().unwrap();
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0]["kind"], "oidc");
    assert!(connections[0]["start_url"]
        .as_str()
        .unwrap()
        .ends_with("/start"));

    // an unverified domain resolves to nothing (password fallback)
    let (_, body) = post_json(
        &h.app,
        "/api/v2/auth/org-sso/discover",
        json!({ "email": "someone@unknown.test" }),
    )
    .await;
    assert!(body["data"]["connections"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_full_login_provisions_the_user_and_joins_the_org() {
    let issuer = start_mock_idp("alice@acme.test", "Alice Example").await;
    let h = harness(&issuer).await;

    // discover the connection id
    let (_, body) = post_json(
        &h.app,
        "/api/v2/auth/org-sso/discover",
        json!({ "email": "alice@acme.test" }),
    )
    .await;
    let connection_id = body["data"]["connections"][0]["id"].as_u64().unwrap();

    // start: read the authorization URL, pull state + nonce out of it
    let (status, body) = get_req(
        &h.app,
        &format!("/api/v2/auth/org-sso/{connection_id}/start"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let url = body["data"]["authorization_url"].as_str().unwrap();
    let state = url_param(url, "state");
    let nonce = url_param(url, "nonce");
    assert!(state.starts_with(&format!("{connection_id}~")));

    // callback: the mock echoes code -> nonce, so pass the nonce as the code
    let (status, body) = get_req(
        &h.app,
        &format!("/api/v2/auth/org-sso/callback?code={nonce}&state={state}"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["data"]["session_token"].as_str().is_some());
    assert_eq!(body["data"]["user"]["email_address"], "alice@acme.test");

    // the provisioned user is a member of the organization
    let user = h
        .store
        .user_by_email("alice@acme.test")
        .await
        .unwrap()
        .unwrap();
    let membership = h
        .store
        .membership(h.org_id, user.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(membership.role, Role::Member);
}

#[tokio::test]
async fn a_login_is_rejected_when_the_idp_email_domain_is_not_verified() {
    // the IdP asserts an email whose domain the org never verified
    let issuer = start_mock_idp("mallory@evil.test", "Mallory").await;
    let h = harness(&issuer).await;
    let connection_id = h.store.list_org_sso_connections(h.org_id).await.unwrap()[0].id;

    let (_, body) = get_req(
        &h.app,
        &format!("/api/v2/auth/org-sso/{connection_id}/start"),
        true,
    )
    .await;
    let url = body["data"]["authorization_url"].as_str().unwrap();
    let state = url_param(url, "state");
    let nonce = url_param(url, "nonce");

    let (status, body) = get_req(
        &h.app,
        &format!("/api/v2/auth/org-sso/callback?code={nonce}&state={state}"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // no account was provisioned
    assert!(h
        .store
        .user_by_email("mallory@evil.test")
        .await
        .unwrap()
        .is_none());
}
