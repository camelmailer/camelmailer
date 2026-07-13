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
use camelmailer_api::{build_org_sso_login_router, ApiState, GithubEmail, GithubOauth, GithubUser};
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

// ---------------------------------------------------------------- GitHub

struct MockGithub;

#[async_trait::async_trait]
impl GithubOauth for MockGithub {
    fn authorize_endpoint(&self) -> String {
        "https://github.test/login/oauth/authorize".into()
    }
    async fn exchange_code(
        &self,
        _client_id: &str,
        _client_secret: &str,
        _code: &str,
        _redirect_uri: &str,
    ) -> Result<String, String> {
        Ok("gh-token".into())
    }
    async fn fetch_user(&self, _token: &str) -> Result<GithubUser, String> {
        Ok(GithubUser {
            id: 42,
            login: "octo".into(),
            name: Some("Octo Cat".into()),
        })
    }
    async fn fetch_emails(&self, _token: &str) -> Result<Vec<GithubEmail>, String> {
        Ok(vec![GithubEmail {
            email: "octo@acme.test".into(),
            primary: true,
            verified: true,
        }])
    }
}

async fn github_harness() -> Harness {
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
            kind: SsoKind::Github,
            name: "GitHub".into(),
            enabled: true,
            config: json!({ "client_id": "gh-client", "client_secret": "gh-secret" }),
            default_role: Role::Member,
            auto_provision: true,
        })
        .await
        .unwrap();
    let config = camelmailer_config::Config::default();
    let state = ApiState::full_with_github(
        store.clone(),
        None,
        Some(store.clone()),
        None,
        config,
        Arc::new(MockGithub),
    )
    .with_org_sso_store(store.clone());
    Harness {
        app: build_org_sso_login_router(state),
        store,
        org_id: org.id,
    }
}

#[tokio::test]
async fn a_github_login_provisions_and_joins_the_org() {
    let h = github_harness().await;
    let connection_id = h.store.list_org_sso_connections(h.org_id).await.unwrap()[0].id;

    let (_, body) = get_req(
        &h.app,
        &format!("/api/v2/auth/org-sso/{connection_id}/start"),
        true,
    )
    .await;
    let url = body["data"]["authorization_url"].as_str().unwrap();
    assert!(url.starts_with("https://github.test/login/oauth/authorize"));
    let state = url_param(url, "state");

    let (status, body) = get_req(
        &h.app,
        &format!("/api/v2/auth/org-sso/callback?code=any&state={state}"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["user"]["email_address"], "octo@acme.test");

    let user = h
        .store
        .user_by_email("octo@acme.test")
        .await
        .unwrap()
        .unwrap();
    assert!(h
        .store
        .membership(h.org_id, user.id)
        .await
        .unwrap()
        .is_some());
}

// ------------------------------------------------------------------ SAML

const ORG_ACS: &str = "https://postal.example.com/api/v2/auth/org-sso/acs";
const ORG_AUDIENCE: &str = "https://postal.example.com";

/// Self-signed X.509 certificate for `idp_key()`, as pasted-in PEM.
fn idp_certificate_pem() -> &'static str {
    use rsa::pkcs8::EncodePrivateKey;
    static PEM: OnceLock<String> = OnceLock::new();
    PEM.get_or_init(|| {
        let pkcs8 = idp_key().to_pkcs8_der().unwrap();
        let key_pair = rcgen::KeyPair::try_from(pkcs8.as_bytes()).unwrap();
        rcgen::CertificateParams::new(vec!["idp.example.com".into()])
            .unwrap()
            .self_signed(&key_pair)
            .unwrap()
            .pem()
    })
}

