//! SAML 2.0 single sign-on, service-provider role
//! (`/api/v2/auth/saml/*`) — the enterprise counterpart to
//! [`crate::oidc`].
//!
//! Bindings: HTTP-Redirect for the `AuthnRequest` (deflate + base64 in
//! the `SAMLRequest` query parameter), HTTP-POST for the response at the
//! assertion consumer service (ACS). Every response is validated end to
//! end before a session is issued:
//!
//! - the XML signature (assertion- or response-level) against the
//!   **configured** IdP certificate — unsigned or foreign-signed
//!   responses are rejected, `ds:KeyInfo` in the message is ignored
//! - `Status` must be success, exactly one (unencrypted) assertion
//! - `Audience` must equal the SP entity id, `Destination` (when
//!   present) must be the ACS URL
//! - `InResponseTo` must redeem a stored, unexpired AuthnRequest id
//!   (single use — no IdP-initiated logins)
//! - `Conditions` `NotBefore`/`NotOnOrAfter` (±90 s clock skew) and the
//!   bearer `SubjectConfirmationData` window
//! - the assertion id goes into a replay cache until its own
//!   `NotOnOrAfter`; a second presentation is rejected
//!
//! Accounts resolve by email (NameID in emailAddress format, or an
//! email attribute): existing account → sign in; unknown →
//! provisioned when `saml.auto_provision` allows it and the domain
//! passes `saml.allowed_email_domains`.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use camelmailer_core::auth::{self, NewAuthEvent};
use camelmailer_core::{AuthStore, StoreError};
use chrono::{DateTime, Duration, Utc};
use flate2::write::DeflateEncoder;
use flate2::Compression;
use roxmltree::{Document, Node};
use serde::Deserialize;
use serde_json::json;
use std::io::Write;
use std::sync::Arc;

use crate::app::{
    render_error, render_success, timing_middleware, ApiResponse, ApiState, RequestStart,
};
use crate::auth_api::{client_ip, issue_session, user_json};
use crate::xmldsig;

const NS_PROTOCOL: &str = "urn:oasis:names:tc:SAML:2.0:protocol";
const NS_ASSERTION: &str = "urn:oasis:names:tc:SAML:2.0:assertion";
const STATUS_SUCCESS: &str = "urn:oasis:names:tc:SAML:2.0:status:Success";
const METHOD_BEARER: &str = "urn:oasis:names:tc:SAML:2.0:cm:bearer";
const NAMEID_EMAIL: &str = "urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress";
const REQUEST_TTL_MINUTES: i64 = 10;
/// Tolerated clock skew between the IdP and us.
const CLOCK_SKEW_SECONDS: i64 = 90;

fn sso_error(start: Option<&RequestStart>, message: &str) -> ApiResponse {
    render_error(start, StatusCode::UNPROCESSABLE_ENTITY, "SSOError", message)
}

fn disabled(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::NOT_FOUND,
        "SAMLDisabled",
        "SAML single sign-on is not enabled on this instance",
    )
}

/// The assertion consumer service URL registered with the IdP.
fn acs_url(state: &ApiState) -> String {
    format!(
        "{}://{}/api/v2/auth/saml/acs",
        state.config.camelmailer.web_protocol, state.config.camelmailer.web_hostname
    )
}

fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn urlencode(value: &str) -> String {
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

/// Load the configured IdP certificate (inline PEM or a file path) and
/// extract its RSA public key.
fn idp_public_key(state: &ApiState) -> Result<rsa::RsaPublicKey, String> {
    let configured = state
        .config
        .saml
        .idp_certificate
        .as_deref()
        .unwrap_or_default();
    let pem = if configured.contains("-----BEGIN") {
        configured.to_string()
    } else {
        std::fs::read_to_string(configured).map_err(|error| {
            format!("could not read saml.idp_certificate {configured:?}: {error}")
        })?
    };
    xmldsig::public_key_from_certificate_pem(&pem)
}

// ----------------------------------------------------------- /metadata

/// `GET /api/v2/auth/saml/metadata` — SP metadata for IdP registration.
async fn saml_metadata(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
) -> Response {
    if !state.config.saml.enabled {
        return disabled(Some(&start.0)).into_response();
    }
    let metadata = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <md:EntityDescriptor xmlns:md=\"urn:oasis:names:tc:SAML:2.0:metadata\" entityID=\"{entity}\">\
         <md:SPSSODescriptor AuthnRequestsSigned=\"false\" WantAssertionsSigned=\"true\" protocolSupportEnumeration=\"urn:oasis:names:tc:SAML:2.0:protocol\">\
         <md:NameIDFormat>{nameid}</md:NameIDFormat>\
         <md:AssertionConsumerService Binding=\"urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST\" Location=\"{acs}\" index=\"0\" isDefault=\"true\"/>\
         </md:SPSSODescriptor>\
         </md:EntityDescriptor>",
        entity = xml_escape(&state.config.saml_sp_entity_id()),
        nameid = NAMEID_EMAIL,
        acs = xml_escape(&acs_url(&state)),
    );
    (
        StatusCode::OK,
        [("content-type", "application/samlmetadata+xml")],
        metadata,
    )
        .into_response()
}

// -------------------------------------------------------------- /start

/// `GET /api/v2/auth/saml/start` — begin the login. Responds with a
/// redirect to the IdP SSO URL carrying the deflated `SAMLRequest` (or
/// the URL as JSON when requested with `Accept: application/json`).
async fn saml_start(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !state.config.saml.enabled {
        return disabled(Some(&start.0)).into_response();
    }
    let Some(store) = state.auth_store.clone() else {
        return sso_error(Some(&start.0), "accounts require persistent storage").into_response();
    };

    let request_id = format!("_{}", auth::generate_auth_token());
    if let Err(error) = store
        .create_saml_request(
            &request_id,
            Utc::now() + Duration::minutes(REQUEST_TTL_MINUTES),
        )
        .await
    {
        return crate::app::render_store_error(Some(&start.0), error).into_response();
    }

    let authn_request = format!(
        "<samlp:AuthnRequest xmlns:samlp=\"{NS_PROTOCOL}\" xmlns:saml=\"{NS_ASSERTION}\" \
         ID=\"{id}\" Version=\"2.0\" IssueInstant=\"{instant}\" Destination=\"{sso}\" \
         AssertionConsumerServiceURL=\"{acs}\" \
         ProtocolBinding=\"urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST\">\
         <saml:Issuer>{issuer}</saml:Issuer>\
         </samlp:AuthnRequest>",
        id = request_id,
        instant = Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        sso = xml_escape(&state.config.saml.idp_sso_url),
        acs = xml_escape(&acs_url(&state)),
        issuer = xml_escape(&state.config.saml_sp_entity_id()),
    );

    // HTTP-Redirect binding: raw deflate, then base64, then URL-encode.
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    let deflated = encoder
        .write_all(authn_request.as_bytes())
        .and_then(|()| encoder.finish());
    let deflated = match deflated {
        Ok(bytes) => bytes,
        Err(error) => {
            return sso_error(
                Some(&start.0),
                &format!("could not encode the request: {error}"),
            )
            .into_response()
        }
    };
    let sso_url = &state.config.saml.idp_sso_url;
    let separator = if sso_url.contains('?') { '&' } else { '?' };
    let url = format!(
        "{sso_url}{separator}SAMLRequest={}",
        urlencode(&STANDARD.encode(deflated))
    );

    let wants_json = headers
        .get("accept")
        .and_then(|value| value.to_str().ok())
        .map(|accept| accept.contains("application/json"))
        .unwrap_or(false);
    if wants_json {
        render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "authorization_url": url, "name": state.config.saml.name }),
        )
        .into_response()
    } else {
        Redirect::temporary(&url).into_response()
    }
}

// ---------------------------------------------------------------- /acs

