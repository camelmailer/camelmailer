//! SAML SSO, tested against an in-test identity provider: a self-signed
//! RSA certificate and hand-built, correctly signed (or deliberately
//! broken) responses posted to the ACS endpoint.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use camelmailer_api::{build_auth_router, build_saml_router, ApiState};
use camelmailer_core::{AdminStore, AuthStore, MemoryStore, NewUser};
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use rsa::pkcs8::EncodePrivateKey;
use rsa::RsaPrivateKey;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;

const ACS: &str = "https://postal.example.com/api/v2/auth/saml/acs";
const AUDIENCE: &str = "https://postal.example.com";

/// One RSA key for the whole test binary (keygen is slow in debug builds).
fn idp_key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap())
}

/// A different key whose signatures must be rejected.
fn foreign_key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap())
}

/// Self-signed X.509 certificate for `idp_key()`, as configured PEM.
fn idp_certificate_pem() -> &'static str {
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

fn instant(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

struct ResponseOptions {
    in_response_to: String,
    assertion_id: String,
    email: String,
    audience: String,
    not_before: DateTime<Utc>,
    not_on_or_after: DateTime<Utc>,
    signed: bool,
    signer: &'static RsaPrivateKey,
}

impl ResponseOptions {
    fn new(in_response_to: &str, email: &str) -> Self {
        Self {
            in_response_to: in_response_to.to_string(),
            assertion_id: format!("_a{}", camelmailer_core::auth::generate_auth_token()),
            email: email.to_string(),
            audience: AUDIENCE.into(),
            not_before: Utc::now() - Duration::minutes(5),
            not_on_or_after: Utc::now() + Duration::minutes(5),
            signed: true,
            signer: idp_key(),
        }
    }
}

/// Build a SAML response the way an IdP would: the assertion in
/// canonical form, digested, and carrying an enveloped RSA-SHA256
/// signature.
fn build_response(options: &ResponseOptions) -> String {
    let assertion_open = format!(
        "<saml:Assertion xmlns:saml=\"urn:oasis:names:tc:SAML:2.0:assertion\" ID=\"{id}\" IssueInstant=\"{now}\" Version=\"2.0\">",
        id = options.assertion_id,
        now = instant(Utc::now()),
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
        email = options.email,
        irt = options.in_response_to,
        nb = instant(options.not_before),
        nooa = instant(options.not_on_or_after),
        acs = ACS,
        audience = options.audience,
    );
    let unsigned_assertion = format!("{assertion_open}{issuer}{body}</saml:Assertion>");

    let assertion = if options.signed {
        // Digest over the canonical assertion *without* the signature —
        // exactly what the verifier reconstructs after the
        // enveloped-signature transform.
        let digest = STANDARD.encode(Sha256::digest(unsigned_assertion.as_bytes()));
        let signed_info = format!(
            "<ds:SignedInfo xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\">\
             <ds:CanonicalizationMethod Algorithm=\"http://www.w3.org/2001/10/xml-exc-c14n#\"></ds:CanonicalizationMethod>\
             <ds:SignatureMethod Algorithm=\"http://www.w3.org/2001/04/xmldsig-more#rsa-sha256\"></ds:SignatureMethod>\
             <ds:Reference URI=\"#{id}\">\
             <ds:Transforms>\
             <ds:Transform Algorithm=\"http://www.w3.org/2000/09/xmldsig#enveloped-signature\"></ds:Transform>\
             <ds:Transform Algorithm=\"http://www.w3.org/2001/10/xml-exc-c14n#\"></ds:Transform>\
             </ds:Transforms>\
             <ds:DigestMethod Algorithm=\"http://www.w3.org/2001/04/xmlenc#sha256\"></ds:DigestMethod>\
             <ds:DigestValue>{digest}</ds:DigestValue>\
             </ds:Reference>\
             </ds:SignedInfo>",
            id = options.assertion_id,
        );
        use rsa::pkcs1v15::SigningKey;
        use rsa::signature::{SignatureEncoding, Signer};
        let signing_key = SigningKey::<Sha256>::new(options.signer.clone());
        let signature = STANDARD.encode(signing_key.sign(signed_info.as_bytes()).to_bytes());
        let signature_element = format!(
            "<ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\">{signed_info}\
             <ds:SignatureValue>{signature}</ds:SignatureValue></ds:Signature>"
        );
        format!("{assertion_open}{issuer}{signature_element}{body}</saml:Assertion>")
    } else {
        unsigned_assertion
    };

    format!(
        "<samlp:Response xmlns:samlp=\"urn:oasis:names:tc:SAML:2.0:protocol\" \
         Destination=\"{acs}\" ID=\"_r{irt}\" InResponseTo=\"{irt}\" \
         IssueInstant=\"{now}\" Version=\"2.0\">\
         <samlp:Status><samlp:StatusCode Value=\"urn:oasis:names:tc:SAML:2.0:status:Success\"></samlp:StatusCode></samlp:Status>\
         {assertion}\
         </samlp:Response>",
        acs = ACS,
        irt = options.in_response_to,
        now = instant(Utc::now()),
    )
}

struct Harness {
    app: Router,
    store: Arc<MemoryStore>,
}

async fn harness(mutate: impl FnOnce(&mut camelmailer_config::Config)) -> Harness {
    let mut config = camelmailer_config::Config::default();
    config.saml.enabled = true;
    config.saml.name = "Okta".into();
    config.saml.idp_sso_url = "https://idp.example.com/sso".into();
    config.saml.idp_certificate = Some(idp_certificate_pem().to_string());
    mutate(&mut config);
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::full(store.clone(), None, Some(store.clone()), None, config);
    let app = build_saml_router(state.clone()).merge(build_auth_router(state));
    Harness { app, store }
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
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    out
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

    /// Run /start and return the AuthnRequest id the ACS must see as
    /// `InResponseTo`, extracted from the redirect's SAMLRequest.
    async fn start_login(&self) -> String {
        let (status, _, location) = self.get("/api/v2/auth/saml/start", false).await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        let location = location.unwrap();
        assert!(
            location.starts_with("https://idp.example.com/sso?SAMLRequest="),
            "{location}"
        );
        let encoded = location.split("SAMLRequest=").nth(1).unwrap();
        let encoded = encoded.split('&').next().unwrap();
        let deflated = STANDARD
            .decode(percent_decode(encoded))
            .expect("SAMLRequest is base64");
        let mut xml = String::new();
        flate2::read::DeflateDecoder::new(deflated.as_slice())
            .read_to_string(&mut xml)
            .expect("SAMLRequest is raw deflate");
        assert!(xml.starts_with("<samlp:AuthnRequest"), "{xml}");
        assert!(xml.contains(&format!("AssertionConsumerServiceURL=\"{ACS}\"")));
        let id = xml.split("ID=\"").nth(1).unwrap();
        id.split('"').next().unwrap().to_string()
    }

    async fn post_acs(&self, response_xml: &str) -> (StatusCode, Value, Option<String>) {
        let body = format!(
            "SAMLResponse={}",
            percent_encode(&STANDARD.encode(response_xml))
        );
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v2/auth/saml/acs")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
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

    /// A complete, correctly signed login for `email`.
    async fn sso_login(&self, email: &str) -> (StatusCode, Value) {
        let request_id = self.start_login().await;
        let xml = build_response(&ResponseOptions::new(&request_id, email));
        let (status, body, _) = self.post_acs(&xml).await;
        (status, body)
    }
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

// ------------------------------------------------------------- metadata

#[tokio::test]
async fn metadata_describes_the_service_provider() {
    let h = harness(|_| {}).await;
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v2/auth/saml/metadata")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/samlmetadata+xml"
    );
    let xml = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(xml.contains(&format!("entityID=\"{AUDIENCE}\"")), "{xml}");
    assert!(xml.contains(&format!("Location=\"{ACS}\"")), "{xml}");
    assert!(xml.contains("urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST"));
}

// ---------------------------------------------------------------- start

#[tokio::test]
async fn start_redirects_with_a_deflated_authn_request() {
    let h = harness(|_| {}).await;
    let request_id = h.start_login().await;
    assert!(request_id.starts_with('_'), "{request_id}");

    // SPA-style: JSON carrying the URL and the configured button label
    let (status, body, _) = h.get("/api/v2/auth/saml/start", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["authorization_url"]
        .as_str()
        .unwrap()
        .contains("SAMLRequest="));
    assert_eq!(body["data"]["name"], "Okta");
}

// ------------------------------------------------------------ the flow

#[tokio::test]
async fn a_full_login_provisions_the_account_and_issues_a_session() {
    let h = harness(|_| {}).await;
    let (status, body) = h.sso_login("ada@corp.example").await;
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

    // a second SSO login maps onto the same account (by email)
    let (status, _) = h.sso_login("ada@corp.example").await;
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
    assert!(events.contains(&"saml.provision".to_string()));
    assert!(events.iter().filter(|event| *event == "saml.login").count() >= 2);
}

#[tokio::test]
async fn a_configured_frontend_receives_the_token_in_the_fragment() {
    let h = harness(|config| {
        config.auth.frontend_url = Some("https://mail.corp.example".into());
    })
    .await;
    let request_id = h.start_login().await;
    let xml = build_response(&ResponseOptions::new(&request_id, "ada@corp.example"));
    let (status, _, location) = h.post_acs(&xml).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert!(location
        .unwrap()
        .starts_with("https://mail.corp.example/auth/callback#session_token="));
}

// ------------------------------------------------------------ rejection

#[tokio::test]
async fn an_unsigned_response_is_rejected() {
    let h = harness(|_| {}).await;
    let request_id = h.start_login().await;
    let mut options = ResponseOptions::new(&request_id, "ada@corp.example");
    options.signed = false;
    let (status, body, _) = h.post_acs(&build_response(&options)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "SSOError");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not signed"),
        "{body}"
    );
    assert_eq!(h.store.list_users().await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_tampered_response_fails_the_digest() {
    let h = harness(|_| {}).await;
    let request_id = h.start_login().await;
    let xml = build_response(&ResponseOptions::new(&request_id, "ada@corp.example"))
        .replace("ada@corp.example", "eve@corp.example");
    let (status, body, _) = h.post_acs(&xml).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("digest mismatch"),
        "{body}"
    );
    assert_eq!(h.store.list_users().await.unwrap().len(), 0);
}

#[tokio::test]
async fn a_response_signed_with_a_foreign_key_is_rejected() {
    let h = harness(|_| {}).await;
    let request_id = h.start_login().await;
    let mut options = ResponseOptions::new(&request_id, "ada@corp.example");
    options.signer = foreign_key();
    let (status, body, _) = h.post_acs(&build_response(&options)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("signature verification failed"),
        "{body}"
    );
}

#[tokio::test]
async fn expired_conditions_are_rejected() {
    let h = harness(|_| {}).await;
    let request_id = h.start_login().await;
    let mut options = ResponseOptions::new(&request_id, "ada@corp.example");
    options.not_before = Utc::now() - Duration::minutes(30);
    options.not_on_or_after = Utc::now() - Duration::minutes(10);
    let (status, body, _) = h.post_acs(&build_response(&options)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("expired"),
        "{body}"
    );
}

#[tokio::test]
async fn a_not_yet_valid_assertion_is_rejected() {
    let h = harness(|_| {}).await;
    let request_id = h.start_login().await;
    let mut options = ResponseOptions::new(&request_id, "ada@corp.example");
    options.not_before = Utc::now() + Duration::minutes(10);
    options.not_on_or_after = Utc::now() + Duration::minutes(20);
    let (status, body, _) = h.post_acs(&build_response(&options)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not yet valid"),
        "{body}"
    );
}

#[tokio::test]
async fn a_wrong_audience_is_rejected() {
    let h = harness(|_| {}).await;
    let request_id = h.start_login().await;
    let mut options = ResponseOptions::new(&request_id, "ada@corp.example");
    options.audience = "https://some-other-sp.example".into();
    let (status, body, _) = h.post_acs(&build_response(&options)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("audience"),
        "{body}"
    );
}

#[tokio::test]
async fn an_unknown_in_response_to_is_rejected() {
    let h = harness(|_| {}).await;
    // no /start — the response answers a request we never made
    let xml = build_response(&ResponseOptions::new("_forged-request", "ada@corp.example"));
    let (status, body, _) = h.post_acs(&xml).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("login request"),
        "{body}"
    );
}

#[tokio::test]
async fn replaying_an_assertion_is_rejected() {
    let h = harness(|_| {}).await;

    // First login succeeds.
    let request_id = h.start_login().await;
    let mut options = ResponseOptions::new(&request_id, "ada@corp.example");
    let (status, _, _) = h.post_acs(&build_response(&options)).await;
    assert_eq!(status, StatusCode::CREATED);

    // Same assertion id inside a fresh, correctly signed response for a
    // *new* request: the replay cache must still reject it.
    let second_request = h.start_login().await;
    options.in_response_to = second_request;
    let (status, body, _) = h.post_acs(&build_response(&options)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already been used"),
        "{body}"
    );

    // And simply re-posting the identical response fails on the consumed
    // request state.
    let request_id = h.start_login().await;
    let options = ResponseOptions::new(&request_id, "ada@corp.example");
    let xml = build_response(&options);
    let (status, _, _) = h.post_acs(&xml).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, _) = h.post_acs(&xml).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// -------------------------------------------------- provisioning policy

#[tokio::test]
async fn provisioning_can_be_disabled() {
    let h = harness(|config| {
        config.saml.auto_provision = false;
    })
    .await;

    // unknown user → rejected
    let (status, body) = h.sso_login("unknown@corp.example").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("provisioning is disabled"),
        "{body}"
    );

    // existing user → signs in
    h.store
        .create_user(NewUser {
            email_address: "existing@corp.example".into(),
            first_name: "Existing".into(),
            last_name: "Person".into(),
            admin: false,
        })
        .await
        .unwrap();
    let (status, body) = h.sso_login("existing@corp.example").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(h.store.list_users().await.unwrap().len(), 1);
}

