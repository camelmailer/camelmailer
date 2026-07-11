//! Social sign-in (`auth.sso_providers`), tested against local mock
//! providers: a mock OIDC IdP (discovery + JWKS + token endpoint, RSA-signed
//! ID tokens — the same approach as the enterprise OIDC tests) and a mock
//! GitHub (access_token + /user + /user/emails behind the `GithubOauth`
//! trait's HTTP implementation pointed at the mock).

use axum::body::Body;
use axum::extract::Form;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use camelmailer_api::{build_auth_router, build_sso_router, ApiState, HttpGithub};
use camelmailer_config::SsoProvider;
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

fn jwks_json() -> Value {
    let public = idp_key().to_public_key();
    json!({
        "keys": [{
            "kty": "RSA",
            "kid": "test",
            "alg": "RS256",
            "use": "sig",
            "n": URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(public.e().to_bytes_be()),
        }]
    })
}

/// Start a mock OIDC IdP under the given path prefix (e.g. "" or
/// "/common/v2.0"); returns its issuer URL. The token endpoint echoes the
/// authorization `code` back as the `nonce` claim (which lets tests drive
/// the flow without a browser) and stamps `iss_claim` — or the issuer
/// itself when `None` — into the token.
async fn start_mock_idp(
    prefix: &str,
    client_id: &'static str,
    email: &'static str,
    name: &'static str,
    iss_claim: Option<&'static str>,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}{prefix}", listener.local_addr().unwrap());
    let issuer_for_discovery = issuer.clone();
    let issuer_for_token = issuer.clone();

    let app = Router::new()
        .route(
            &format!("{prefix}/.well-known/openid-configuration"),
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
            &format!("{prefix}/jwks"),
            get(|| async { Json(jwks_json()) }),
        )
        .route(
            &format!("{prefix}/token"),
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
                    assert_eq!(field("client_id"), client_id);
                    let now = chrono::Utc::now().timestamp();
                    let id_token = sign_id_token(&json!({
                        "iss": iss_claim.map(str::to_string).unwrap_or(issuer),
                        "aud": client_id,
                        "sub": format!("sso-{client_id}"),
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

/// Start a mock GitHub (OAuth token endpoint + /user + /user/emails);
/// returns its base URL. Like the IdP mock it reflects the authorization
/// `code`: the code becomes the access token, which the API endpoints
/// expect back as the bearer token.
async fn start_mock_github(
    login: &'static str,
    name: Option<&'static str>,
    emails: Value,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());

    let app = Router::new()
        .route(
            "/login/oauth/access_token",
            post(|Form(form): Form<Vec<(String, String)>>| async move {
                let field = |name: &str| {
                    form.iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.clone())
                        .unwrap_or_default()
                };
                assert_eq!(field("client_id"), "gh-client");
                assert_eq!(field("client_secret"), "gh-secret");
                assert!(!field("code").is_empty(), "missing code");
                Json(json!({
                    "access_token": format!("token-for-{}", field("code")),
                    "token_type": "bearer",
                }))
            }),
        )
        .route(
            "/user",
            get(move |headers: axum::http::HeaderMap| async move {
                assert!(headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with("Bearer token-for-")));
                Json(json!({ "id": 583231, "login": login, "name": name }))
            }),
        )
        .route("/user/emails", get(move || async move { Json(emails) }));

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    base
}

fn oidc_provider(id: &str, name: &str, issuer: &str, client_id: &str) -> SsoProvider {
    serde_yaml::from_str(&format!(
        "{{ id: {id}, type: oidc, name: {name}, issuer: {issuer:?}, client_id: {client_id}, client_secret: secret-{client_id} }}"
    ))
    .unwrap()
}

fn github_provider() -> SsoProvider {
    serde_yaml::from_str(
        "{ id: github, type: github, name: GitHub, client_id: gh-client, client_secret: gh-secret }",
    )
    .unwrap()
}

struct Harness {
    app: Router,
    store: Arc<MemoryStore>,
}

fn harness_with_github(
    providers: Vec<SsoProvider>,
    github_base: Option<&str>,
    mutate: impl FnOnce(&mut camelmailer_config::Config),
) -> Harness {
    let mut config = camelmailer_config::Config::default();
    config.auth.sso_providers = providers;
    mutate(&mut config);
    config.validate().unwrap();
    let store = Arc::new(MemoryStore::new());
    let github: Arc<dyn camelmailer_api::GithubOauth> = match github_base {
        Some(base) => Arc::new(HttpGithub::with_base_urls(
            &format!("{base}/login/oauth"),
            base,
        )),
        None => Arc::new(HttpGithub::default()),
    };
    let state = ApiState::full_with_github(
        store.clone(),
        None,
        Some(store.clone()),
        None,
        config,
        github,
    );
    let app = build_sso_router(state.clone()).merge(build_auth_router(state));
    Harness { app, store }
}

fn harness(providers: Vec<SsoProvider>) -> Harness {
    harness_with_github(providers, None, |_| {})
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

    /// Run /start for the given provider and return (state, nonce) parsed
    /// from the authorization URL.
    async fn start_login(&self, provider_id: &str) -> (String, String) {
        let (status, body, _) = self
            .get(&format!("/api/v2/auth/sso/{provider_id}/start"), true)
            .await;
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

    /// Complete a full code-flow login. Both mocks reflect the `code`
    /// (the IdP signs it as the nonce, GitHub derives the access token),
    /// so passing the nonce as code yields a valid login.
    async fn sso_login(&self, provider_id: &str) -> (StatusCode, Value) {
        let (state, nonce) = self.start_login(provider_id).await;
        let (status, body, _) = self
            .get(
                &format!("/api/v2/auth/sso/{provider_id}/callback?code={nonce}&state={state}"),
                false,
            )
            .await;
        (status, body)
    }

    async fn me(&self, token: &str) -> StatusCode {
        self.app
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
            .unwrap()
            .status()
    }
}

// ------------------------------------------------------------- features

#[tokio::test]
async fn features_lists_providers_without_secrets() {
    let h = harness(vec![
        oidc_provider("google", "Google", "https://accounts.google.com", "g-1"),
        github_provider(),
    ]);
    let (status, body, _) = h.get("/api/v2/auth/features", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["sso"],
        json!([
            { "id": "google", "name": "Google", "type": "oidc" },
            { "id": "github", "name": "GitHub", "type": "github" },
        ])
    );
    assert!(!body.to_string().contains("secret"), "{body}");
}

#[tokio::test]
async fn features_is_empty_without_providers() {
    let h = harness(vec![]);
    let (status, body, _) = h.get("/api/v2/auth/features", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["sso"], json!([]));
}

// ------------------------------------------------------- provider lookup

#[tokio::test]
async fn an_unknown_provider_id_is_a_404() {
    let h = harness(vec![github_provider()]);
    for path in [
        "/api/v2/auth/sso/nope/start",
        "/api/v2/auth/sso/nope/callback?code=x&state=y",
    ] {
        let (status, body, _) = h.get(path, true).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body["error"]["code"], "SSOProviderNotFound", "{path}");
    }
}

// ------------------------------------------------------------- OIDC flow

#[tokio::test]
async fn an_oidc_provider_logs_in_end_to_end() {
    let issuer = start_mock_idp("", "g-1", "ada@corp.example", "Ada Lovelace", None).await;
    let h = harness(vec![oidc_provider("google", "Google", &issuer, "g-1")]);

    // browser-style start: a 307 redirect carrying state, nonce and PKCE
    let (status, _, location) = h.get("/api/v2/auth/sso/google/start", false).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let location = location.unwrap();
    assert!(location.starts_with(&format!("{issuer}/authorize?")));
    for param in [
        "state=",
        "nonce=",
        "code_challenge=",
        "code_challenge_method=S256",
        "client_id=g-1",
        "redirect_uri=https%3A%2F%2Fpostal.example.com%2Fapi%2Fv2%2Fauth%2Fsso%2Fgoogle%2Fcallback",
    ] {
        assert!(location.contains(param), "missing {param} in {location}");
    }

    // full login provisions the account and the session works on /me
    let (status, body) = h.sso_login("google").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["user"]["email_address"], "ada@corp.example");
    assert_eq!(body["data"]["user"]["first_name"], "Ada");
    assert_eq!(body["data"]["user"]["last_name"], "Lovelace");
    let token = body["data"]["session_token"].as_str().unwrap();
    assert_eq!(h.me(token).await, StatusCode::OK);

    // a second login maps onto the same account (linked by subject)
    let (status, _) = h.sso_login("google").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(h.store.list_users().await.unwrap().len(), 1);

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
async fn the_login_state_is_single_use_and_bogus_states_fail() {
    let issuer = start_mock_idp("", "g-1", "ada@corp.example", "Ada", None).await;
    let h = harness(vec![oidc_provider("google", "Google", &issuer, "g-1")]);

    let (state, nonce) = h.start_login("google").await;
    let (status, _, _) = h
        .get(
            &format!("/api/v2/auth/sso/google/callback?code={nonce}&state={state}"),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // replaying the same state fails …
    let (status, body, _) = h
        .get(
            &format!("/api/v2/auth/sso/google/callback?code={nonce}&state={state}"),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "SSOError");
    assert!(body["error"]["message"].as_str().unwrap().contains("state"));

    // … and so does a state that never existed
    let (status, body, _) = h
        .get(
            "/api/v2/auth/sso/google/callback?code=x&state=made-up",
            false,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"].as_str().unwrap().contains("state"));
}

#[tokio::test]
async fn a_state_is_bound_to_its_provider() {
    let issuer = start_mock_idp("", "g-1", "ada@corp.example", "Ada", None).await;
    let github = start_mock_github(
        "octocat",
        None,
        json!([{ "email": "ada@corp.example", "primary": true, "verified": true }]),
    )
    .await;
    let h = harness_with_github(
        vec![
            oidc_provider("google", "Google", &issuer, "g-1"),
            github_provider(),
        ],
        Some(&github),
        |_| {},
    );

    // a state started with Google cannot be redeemed on the GitHub callback
    let (state, nonce) = h.start_login("google").await;
    let (status, body, _) = h
        .get(
            &format!("/api/v2/auth/sso/github/callback?code={nonce}&state={state}"),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["error"]["message"].as_str().unwrap().contains("state"));
}

#[tokio::test]
async fn a_nonce_mismatch_is_rejected() {
    let issuer = start_mock_idp("", "g-1", "ada@corp.example", "Ada", None).await;
    let h = harness(vec![oidc_provider("google", "Google", &issuer, "g-1")]);

    let (state, _nonce) = h.start_login("google").await;
    // the mock signs whatever `code` we send as the nonce — send garbage
    let (status, body, _) = h
        .get(
            &format!("/api/v2/auth/sso/google/callback?code=wrong-nonce&state={state}"),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"].as_str().unwrap().contains("nonce"));
}

#[tokio::test]
async fn a_configured_frontend_receives_the_token_in_the_fragment() {
    let issuer = start_mock_idp("", "g-1", "ada@corp.example", "Ada", None).await;
    let h = harness_with_github(
        vec![oidc_provider("google", "Google", &issuer, "g-1")],
        None,
        |config| {
            config.auth.frontend_url = Some("https://mail.corp.example".into());
        },
    );

    let (state, nonce) = h.start_login("google").await;
    let (status, _, location) = h
        .get(
            &format!("/api/v2/auth/sso/google/callback?code={nonce}&state={state}"),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert!(location
        .unwrap()
        .starts_with("https://mail.corp.example/auth/callback#session_token="));
}

// -------------------------------------------------- Microsoft common iss

#[tokio::test]
async fn a_microsoft_common_issuer_accepts_any_tenant_iss() {
    // The configured issuer ends in /common/v2.0, the token's iss names a
    // concrete tenant — that must validate.
    let issuer = start_mock_idp(
        "/common/v2.0",
        "ms-1",
        "ada@corp.example",
        "Ada Lovelace",
        Some("https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"),
    )
    .await;
    let h = harness(vec![oidc_provider(
        "microsoft",
        "Microsoft",
        &issuer,
        "ms-1",
    )]);

    let (status, body) = h.sso_login("microsoft").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["user"]["email_address"], "ada@corp.example");
}

#[tokio::test]
async fn a_microsoft_common_issuer_rejects_foreign_issuer_hosts() {
    // Same multi-tenant configuration, but the signed iss points at a
    // non-Microsoft host — hard failure.
    let issuer = start_mock_idp(
        "/common/v2.0",
        "ms-1",
        "ada@corp.example",
        "Ada Lovelace",
        Some("https://evil.example/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"),
    )
    .await;
    let h = harness(vec![oidc_provider(
        "microsoft",
        "Microsoft",
        &issuer,
        "ms-1",
    )]);

    let (status, body) = h.sso_login("microsoft").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("tenant issuer"));
}

#[tokio::test]
async fn a_strict_issuer_still_rejects_iss_mismatches() {
    // A non-common issuer keeps strict equality: a token claiming a
    // Microsoft tenant iss is rejected when the configured issuer differs.
    let issuer = start_mock_idp(
        "",
        "g-1",
        "ada@corp.example",
        "Ada",
        Some("https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"),
    )
    .await;
    let h = harness(vec![oidc_provider("google", "Google", &issuer, "g-1")]);

    let (status, body) = h.sso_login("google").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

// ------------------------------------------------------------ GitHub flow

#[tokio::test]
async fn github_logs_in_end_to_end() {
    let github = start_mock_github(
        "octocat",
        Some("Grace Hopper"),
        json!([
            { "email": "old@corp.example", "primary": false, "verified": true },
            { "email": "Grace@Corp.example", "primary": true, "verified": true },
        ]),
    )
    .await;
    let h = harness_with_github(vec![github_provider()], Some(&github), |_| {});

    // start redirects to GitHub's authorize endpoint with our state
    let (status, _, location) = h.get("/api/v2/auth/sso/github/start", false).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let location = location.unwrap();
    assert!(location.starts_with(&format!("{github}/login/oauth/authorize?")));
    assert!(location.contains("client_id=gh-client"));
    assert!(location.contains("state="));

    // callback exchanges the code and picks the primary verified email
    let (status, body) = h.sso_login("github").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["user"]["email_address"], "grace@corp.example");
    assert_eq!(body["data"]["user"]["first_name"], "Grace");
    assert_eq!(body["data"]["user"]["last_name"], "Hopper");
    let token = body["data"]["session_token"].as_str().unwrap();
    assert_eq!(h.me(token).await, StatusCode::OK);

    // a second login reuses the account (linked by the GitHub user id)
    let (status, _) = h.sso_login("github").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(h.store.list_users().await.unwrap().len(), 1);
}

#[tokio::test]
async fn github_name_falls_back_to_the_login() {
    let github = start_mock_github(
        "octocat",
        None,
        json!([{ "email": "octo@corp.example", "primary": true, "verified": true }]),
    )
    .await;
    let h = harness_with_github(vec![github_provider()], Some(&github), |_| {});

    let (status, body) = h.sso_login("github").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["user"]["first_name"], "octocat");
    assert_eq!(body["data"]["user"]["last_name"], "");
}

#[tokio::test]
async fn github_without_a_verified_email_is_rejected() {
    let github = start_mock_github(
        "octocat",
        Some("Octo Cat"),
        json!([
            { "email": "primary@corp.example", "primary": true, "verified": false },
            { "email": "other@corp.example", "primary": false, "verified": false },
        ]),
    )
    .await;
    let h = harness_with_github(vec![github_provider()], Some(&github), |_| {});

    let (status, body) = h.sso_login("github").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "SSOEmailUnavailable");
    assert_eq!(h.store.list_users().await.unwrap().len(), 0);
}

#[tokio::test]
async fn github_links_an_existing_account_by_email() {
    let github = start_mock_github(
        "octocat",
        Some("Existing Person"),
        json!([{ "email": "existing@corp.example", "primary": true, "verified": true }]),
    )
    .await;
    let h = harness_with_github(vec![github_provider()], Some(&github), |_| {});
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

    let (status, body) = h.sso_login("github").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["user"]["id"], user.id);
    assert_eq!(h.store.list_users().await.unwrap().len(), 1);
    assert_eq!(
        h.store
            .user_by_sso_identity("github", "583231")
            .await
            .unwrap()
            .unwrap()
            .id,
        user.id
    );
}

// -------------------------------------------------------- policy per provider

#[tokio::test]
async fn email_domains_can_be_restricted_per_provider() {
    let github = start_mock_github(
        "octocat",
        Some("Ada L"),
        json!([{ "email": "ada@evil.example", "primary": true, "verified": true }]),
    )
    .await;
    let mut provider = github_provider();
    provider.allowed_email_domains = vec!["corp.example".into()];
    let h = harness_with_github(vec![provider], Some(&github), |_| {});

    let (status, body) = h.sso_login("github").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("domain"));
    assert_eq!(h.store.list_users().await.unwrap().len(), 0);
}

#[tokio::test]
async fn disabled_provisioning_only_admits_existing_accounts() {
    let issuer = start_mock_idp("", "g-1", "known@corp.example", "Known Person", None).await;
    let mut provider = oidc_provider("google", "Google", &issuer, "g-1");
    provider.auto_provision = false;
    let h = harness(vec![provider]);

    // unknown → rejected, nothing provisioned
    let (status, body) = h.sso_login("google").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("provisioning is disabled"));
    assert_eq!(h.store.list_users().await.unwrap().len(), 0);

    // an existing account may sign in and gets linked
    let user = h
        .store
        .create_user(NewUser {
            email_address: "known@corp.example".into(),
            first_name: "Known".into(),
            last_name: "Person".into(),
            admin: false,
        })
        .await
        .unwrap();
    let (status, body) = h.sso_login("google").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["user"]["id"], user.id);
}