fn instant(value: chrono::DateTime<chrono::Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Build a signed SAML response the way the IdP would (the same shape the
/// instance-wide SAML tests use, pointed at the tenant ACS).
fn build_saml_response(in_response_to: &str, email: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use rsa::pkcs1v15::SigningKey;
    use rsa::sha2::Sha256;
    use rsa::signature::{SignatureEncoding, Signer};
    use sha2::Digest;

    let now = chrono::Utc::now();
    let assertion_id = format!("_a{}", camelmailer_core::auth::generate_auth_token());
    let assertion_open = format!(
        "<saml:Assertion xmlns:saml=\"urn:oasis:names:tc:SAML:2.0:assertion\" ID=\"{assertion_id}\" IssueInstant=\"{}\" Version=\"2.0\">",
        instant(now),
    );
    let issuer = "<saml:Issuer>https://idp.example.com</saml:Issuer>";
    let body = format!(
        "<saml:Subject>\
         <saml:NameID Format=\"urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress\">{email}</saml:NameID>\
         <saml:SubjectConfirmation Method=\"urn:oasis:names:tc:SAML:2.0:cm:bearer\">\
         <saml:SubjectConfirmationData InResponseTo=\"{irt}\" NotOnOrAfter=\"{nooa}\" Recipient=\"{acs}\"></saml:SubjectConfirmationData>\
         </saml:SubjectConfirmation>\
         </saml:Subject>\
         <saml:Conditions NotBefore=\"{nb}\" NotOnOrAfter=\"{nooa}\">\
         <saml:AudienceRestriction><saml:Audience>{audience}</saml:Audience></saml:AudienceRestriction>\
         </saml:Conditions>\
         <saml:AttributeStatement>\
         <saml:Attribute Name=\"givenName\"><saml:AttributeValue>Ada</saml:AttributeValue></saml:Attribute>\
         <saml:Attribute Name=\"sn\"><saml:AttributeValue>Lovelace</saml:AttributeValue></saml:Attribute>\
         </saml:AttributeStatement>",
        irt = in_response_to,
        nb = instant(now - chrono::Duration::minutes(5)),
        nooa = instant(now + chrono::Duration::minutes(5)),
        acs = ORG_ACS,
        audience = ORG_AUDIENCE,
    );
    let unsigned_assertion = format!("{assertion_open}{issuer}{body}</saml:Assertion>");
    let digest = STANDARD.encode(sha2::Sha256::digest(unsigned_assertion.as_bytes()));
    let signed_info = format!(
        "<ds:SignedInfo xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\">\
         <ds:CanonicalizationMethod Algorithm=\"http://www.w3.org/2001/10/xml-exc-c14n#\"></ds:CanonicalizationMethod>\
         <ds:SignatureMethod Algorithm=\"http://www.w3.org/2001/04/xmldsig-more#rsa-sha256\"></ds:SignatureMethod>\
         <ds:Reference URI=\"#{assertion_id}\">\
         <ds:Transforms>\
         <ds:Transform Algorithm=\"http://www.w3.org/2000/09/xmldsig#enveloped-signature\"></ds:Transform>\
         <ds:Transform Algorithm=\"http://www.w3.org/2001/10/xml-exc-c14n#\"></ds:Transform>\
         </ds:Transforms>\
         <ds:DigestMethod Algorithm=\"http://www.w3.org/2001/04/xmlenc#sha256\"></ds:DigestMethod>\
         <ds:DigestValue>{digest}</ds:DigestValue>\
         </ds:Reference>\
         </ds:SignedInfo>",
    );
    let signing_key = SigningKey::<Sha256>::new(idp_key().clone());
    let signature = STANDARD.encode(signing_key.sign(signed_info.as_bytes()).to_bytes());
    let signature_element = format!(
        "<ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\">{signed_info}\
         <ds:SignatureValue>{signature}</ds:SignatureValue></ds:Signature>"
    );
    let assertion = format!("{assertion_open}{issuer}{signature_element}{body}</saml:Assertion>");
    format!(
        "<samlp:Response xmlns:samlp=\"urn:oasis:names:tc:SAML:2.0:protocol\" \
         Destination=\"{ORG_ACS}\" ID=\"_r{irt}\" InResponseTo=\"{irt}\" \
         IssueInstant=\"{}\" Version=\"2.0\">\
         <samlp:Status><samlp:StatusCode Value=\"urn:oasis:names:tc:SAML:2.0:status:Success\"></samlp:StatusCode></samlp:Status>\
         {assertion}\
         </samlp:Response>",
        instant(now),
        irt = in_response_to,
    )
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn percent_decode(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() + 1 => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    out
}

async fn saml_harness() -> Harness {
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
            kind: SsoKind::Saml,
            name: "Acme Okta SAML".into(),
            enabled: true,
            config: json!({
                "idp_sso_url": "https://idp.example.com/sso",
                "idp_certificate": idp_certificate_pem(),
            }),
            default_role: Role::Member,
            auto_provision: true,
        })
        .await
        .unwrap();
    let config = camelmailer_config::Config::default();
    let state = ApiState::full(store.clone(), None, Some(store.clone()), None, config)
        .with_org_sso_store(store.clone());
    Harness {
        app: build_org_sso_login_router(state),
        store,
        org_id: org.id,
    }
}

async fn post_org_acs(app: &Router, response_xml: &str, relay_state: &str) -> (StatusCode, Value) {
    use base64::engine::general_purpose::STANDARD;
    let body = format!(
        "SAMLResponse={}&RelayState={relay_state}",
        percent_encode(&STANDARD.encode(response_xml))
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v2/auth/org-sso/acs")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
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

#[tokio::test]
async fn a_saml_login_provisions_and_joins_the_org() {
    use base64::engine::general_purpose::STANDARD;
    use std::io::Read;

    let h = saml_harness().await;
    let connection_id = h.store.list_org_sso_connections(h.org_id).await.unwrap()[0].id;

    // start: the authorization URL points at the connection's IdP and
    // carries the connection id as RelayState
    let (status, body) = get_req(
        &h.app,
        &format!("/api/v2/auth/org-sso/{connection_id}/start"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let url = body["data"]["authorization_url"].as_str().unwrap();
    assert!(
        url.starts_with("https://idp.example.com/sso?SAMLRequest="),
        "{url}"
    );
    assert!(
        url.ends_with(&format!("&RelayState={connection_id}")),
        "{url}"
    );

    // pull the request id out of the deflated AuthnRequest
    let encoded = url.split("SAMLRequest=").nth(1).unwrap();
    let encoded = encoded.split('&').next().unwrap();
    let deflated = STANDARD.decode(percent_decode(encoded)).unwrap();
    let mut xml = String::new();
    flate2::read::DeflateDecoder::new(deflated.as_slice())
        .read_to_string(&mut xml)
        .unwrap();
    assert!(
        xml.contains(&format!("AssertionConsumerServiceURL=\"{ORG_ACS}\"")),
        "{xml}"
    );
    let request_id = xml
        .split("ID=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    assert!(
        request_id.starts_with(&format!("_c{connection_id}.")),
        "{request_id}"
    );

    // a correctly signed response signs the user in and joins the org
    let response_xml = build_saml_response(request_id, "ada@acme.test");
    let (status, body) = post_org_acs(&h.app, &response_xml, &connection_id.to_string()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["user"]["email_address"], "ada@acme.test");
    assert_eq!(body["data"]["user"]["first_name"], "Ada");

    let user = h
        .store
        .user_by_email("ada@acme.test")
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

    // replaying the same response is rejected (request already consumed)
    let (status, body) = post_org_acs(&h.app, &response_xml, &connection_id.to_string()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn a_saml_login_with_an_unverified_email_domain_is_rejected() {
    use base64::engine::general_purpose::STANDARD;
    use std::io::Read;

    let h = saml_harness().await;
    let connection_id = h.store.list_org_sso_connections(h.org_id).await.unwrap()[0].id;
    let (_, body) = get_req(
        &h.app,
        &format!("/api/v2/auth/org-sso/{connection_id}/start"),
        true,
    )
    .await;
    let url = body["data"]["authorization_url"].as_str().unwrap();
    let encoded = url
        .split("SAMLRequest=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let deflated = STANDARD.decode(percent_decode(encoded)).unwrap();
    let mut xml = String::new();
    flate2::read::DeflateDecoder::new(deflated.as_slice())
        .read_to_string(&mut xml)
        .unwrap();
    let request_id = xml
        .split("ID=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();

    let response_xml = build_saml_response(request_id, "mallory@evil.test");
    let (status, body) = post_org_acs(&h.app, &response_xml, &connection_id.to_string()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(h
        .store
        .user_by_email("mallory@evil.test")
        .await
        .unwrap()
        .is_none());
}