#[tokio::test]
async fn email_domains_can_be_restricted() {
    let h = harness(|config| {
        config.saml.allowed_email_domains = vec!["corp.example".into()];
    })
    .await;
    let (status, body) = h.sso_login("ada@evil.example").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("domain"));
    assert_eq!(h.store.list_users().await.unwrap().len(), 0);

    let (status, _) = h.sso_login("ada@corp.example").await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn a_deactivated_account_cannot_sign_in_via_saml() {
    let h = harness(|_| {}).await;
    let user = h
        .store
        .create_user(NewUser {
            email_address: "ada@corp.example".into(),
            first_name: "Ada".into(),
            last_name: "L".into(),
            admin: false,
        })
        .await
        .unwrap();
    h.store.set_user_disabled(user.id, true).await.unwrap();

    let (status, body) = h.sso_login("ada@corp.example").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "AccountDisabled");
}

// ------------------------------------------------------------- disabled

#[tokio::test]
async fn saml_endpoints_404_when_disabled() {
    let h = harness(|config| {
        config.saml.enabled = false;
    })
    .await;
    for path in ["/api/v2/auth/saml/start", "/api/v2/auth/saml/metadata"] {
        let (status, body, _) = h.get(path, true).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body["error"]["code"], "SAMLDisabled", "{path}");
    }
    let (status, body, _) = h.post_acs("<xml/>").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "SAMLDisabled");
}