#[derive(Debug, Deserialize)]
struct AcsForm {
    #[serde(rename = "SAMLResponse")]
    saml_response: Option<String>,
    #[serde(rename = "RelayState")]
    #[allow(dead_code)]
    relay_state: Option<String>,
}

/// Everything extracted from a fully validated SAML response.
struct ValidatedAssertion {
    assertion_id: String,
    in_response_to: String,
    name_id: Option<String>,
    name_id_format: Option<String>,
    /// `(name-or-friendly-name, first value)` pairs, lowercased names.
    attributes: Vec<(String, String)>,
    /// The assertion's own end of validity — the replay-cache lifetime.
    not_on_or_after: DateTime<Utc>,
}

/// `POST /api/v2/auth/saml/acs` — the assertion consumer service.
async fn saml_acs(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Form(form): Form<AcsForm>,
) -> Response {
    if !state.config.saml.enabled {
        return disabled(Some(&start.0)).into_response();
    }
    let Some(store) = state.auth_store.clone() else {
        return sso_error(Some(&start.0), "accounts require persistent storage").into_response();
    };
    let Some(encoded) = form.saml_response.filter(|value| !value.is_empty()) else {
        return sso_error(Some(&start.0), "missing SAMLResponse parameter").into_response();
    };
    let xml = match xmldsig::decode_base64(&encoded).map(String::from_utf8) {
        Ok(Ok(xml)) => xml,
        _ => {
            return sso_error(Some(&start.0), "SAMLResponse is not valid base64 XML")
                .into_response()
        }
    };
    let public_key = match idp_public_key(&state) {
        Ok(key) => key,
        Err(message) => return sso_error(Some(&start.0), &message).into_response(),
    };

    let now = Utc::now();
    let validated = match validate_response(
        &xml,
        &public_key,
        &state.config.saml_sp_entity_id(),
        &acs_url(&state),
        now,
    ) {
        Ok(validated) => validated,
        Err(message) => {
            tracing::warn!(%message, "rejected SAML response");
            return sso_error(Some(&start.0), &message).into_response();
        }
    };

    // The response must answer one of *our* outstanding requests …
    match store
        .consume_saml_request(&validated.in_response_to, now)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return sso_error(
                Some(&start.0),
                "the login request is unknown, already used, or has expired",
            )
            .into_response()
        }
        Err(error) => return crate::app::render_store_error(Some(&start.0), error).into_response(),
    }
    // … and every assertion may be presented exactly once.
    match store
        .register_saml_assertion(&validated.assertion_id, validated.not_on_or_after, now)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return sso_error(Some(&start.0), "this assertion has already been used")
                .into_response()
        }
        Err(error) => return crate::app::render_store_error(Some(&start.0), error).into_response(),
    }

    let Some(email) = extract_email(&validated) else {
        return sso_error(
            Some(&start.0),
            "the assertion carries no email address (NameID or email attribute)",
        )
        .into_response();
    };

    let user = match resolve_user(&state, &store, &email, &validated).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    let _ = store
        .record_auth_event(NewAuthEvent {
            user_id: Some(user.id),
            email_address: Some(user.email_address.clone()),
            event: "saml.login".into(),
            ip_address: client_ip(&headers),
            user_agent: None,
        })
        .await;

    match issue_session(&store, &state, &user, &headers).await {
        Ok((token, session)) => {
            if let Some(base) = state.config.auth.frontend_url.as_deref() {
                let url = format!(
                    "{}/auth/callback#session_token={}",
                    base.trim_end_matches('/'),
                    token
                );
                return Redirect::temporary(&url).into_response();
            }
            render_success(
                Some(&start.0),
                StatusCode::CREATED,
                json!({
                    "session_token": token,
                    "expires_at": session.expires_at,
                    "user": user_json(&user),
                }),
            )
            .into_response()
        }
        Err(error) => crate::app::render_store_error(Some(&start.0), error).into_response(),
    }
}

