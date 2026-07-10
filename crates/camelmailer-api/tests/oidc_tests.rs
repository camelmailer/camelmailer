//! OIDC single sign-on, tested against a local mock identity provider
//! that serves discovery, JWKS and a token endpoint issuing RSA-signed
//! ID tokens.

use axum::body::Body;
use axum::extract::Form;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use camelmailer_api::{build_auth_router, build_oidc_router, ApiState};
use camelmailer_core::{AdminStore, AuthStore, MemoryStore, NewUser};
use http_body_util::BodyExt;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;

/// One RSA key for the whole test binary (keygen is slow in debug builds).
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

/// Start the mock IdP; returns its issuer URL. The token endpoint echoes
/// the authorization `code` back as the `nonce` claim, which lets tests
/// drive the flow without a browser.
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
}

async fn harness(issuer: &str, mutate: impl FnOnce(&mut camelmailer_config::Config)) -> Harness {
    let mut config = camelmailer_config::Config::default();
    config.oidc.enabled = true;
    config.oidc.issuer = issuer.to_string();
    config.oidc.identifier = Some("client-1".into());
    config.oidc.secret = Some("client-secret".into());
    mutate(&mut config);
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::full(store.clone(), None, Some(store.clone()), None, config);
    let app = build_oidc_router(state.clone()).merge(build_auth_router(state));
    Harness { app, store }
}

impl Harness {
    async fn get(&self, path: &str, accept_json: bool) -> (StatusCode, Value, Option<String>) {
        let mut builder = Request::builder().method("GET").uri(path);
        if accept_json {
            builder = builder.header("accept", "application/json");
        }
        let response = self
            .app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json, location)
    }

    /// Run /start and return (state, nonce) parsed from the authorization
    /// URL.
    async fn start_login(&self) -> (String, String) {
        let (status, body, _) = self.get("/api/v2/auth/oidc/start", true).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let url = body["data"]["authorization_url"].as_str().unwrap();
        let param = |name: &str| {
            url.split(&format!("{name}="))
                .nth(1)
                .unwrap()
                .split('&')
                .next()
                .unwrap()
                .to_string()
        };
        (param("state"), param("nonce"))
    }

    /// Complete a full code-flow login; the mock puts the `code` into the
    /// nonce claim, so passing the real nonce as code yields a valid token.
    async fn sso_login(&self) -> (StatusCode, Value) {
        let (state, nonce) = self.start_login().await;
        let (status, body, _) = self
            .get(
                &format!("/api/v2/auth/oidc/callback?code={nonce}&state={state}"),
                false,
            )
            .await;
        (status, body)
    }
}

#[tokio::test]
async fn start_redirects_to_the_authorization_endpoint() {
    let issuer = start_mock_idp("ada@corp.example", "Ada Lovelace").await;
    let h = harness(&issuer, |_| {}).await;

    // browser-style: a 307 redirect carrying state, nonce and PKCE
    let (status, _, location) = h.get("/api/v2/auth/oidc/start", false).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let location = location.unwrap();
    assert!(location.starts_with(&format!("{issuer}/authorize?")));
    for param in [
        "state=",
        "nonce=",
        "code_challenge=",
        "code_challenge_method=S256",
        "client_id=client-1",
    ] {
        assert!(location.contains(param), "missing {param} in {location}");
    }

    // SPA-style: JSON with the same URL
    let (status, body, _) = h.get("/api/v2/auth/oidc/start", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["authorization_url"]
        .as_str()
        .unwrap()
        .contains("response_type=code"));
}