/// Email resolution: NameID in emailAddress format first, then the
/// common email attributes, then a NameID that merely looks like one.
fn extract_email(validated: &ValidatedAssertion) -> Option<String> {
    if validated.name_id_format.as_deref() == Some(NAMEID_EMAIL) {
        if let Some(name_id) = validated.name_id.as_deref().filter(|v| v.contains('@')) {
            return Some(name_id.to_lowercase());
        }
    }
    if let Some(value) = attribute_value(
        &validated.attributes,
        &[
            "email",
            "emailaddress",
            "mail",
            "urn:oid:0.9.2342.19200300.100.1.3",
        ],
    ) {
        if value.contains('@') {
            return Some(value.to_lowercase());
        }
    }
    validated
        .name_id
        .as_deref()
        .filter(|value| value.contains('@'))
        .map(str::to_lowercase)
}

/// Look an attribute up by its short name (the part after the last `/`
/// or `:` of the SAML attribute name, lowercased) or its full name.
fn attribute_value(attributes: &[(String, String)], candidates: &[&str]) -> Option<String> {
    attributes
        .iter()
        .find(|(name, value)| {
            !value.is_empty()
                && (candidates.contains(&name.as_str())
                    || candidates.contains(&short_name(name).as_ref()))
        })
        .map(|(_, value)| value.clone())
}

fn short_name(name: &str) -> String {
    name.rsplit(['/', ':']).next().unwrap_or(name).to_string()
}

/// The user's name from the usual attributes, with a display-name
/// fallback.
fn extract_names(validated: &ValidatedAssertion) -> (String, String) {
    let first = attribute_value(
        &validated.attributes,
        &["givenname", "firstname", "urn:oid:2.5.4.42"],
    );
    let last = attribute_value(
        &validated.attributes,
        &["sn", "surname", "lastname", "urn:oid:2.5.4.4"],
    );
    if first.is_some() || last.is_some() {
        return (first.unwrap_or_default(), last.unwrap_or_default());
    }
    let display =
        attribute_value(&validated.attributes, &["displayname", "cn", "name"]).unwrap_or_default();
    match display.split_once(' ') {
        Some((first, last)) => (first.to_string(), last.to_string()),
        None => (display, String::new()),
    }
}

/// Find or provision the account for a validated assertion.
async fn resolve_user(
    state: &Arc<ApiState>,
    store: &Arc<dyn AuthStore>,
    email: &str,
    validated: &ValidatedAssertion,
) -> Result<camelmailer_core::User, ApiResponse> {
    let map_store_error = |error: StoreError| crate::app::render_store_error(None, error);
    let saml = &state.config.saml;

    if !saml.allowed_email_domains.is_empty() {
        let domain = email.rsplit('@').next().unwrap_or("");
        if !saml
            .allowed_email_domains
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(domain))
        {
            return Err(sso_error(
                None,
                "this email domain is not allowed to sign in via SSO",
            ));
        }
    }

    if let Some(user) = store.user_by_email(email).await.map_err(map_store_error)? {
        let user_auth = store.user_auth(user.id).await.map_err(map_store_error)?;
        if user_auth.map(|auth| auth.disabled).unwrap_or(false) {
            return Err(render_error(
                None,
                StatusCode::FORBIDDEN,
                "AccountDisabled",
                "This account has been deactivated",
            ));
        }
        return Ok(user);
    }

    if !saml.auto_provision {
        return Err(sso_error(
            None,
            "no account exists for this identity and provisioning is disabled",
        ));
    }
    let (first_name, last_name) = extract_names(validated);
    let user = state
        .store
        .create_user(camelmailer_core::NewUser {
            email_address: email.to_string(),
            first_name,
            last_name,
            admin: false,
        })
        .await
        .map_err(map_store_error)?;
    let _ = store
        .record_auth_event(NewAuthEvent {
            user_id: Some(user.id),
            email_address: Some(user.email_address.clone()),
            event: "saml.provision".into(),
            ip_address: None,
            user_agent: None,
        })
        .await;
    // Starter workspace (auth.bootstrap_workspace) — org + servers only:
    // SSO provisioning has no response channel that could show an API
    // key exactly once, so no credential is created here.
    let _ = crate::workspace::bootstrap_workspace(state, &user, false).await;
    Ok(user)
}