#[tokio::test]
async fn a_full_login_provisions_the_account_and_issues_a_session() {
    let issuer = start_mock_idp("ada@corp.example", "Ada Lovelace").await;
    let h = harness(&issuer, |_| {}).await;

    let (status, body) = h.sso_login().await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["user"]["email_address"], "ada@corp.example");
    assert_eq!(body["data"]["user"]["first_name"], "Ada");
    assert_eq!(body["data"]["user"]["last_name"], "Lovelace");
    let token = body["data"]["session_token"].as_str().unwrap().to_string();

    // the session works against /me
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v2/auth/me")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // a second SSO login maps onto the same account (linked by sub)
    let (status, body) = h.sso_login().await;
    assert_eq!(status, StatusCode::CREATED);
    let users = h.store.list_users().await.unwrap();
    assert_eq!(users.len(), 1, "no duplicate account: {body}");

    // audit trail carries provision + logins
    let events: Vec<String> = h
        .store
        .list_auth_events(10)
        .await
        .unwrap()
        .iter()
        .map(|event| event.event.clone())
        .collect();
    assert!(events.contains(&"sso.provision".to_string()));
    assert!(events.iter().filter(|event| *event == "sso.login").count() >= 2);
}

#[tokio::test]
async fn sso_links_an_existing_account_by_email() {
    let issuer = start_mock_idp("existing@corp.example", "Existing Person").await;
    let h = harness(&issuer, |_| {}).await;
    let user = h
        .store
        .create_user(NewUser {
            email_address: "existing@corp.example".into(),
            first_name: "Existing".into(),
            last_name: "Person".into(),
            admin: false,
        })
        .await
        .unwrap();

    let (status, body) = h.sso_login().await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["user"]["id"], user.id);
    let auth = h.store.user_auth(user.id).await.unwrap().unwrap();
    assert_eq!(auth.oidc_sub.as_deref(), Some("sso-user-1"));
    assert_eq!(h.store.list_users().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_login_state_is_single_use() {
    let issuer = start_mock_idp("ada@corp.example", "Ada").await;
    let h = harness(&issuer, |_| {}).await;

    let (state, nonce) = h.start_login().await;
    let (status, _, _) = h
        .get(
            &format!("/api/v2/auth/oidc/callback?code={nonce}&state={state}"),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // replaying the same state fails
    let (status, body, _) = h
        .get(
            &format!("/api/v2/auth/oidc/callback?code={nonce}&state={state}"),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "SSOError");
}

#[tokio::test]
async fn a_nonce_mismatch_is_rejected() {
    let issuer = start_mock_idp("ada@corp.example", "Ada").await;
    let h = harness(&issuer, |_| {}).await;

    let (state, _nonce) = h.start_login().await;
    // the mock signs whatever `code` we send as the nonce — send garbage
    let (status, body, _) = h
        .get(
            &format!("/api/v2/auth/oidc/callback?code=wrong-nonce&state={state}"),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"].as_str().unwrap().contains("nonce"));
}

#[tokio::test]
async fn email_domains_can_be_restricted() {
    let issuer = start_mock_idp("ada@evil.example", "Ada").await;
    let h = harness(&issuer, |config| {
        config.oidc.allowed_email_domains = vec!["corp.example".into()];
    })
    .await;

    let (status, body) = h.sso_login().await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("domain"));
    assert_eq!(h.store.list_users().await.unwrap().len(), 0);
}

#[tokio::test]
async fn provisioning_can_be_disabled() {
    let issuer = start_mock_idp("unknown@corp.example", "Unknown").await;
    let h = harness(&issuer, |config| {
        config.oidc.auto_provision = false;
    })
    .await;

    let (status, body) = h.sso_login().await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("provisioning is disabled"));
}

#[tokio::test]
async fn a_configured_frontend_receives_the_token_in_the_fragment() {
    let issuer = start_mock_idp("ada@corp.example", "Ada").await;
    let h = harness(&issuer, |config| {
        config.auth.frontend_url = Some("https://mail.corp.example".into());
    })
    .await;

    let (state, nonce) = h.start_login().await;
    let (status, _, location) = h
        .get(
            &format!("/api/v2/auth/oidc/callback?code={nonce}&state={state}"),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let location = location.unwrap();
    assert!(location.starts_with("https://mail.corp.example/auth/callback#session_token="));
}

#[tokio::test]
async fn sso_endpoints_404_when_disabled() {
    let issuer = start_mock_idp("ada@corp.example", "Ada").await;
    let h = harness(&issuer, |config| {
        config.oidc.enabled = false;
    })
    .await;
    let (status, body, _) = h.get("/api/v2/auth/oidc/start", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "SSODisabled");
}