// ---------------------------------------------------------- validation

fn find_child<'a, 'input>(
    parent: Node<'a, 'input>,
    namespace: &str,
    name: &str,
) -> Option<Node<'a, 'input>> {
    parent.children().find(|child| {
        child.is_element()
            && child.tag_name().namespace() == Some(namespace)
            && child.tag_name().name() == name
    })
}

fn parse_instant(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| format!("invalid timestamp {value:?} in the assertion"))
}

/// Validate a decoded SAML response end to end (see the module docs for
/// the full checklist) and extract the identity it asserts.
fn validate_response(
    xml: &str,
    public_key: &rsa::RsaPublicKey,
    sp_entity_id: &str,
    acs: &str,
    now: DateTime<Utc>,
) -> Result<ValidatedAssertion, String> {
    let document =
        Document::parse(xml).map_err(|error| format!("invalid response XML: {error}"))?;
    let response = document.root_element();
    if response.tag_name().namespace() != Some(NS_PROTOCOL)
        || response.tag_name().name() != "Response"
    {
        return Err("the document is not a SAML response".into());
    }
    if let Some(destination) = response.attribute("Destination") {
        if destination != acs {
            return Err("the response Destination does not match our ACS URL".into());
        }
    }
    let in_response_to = response
        .attribute("InResponseTo")
        .filter(|value| !value.is_empty())
        .ok_or("the response carries no InResponseTo (IdP-initiated logins are not supported)")?
        .to_string();

    // Status must be success.
    let status_code = find_child(response, NS_PROTOCOL, "Status")
        .and_then(|status| find_child(status, NS_PROTOCOL, "StatusCode"))
        .and_then(|code| code.attribute("Value"))
        .unwrap_or("");
    if status_code != STATUS_SUCCESS {
        return Err(format!(
            "the identity provider reported a non-success status {status_code:?}"
        ));
    }

    if document.descendants().any(|node| {
        node.is_element()
            && node.tag_name().namespace() == Some(NS_ASSERTION)
            && node.tag_name().name() == "EncryptedAssertion"
    }) {
        return Err("encrypted assertions are not supported".into());
    }
    let assertions: Vec<Node> = document
        .descendants()
        .filter(|node| {
            node.is_element()
                && node.tag_name().namespace() == Some(NS_ASSERTION)
                && node.tag_name().name() == "Assertion"
        })
        .collect();
    let [assertion] = assertions[..] else {
        return Err("the response must contain exactly one assertion".into());
    };
    if assertion.attribute("Version") != Some("2.0") {
        return Err("only SAML 2.0 assertions are supported".into());
    }

    // Signature: on the assertion itself, or on the response covering
    // the whole document. Never optional.
    if xmldsig::direct_signature(assertion).is_some() {
        xmldsig::verify_enveloped_signature(&document, assertion, public_key)
            .map_err(|error| format!("assertion signature invalid: {error}"))?;
    } else if xmldsig::direct_signature(response).is_some() {
        xmldsig::verify_enveloped_signature(&document, response, public_key)
            .map_err(|error| format!("response signature invalid: {error}"))?;
        // The verified reference is the document root, so the assertion
        // inside it is covered by the signature.
    } else {
        return Err("the response is not signed — unsigned assertions are rejected".into());
    }

    let assertion_id = assertion
        .attribute("ID")
        .filter(|value| !value.is_empty())
        .ok_or("the assertion has no ID")?
        .to_string();

    // Conditions: validity window and audience restriction.
    let skew = Duration::seconds(CLOCK_SKEW_SECONDS);
    let conditions = find_child(assertion, NS_ASSERTION, "Conditions")
        .ok_or("the assertion has no Conditions")?;
    if let Some(not_before) = conditions.attribute("NotBefore") {
        if parse_instant(not_before)? > now + skew {
            return Err("the assertion is not yet valid (NotBefore)".into());
        }
    }
    let not_on_or_after = parse_instant(
        conditions
            .attribute("NotOnOrAfter")
            .ok_or("the assertion Conditions carry no NotOnOrAfter")?,
    )?;
    if now - skew >= not_on_or_after {
        return Err("the assertion has expired (NotOnOrAfter)".into());
    }
    let audiences: Vec<&str> = conditions
        .children()
        .filter(|child| child.is_element() && child.tag_name().name() == "AudienceRestriction")
        .flat_map(|restriction| restriction.children())
        .filter(|child| child.is_element() && child.tag_name().name() == "Audience")
        .filter_map(|audience| audience.text())
        .map(str::trim)
        .collect();
    if audiences.is_empty() {
        return Err("the assertion carries no AudienceRestriction".into());
    }
    if !audiences.contains(&sp_entity_id) {
        return Err("the assertion audience does not include this service provider".into());
    }

    // Subject: NameID plus a valid bearer confirmation.
    let subject =
        find_child(assertion, NS_ASSERTION, "Subject").ok_or("the assertion has no Subject")?;
    let name_id = find_child(subject, NS_ASSERTION, "NameID");
    let name_id_value = name_id
        .and_then(|node| node.text())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let name_id_format = name_id
        .and_then(|node| node.attribute("Format"))
        .map(str::to_string);

    let bearer = subject
        .children()
        .filter(|child| {
            child.is_element()
                && child.tag_name().name() == "SubjectConfirmation"
                && child.attribute("Method") == Some(METHOD_BEARER)
        })
        .find_map(|confirmation| find_child(confirmation, NS_ASSERTION, "SubjectConfirmationData"))
        .ok_or("the assertion has no bearer SubjectConfirmation")?;
    if let Some(expiry) = bearer.attribute("NotOnOrAfter") {
        if now - skew >= parse_instant(expiry)? {
            return Err("the subject confirmation has expired".into());
        }
    }
    if let Some(recipient) = bearer.attribute("Recipient") {
        if recipient != acs {
            return Err("the subject confirmation Recipient does not match our ACS URL".into());
        }
    }
    if let Some(confirmation_irt) = bearer.attribute("InResponseTo") {
        if confirmation_irt != in_response_to {
            return Err("the subject confirmation InResponseTo does not match the response".into());
        }
    }

    // Attributes (Name and FriendlyName both usable for lookups).
    let mut attributes: Vec<(String, String)> = vec![];
    for attribute in find_child(assertion, NS_ASSERTION, "AttributeStatement")
        .into_iter()
        .flat_map(|statement| statement.children())
        .filter(|child| child.is_element() && child.tag_name().name() == "Attribute")
    {
        let value = attribute
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == "AttributeValue")
            .find_map(|node| node.text())
            .unwrap_or_default()
            .trim()
            .to_string();
        if let Some(name) = attribute.attribute("Name") {
            attributes.push((name.to_lowercase(), value.clone()));
        }
        if let Some(friendly) = attribute.attribute("FriendlyName") {
            attributes.push((friendly.to_lowercase(), value));
        }
    }

    Ok(ValidatedAssertion {
        assertion_id,
        in_response_to,
        name_id: name_id_value,
        name_id_format,
        attributes,
        not_on_or_after,
    })
}

/// Build the public `/api/v2/auth/saml` router.
pub fn build_saml_router(state: Arc<ApiState>) -> Router {
    Router::new()
        .nest(
            "/api/v2/auth/saml",
            Router::new()
                .route("/metadata", get(saml_metadata))
                .route("/start", get(saml_start))
                .route("/acs", post(saml_acs))
                .with_state(state),
        )
        .layer(middleware::from_fn(
            |request: Request, next: axum::middleware::Next| timing_middleware(request, next),
        ))
}
