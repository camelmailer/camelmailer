//! Admin API v2 resource handlers: domains, credentials, routes, webhooks,
//! suppressions (server-scoped), plus users and IP pools (global). Ports of
//! the corresponding controllers in `app/controllers/admin_api/`.

use crate::app::{
    find_server, paginate, render_deleted, render_error, render_not_found,
    render_parameter_missing, render_store_error, render_success, render_validation_error,
    ApiResponse, ApiState, PaginationParams, Principal, RequestStart,
};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use camelmailer_core::{
    Credential, CredentialType, Domain, IpAddress, IpPool, NewCredential, NewIpAddress, NewRoute,
    NewSenderAddress, NewSuppression, NewTemplate, NewUser, NewWebhook, Route, RouteMode,
    SenderAddress, Server, StoreError, Suppression, Template, User, Webhook, WEBHOOK_EVENTS,
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

/// base64(SubjectPublicKeyInfo DER) — the `p=` value of a DKIM TXT record —
/// for an RSA private key in PKCS#8 or PKCS#1 PEM form.
pub(crate) fn dkim_public_key_b64(private_pem: &str) -> Option<String> {
    use base64::Engine;
    use rsa::pkcs1::DecodeRsaPrivateKey;
    use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey};
    let key = rsa::RsaPrivateKey::from_pkcs8_pem(private_pem)
        .or_else(|_| rsa::RsaPrivateKey::from_pkcs1_pem(private_pem))
        .ok()?;
    let der = key.to_public_key().to_public_key_der().ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(der.as_bytes()))
}

/// Generate the RSA-2048 DKIM key a new domain is created with.
fn generate_dkim_key() -> Result<String, String> {
    use rsa::pkcs8::EncodePrivateKey;
    let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .map_err(|error| format!("could not generate an RSA key: {error}"))?;
    key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .map(|pem| pem.to_string())
        .map_err(|error| format!("could not encode the DKIM key: {error}"))
}

fn dns_record_json(name: String, value: String) -> Value {
    json!({ "name": name, "type": "TXT", "value": value })
}

/// A domain plus the three DNS records its owner should publish. The DKIM
/// record uses the domain's own key when it has one and the installation
/// key otherwise; the private key itself is never rendered.
fn domain_json(state: &ApiState, domain: &Domain) -> Value {
    let dkim_public = domain
        .dkim_private_key
        .as_deref()
        .and_then(dkim_public_key_b64)
        .or_else(|| state.installation_dkim_public_key.clone());
    let dns = &state.config.dns;
    let spf_source = if dns.spf_include.is_empty() {
        format!("a:{}", state.config.camelmailer.smtp_hostname)
    } else {
        format!("include:{}", dns.spf_include)
    };
    json!({
        "id": domain.id,
        "uuid": domain.uuid,
        "name": domain.name,
        "verified": domain.verified,
        "dkim_record": dkim_public.map(|p| dns_record_json(
            format!("{}._domainkey.{}", dns.dkim_identifier, domain.name),
            format!("v=DKIM1; k=rsa; p={p}"),
        )),
        "verification_record": dns_record_json(
            format!("_camelmailer-challenge.{}", domain.name),
            format!("camelmailer-verification={}", domain.verification_token),
        ),
        "spf_record": dns_record_json(
            domain.name.clone(),
            format!("v=spf1 {spf_source} ~all"),
        ),
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
    from_result(
        &start,
        state.store.list_domains(server.id).await,
        |domains| {
            let result = paginate(&domains, &params);
            ok(
                &start,
                json!({
                    "domains": result
                        .items
                        .iter()
                        .map(|domain| domain_json(&state, domain))
                        .collect::<Vec<_>>(),
                    "pagination": result.pagination,
                }),
            )
        },
    )
}

#[derive(Deserialize)]
pub(crate) struct CreateDomain {
    name: Option<String>,
    /// Optional RSA private key (PKCS#8 or PKCS#1 PEM) to import as the
    /// domain's DKIM key. Migrations pass the source provider's key so the
    /// signing key, and therefore the published DKIM record, carries over
    /// unchanged. When absent a fresh RSA-2048 key is generated.
    dkim_private_key: Option<String>,
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
    // A domain either imports an existing DKIM key (migrations preserve the
    // source key so DNS and reputation carry over) or gets a fresh RSA-2048
    // key. Domains created before per-domain keys existed sign with the
    // installation key.
    let dkim_private_key = match body.dkim_private_key.filter(|k| !k.trim().is_empty()) {
        Some(pem) => {
            if dkim_public_key_b64(&pem).is_none() {
                return render_validation_error(
                    Some(&start),
                    "dkim_private_key must be a valid RSA private key in PKCS#8 or PKCS#1 PEM form",
                );
            }
            pem
        }
        None => match tokio::task::spawn_blocking(generate_dkim_key).await {
            Ok(Ok(pem)) => pem,
            Ok(Err(message)) => {
                return render_store_error(Some(&start), StoreError::Other(message))
            }
            Err(error) => {
                return render_store_error(Some(&start), StoreError::Other(error.to_string()))
            }
        },
    };
    from_result(
        &start,
        state
            .store
            .create_server_domain(server.id, &name, Some(dkim_private_key))
            .await,
        |domain| created(&start, json!({ "domain": domain_json(&state, &domain) })),
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
        Ok(domain) => ok(&start, json!({ "domain": domain_json(&state, &domain) })),
        Err(response) => response,
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct VerifyDomain {
    force: Option<bool>,
}

/// `POST …/domains/{name}/verify` — prove domain ownership via DNS: the
/// TXT record `_camelmailer-challenge.<domain>` must contain
/// `camelmailer-verification=<token>`. Operators authenticating with the
/// `X-Admin-API-Key` machine key (never a user session) may skip the
/// check with `{"force": true}`.
pub(crate) async fn domains_verify(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path((org, server, name)): Path<(String, String, String)>,
    body: Option<Json<VerifyDomain>>,
) -> ApiResponse {
    let mut domain = match require_domain(&state, &start, &org, &server, &name).await {
        Ok(domain) => domain,
        Err(response) => return response,
    };
    let force = body
        .map(|Json(b)| b.force.unwrap_or(false))
        .unwrap_or(false);
    if force {
        if !matches!(principal.0, Principal::AdminKey(_)) {
            return render_error(
                Some(&start),
                StatusCode::FORBIDDEN,
                "Forbidden",
                "Forced verification requires the X-Admin-API-Key machine key",
            );
        }
    } else {
        let record_name = format!("_camelmailer-challenge.{}", domain.name);
        let expected = format!("camelmailer-verification={}", domain.verification_token);
        match state.dns_resolver.txt_records(&record_name).await {
            Ok(records) if records.iter().any(|record| record.trim() == expected) => {}
            Ok(_) => {
                return render_validation_error(
                    Some(&start),
                    &format!(
                        "Domain ownership is not proven yet: publish a TXT record at \
                         {record_name} with the value \"{expected}\", wait for DNS to \
                         propagate, then retry"
                    ),
                )
            }
            Err(error) => {
                return render_validation_error(
                    Some(&start),
                    &format!("Could not check the TXT record at {record_name}: {error}"),
                )
            }
        }
    }
    if let Err(error) = state.store.set_domain_verified(domain.id, true).await {
        return render_store_error(Some(&start), error);
    }
    domain.verified = true;
    ok(&start, json!({ "domain": domain_json(&state, &domain) }))
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

// ----------------------------------------------------------- track domains

/// A track domain plus the CNAME record to publish for it: the domain must
/// point at this installation's web server so the public `/track/*`
/// endpoints receive the clicks and opens.
fn track_domain_json(state: &ApiState, domain: &camelmailer_core::TrackDomain) -> Value {
    json!({
        "id": domain.id,
        "uuid": domain.uuid,
        "name": domain.name,
        "verified": domain.verified,
        "cname_record": {
            "name": domain.name,
            "type": "CNAME",
            "value": state.config.camelmailer.web_hostname,
        },
    })
}

pub(crate) async fn track_domains_index(
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
        state.store.list_track_domains(server.id).await,
        |domains| {
            let result = paginate(&domains, &params);
            ok(
                &start,
                json!({
                    "track_domains": result
                        .items
                        .iter()
                        .map(|domain| track_domain_json(&state, domain))
                        .collect::<Vec<_>>(),
                    "pagination": result.pagination,
                }),
            )
        },
    )
}

#[derive(Deserialize)]
pub(crate) struct CreateTrackDomain {
    name: Option<String>,
}

pub(crate) async fn track_domains_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Json(body): Json<CreateTrackDomain>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let Some(name) = body
        .name
        .map(|n| n.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|n| !n.is_empty())
    else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: name",
        );
    };
    if !name.contains('.') || name.contains(char::is_whitespace) {
        return render_validation_error(
            Some(&start),
            "name must be a full hostname, e.g. track.example.com",
        );
    }
    from_result(
        &start,
        state.store.create_track_domain(server.id, &name).await,
        |domain| {
            created(
                &start,
                json!({ "track_domain": track_domain_json(&state, &domain) }),
            )
        },
    )
}

async fn require_track_domain(
    state: &ApiState,
    start: &RequestStart,
    org: &str,
    server: &str,
    id: camelmailer_core::Id,
) -> Result<camelmailer_core::TrackDomain, ApiResponse> {
    let server = require_server(state, start, org, server).await?;
    match state.store.track_domain_by_id(server.id, id).await {
        Ok(Some(domain)) => Ok(domain),
        Ok(None) => Err(render_not_found(Some(start))),
        Err(error) => Err(render_store_error(Some(start), error)),
    }
}

pub(crate) async fn track_domains_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    match require_track_domain(&state, &start, &org, &server, id).await {
        Ok(domain) => ok(
            &start,
            json!({ "track_domain": track_domain_json(&state, &domain) }),
        ),
        Err(response) => response,
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct VerifyTrackDomain {
    force: Option<bool>,
}

/// `POST …/track_domains/{id}/verify` — check that the track domain CNAMEs
/// to this installation (the web server or the installation-wide track
/// domain). Machine keys may skip the check with `{"force": true}`, exactly
/// like sending-domain verification.
pub(crate) async fn track_domains_verify(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path((org, server, id)): Path<(String, String, u64)>,
    body: Option<Json<VerifyTrackDomain>>,
) -> ApiResponse {
    let mut domain = match require_track_domain(&state, &start, &org, &server, id).await {
        Ok(domain) => domain,
        Err(response) => return response,
    };
    let force = body
        .map(|Json(b)| b.force.unwrap_or(false))
        .unwrap_or(false);
    if force {
        if !matches!(principal.0, Principal::AdminKey(_)) {
            return render_error(
                Some(&start),
                StatusCode::FORBIDDEN,
                "Forbidden",
                "Forced verification requires the X-Admin-API-Key machine key",
            );
        }
    } else {
        let mut targets = vec![state.config.camelmailer.web_hostname.to_ascii_lowercase()];
        let global_track = state.config.dns.track_domain.to_ascii_lowercase();
        if !global_track.is_empty() {
            targets.push(global_track);
        }
        match state.dns_resolver.cname(&domain.name).await {
            Ok(Some(target))
                if targets.contains(&target.trim_end_matches('.').to_ascii_lowercase()) => {}
            Ok(_) => {
                return render_validation_error(
                    Some(&start),
                    &format!(
                        "The track domain does not point here yet: publish a CNAME record \
                         at {} with the value \"{}\", wait for DNS to propagate, then retry",
                        domain.name, targets[0]
                    ),
                )
            }
            Err(error) => {
                return render_validation_error(
                    Some(&start),
                    &format!(
                        "Could not check the CNAME record at {}: {error}",
                        domain.name
                    ),
                )
            }
        }
    }
    if let Err(error) = state.store.set_track_domain_verified(domain.id, true).await {
        return render_store_error(Some(&start), error);
    }
    domain.verified = true;
    ok(
        &start,
        json!({ "track_domain": track_domain_json(&state, &domain) }),
    )
}

pub(crate) async fn track_domains_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    match require_track_domain(&state, &start, &org, &server, id).await {
        Ok(domain) => from_result(
            &start,
            state.store.delete_track_domain(domain.id).await,
            |_| render_deleted(Some(&start)),
        ),
        Err(response) => response,
    }
}

// ---------------------------------------------------------- domain health

/// Order of severity for the health traffic light.
fn health_rank(status: &str) -> u8 {
    match status {
        "ok" => 0,
        "warning" => 1,
        _ => 2, // missing
    }
}

fn health_check_json(
    status: &str,
    record_name: &str,
    found: &[String],
    expected: Option<&str>,
    problems: &[String],
) -> Value {
    json!({
        "status": status,
        "record_name": record_name,
        "found": found,
        "expected": expected,
        "problems": problems,
    })
}

/// Escalation heuristics for the recommended next step: with at least
/// this many reported messages and this pass rate, the next-stricter
/// DMARC policy is suggested (documented in docs/dmarc.md).
const DMARC_ESCALATION_MIN_VOLUME: i64 = 10;
const DMARC_ESCALATION_MIN_PASS_RATE: f64 = 0.95;
/// Compliance window the health check looks at.
const DMARC_COMPLIANCE_WINDOW_DAYS: i64 = 30;

/// `GET …/domains/{name}/health` — live DNS health check of a sending
/// domain: SPF, DKIM and DMARC via the injected [`camelmailer_core::DnsResolver`],
/// plus the stored DMARC compliance data (when message storage is
/// configured) to recommend the next policy step.
pub(crate) async fn domains_health(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, name)): Path<(String, String, String)>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let domain = match state.store.domain_by_name(server.id, &name).await {
        Ok(Some(domain)) => domain,
        Ok(None) => return render_not_found(Some(&start)),
        Err(error) => return render_store_error(Some(&start), error),
    };

    use camelmailer_core::dmarc as dmarc_rules;
    let dns = &state.config.dns;

    // ---- SPF: TXT at the domain itself
    let spf_mechanism = if dns.spf_include.is_empty() {
        format!("a:{}", state.config.camelmailer.smtp_hostname)
    } else {
        format!("include:{}", dns.spf_include)
    };
    let expected_spf = format!("v=spf1 {spf_mechanism} ~all");
    let mut spf_problems: Vec<String> = Vec::new();
    let mut spf_found: Vec<String> = Vec::new();
    let spf_status = match state.dns_resolver.txt_records(&domain.name).await {
        Err(error) => {
            spf_problems.push(format!("DNS lookup failed: {error}"));
            "warning"
        }
        Ok(records) => {
            spf_found = records
                .iter()
                .filter(|record| dmarc_rules::is_spf_record(record))
                .cloned()
                .collect();
            match spf_found.len() {
                0 => {
                    spf_problems.push(format!("no v=spf1 TXT record found at {}", domain.name));
                    "missing"
                }
                1 => {
                    let record = &spf_found[0];
                    if !dmarc_rules::spf_contains_mechanism(record, &spf_mechanism) {
                        spf_problems.push(format!(
                            "the record does not include this installation ({spf_mechanism})"
                        ));
                    }
                    match dmarc_rules::spf_all_qualifier(record) {
                        Some('-') | Some('~') => {}
                        Some(other) => spf_problems.push(format!(
                            "the record ends with \"{other}all\" — use ~all or -all so unauthorized senders fail SPF"
                        )),
                        None => spf_problems.push(
                            "the record has no all mechanism — append ~all or -all".into(),
                        ),
                    }
                    if spf_problems.is_empty() {
                        "ok"
                    } else {
                        "warning"
                    }
                }
                n => {
                    spf_problems.push(format!(
                        "{n} v=spf1 records found — receivers treat multiple SPF records as a permanent error"
                    ));
                    "warning"
                }
            }
        }
    };

    // ---- DKIM: TXT at <selector>._domainkey.<domain>
    let dkim_record_name = format!("{}._domainkey.{}", dns.dkim_identifier, domain.name);
    let expected_dkim_key = domain
        .dkim_private_key
        .as_deref()
        .and_then(dkim_public_key_b64)
        .or_else(|| state.installation_dkim_public_key.clone());
    let expected_dkim = expected_dkim_key
        .as_deref()
        .map(|key| format!("v=DKIM1; k=rsa; p={key}"));
    let mut dkim_problems: Vec<String> = Vec::new();
    let mut dkim_found: Vec<String> = Vec::new();
    let dkim_status = match state.dns_resolver.txt_records(&dkim_record_name).await {
        Err(error) => {
            dkim_problems.push(format!("DNS lookup failed: {error}"));
            "warning"
        }
        Ok(records) if records.is_empty() => {
            dkim_problems.push(format!("no TXT record found at {dkim_record_name}"));
            "missing"
        }
        Ok(records) => {
            dkim_found = records;
            match expected_dkim_key.as_deref() {
                None => {
                    dkim_problems.push(
                        "neither the domain nor the installation has a DKIM key configured".into(),
                    );
                    "warning"
                }
                Some(expected) => {
                    let matches = dkim_found.iter().any(|record| {
                        dmarc_rules::dkim_public_key_of_record(record).as_deref() == Some(expected)
                    });
                    if matches {
                        "ok"
                    } else {
                        dkim_problems.push(
                            "the published public key does not match the key this server signs with"
                                .into(),
                        );
                        "warning"
                    }
                }
            }
        }
    };

    // ---- DMARC: TXT at _dmarc.<domain>
    let dmarc_record_name = format!("_dmarc.{}", domain.name);
    let mut dmarc_problems: Vec<String> = Vec::new();
    let mut dmarc_found: Vec<String> = Vec::new();
    let mut dmarc_policy: Option<camelmailer_core::dmarc::DmarcPolicy> = None;
    let dmarc_status = match state.dns_resolver.txt_records(&dmarc_record_name).await {
        Err(error) => {
            dmarc_problems.push(format!("DNS lookup failed: {error}"));
            "warning"
        }
        Ok(records) => {
            dmarc_found = records
                .iter()
                .filter(|record| dmarc_rules::is_dmarc_record(record))
                .cloned()
                .collect();
            match dmarc_found.len() {
                0 => {
                    dmarc_problems.push(format!(
                        "no v=DMARC1 TXT record found at {dmarc_record_name}"
                    ));
                    "missing"
                }
                n => {
                    if n > 1 {
                        dmarc_problems.push(format!(
                            "{n} DMARC records found — receivers ignore the policy entirely"
                        ));
                    }
                    let policy =
                        dmarc_rules::parse_dmarc_record(&dmarc_found[0]).unwrap_or_default();
                    if policy.p.is_none() {
                        dmarc_problems.push(
                            "the record has no valid p= tag (none, quarantine or reject)".into(),
                        );
                    }
                    if policy.rua.is_empty() {
                        dmarc_problems.push(
                            "the record has no rua= tag — you receive no aggregate reports".into(),
                        );
                    }
                    dmarc_policy = Some(policy);
                    if dmarc_problems.is_empty() {
                        "ok"
                    } else {
                        "warning"
                    }
                }
            }
        }
    };

    // ---- stored compliance data (drives the policy recommendation)
    let compliance = match state.server_store.as_ref() {
        None => None,
        Some(server_store) => {
            let filter = camelmailer_core::DmarcFilter {
                domain: Some(domain.name.clone()),
                from: Some(
                    chrono::Utc::now() - chrono::Duration::days(DMARC_COMPLIANCE_WINDOW_DAYS),
                ),
                to: None,
            };
            match server_store.dmarc_records(server.id, &filter).await {
                Ok(records) => Some(camelmailer_core::dmarc::summarize(&records)),
                Err(error) => {
                    tracing::warn!(%error, "could not load DMARC compliance data");
                    None
                }
            }
        }
    };

    // ---- the RUA address of this server's internal DMARC route, if any
    let rua_address = match state.store.list_routes(server.id).await {
        Ok(routes) => {
            let dmarc_route = routes.into_iter().find(|route| {
                route.endpoint_url.as_deref() == Some(dmarc_rules::DMARC_REPORTS_ENDPOINT)
            });
            match dmarc_route {
                Some(route) => {
                    let route_domain = match route.domain_id {
                        Some(domain_id) => state
                            .store
                            .list_domains(server.id)
                            .await
                            .ok()
                            .and_then(|domains| domains.into_iter().find(|d| d.id == domain_id))
                            .map(|d| d.name),
                        None => server.inbound_domain.clone(),
                    };
                    route_domain.map(|domain| format!("{}@{}", route.name, domain))
                }
                None => None,
            }
        }
        Err(_) => None,
    };

    // ---- next step: walk the policy journey
    let policy = dmarc_policy.as_ref().and_then(|p| p.p.as_deref());
    let high_compliance = compliance.as_ref().is_some_and(|summary| {
        summary.total >= DMARC_ESCALATION_MIN_VOLUME
            && summary.pass_rate >= DMARC_ESCALATION_MIN_PASS_RATE
    });
    let rua_hint = rua_address
        .as_deref()
        .map(|address| format!("mailto:{address}"))
        .unwrap_or_else(|| {
            "mailto:<address of an inbound route targeting internal://dmarc-reports>".into()
        });
    let next_step = if dmarc_status == "missing" {
        format!(
            "Publish a DMARC record at {dmarc_record_name}: start monitoring with \
             \"v=DMARC1; p=none; rua={rua_hint}\" — reports will appear here."
        )
    } else if policy == Some("none") && high_compliance {
        "Compliance is high on recent aggregate reports — tighten the policy to p=quarantine."
            .to_string()
    } else if policy == Some("none") {
        if dmarc_policy.as_ref().is_some_and(|p| p.rua.is_empty()) {
            format!(
                "Add rua={rua_hint} to the DMARC record so aggregate reports arrive here, \
                 then escalate the policy once compliance is high."
            )
        } else {
            "Keep collecting aggregate reports; move to p=quarantine once the pass rate stays high."
                .to_string()
        }
    } else if policy == Some("quarantine") && high_compliance {
        "Compliance is high on recent aggregate reports — consider the final step to p=reject."
            .to_string()
    } else if spf_status != "ok" || dkim_status != "ok" {
        "Fix the SPF/DKIM issues listed in the checks so aligned mail passes DMARC.".to_string()
    } else {
        "Everything looks good — keep monitoring the aggregate reports.".to_string()
    };

    let overall = [spf_status, dkim_status, dmarc_status]
        .into_iter()
        .max_by_key(|status| health_rank(status))
        .unwrap_or("ok");

    ok(
        &start,
        json!({
            "health": {
                "domain": domain.name,
                "checks": {
                    "spf": health_check_json(
                        spf_status,
                        &domain.name,
                        &spf_found,
                        Some(&expected_spf),
                        &spf_problems,
                    ),
                    "dkim": health_check_json(
                        dkim_status,
                        &dkim_record_name,
                        &dkim_found,
                        expected_dkim.as_deref(),
                        &dkim_problems,
                    ),
                    "dmarc": {
                        "status": dmarc_status,
                        "record_name": dmarc_record_name,
                        "found": dmarc_found,
                        "policy": dmarc_policy.map(|policy| json!({
                            "p": policy.p,
                            "sp": policy.sp,
                            "rua": policy.rua,
                            "pct": policy.pct,
                        })),
                        "problems": dmarc_problems,
                    },
                },
                "overall": overall,
                "next_step": next_step,
                "rua_address": rua_address,
                "compliance": compliance.map(|summary| json!({
                    "window_days": DMARC_COMPLIANCE_WINDOW_DAYS,
                    "total": summary.total,
                    "pass": summary.pass,
                    "pass_rate": summary.pass_rate,
                })),
            }
        }),
    )
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
        "last_used_at": credential.last_used_at.map(|at| at.to_rfc3339()),
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
        |credential| {
            created(
                &start,
                json!({ "credential": credential_json(&credential) }),
            )
        },
    )
}

/// Resolve a credential by numeric id or by uuid. Postal's Admin API v2
/// addressed credentials by uuid, so integrations built against it keep
/// working unchanged.
async fn require_credential(
    state: &ApiState,
    start: &RequestStart,
    org: &str,
    server: &str,
    id_or_uuid: &str,
) -> Result<Credential, ApiResponse> {
    let server = require_server(state, start, org, server).await?;
    let credential = if let Ok(id) = id_or_uuid.parse::<u64>() {
        state.store.credential_by_id(server.id, id).await
    } else {
        state
            .store
            .list_credentials(server.id)
            .await
            .map(|credentials| {
                credentials
                    .into_iter()
                    .find(|credential| credential.uuid == id_or_uuid)
            })
    };
    match credential {
        Ok(Some(credential)) => Ok(credential),
        Ok(None) => Err(render_not_found(Some(start))),
        Err(error) => Err(render_store_error(Some(start), error)),
    }
}

pub(crate) async fn credentials_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, String)>,
) -> ApiResponse {
    match require_credential(&state, &start, &org, &server, &id).await {
        Ok(credential) => ok(
            &start,
            json!({ "credential": credential_json(&credential) }),
        ),
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
    Path((org, server, id)): Path<(String, String, String)>,
    Json(body): Json<UpdateCredential>,
) -> ApiResponse {
    match require_credential(&state, &start, &org, &server, &id).await {
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
                |credential| {
                    ok(
                        &start,
                        json!({ "credential": credential_json(&credential) }),
                    )
                },
            )
        }
        Err(response) => response,
    }
}

pub(crate) async fn credentials_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, String)>,
) -> ApiResponse {
    match require_credential(&state, &start, &org, &server, &id).await {
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
        "endpoint_url": route.endpoint_url,
        "mode": match route.mode {
            RouteMode::Endpoint => "Endpoint",
            RouteMode::Accept => "Accept",
            RouteMode::Hold => "Hold",
            RouteMode::Bounce => "Bounce",
            RouteMode::Reject => "Reject",
        },
    })
}

/// Validate a route's delivery target: an HTTP(S) URL, or exactly the
/// internal DMARC-ingestion target [`camelmailer_core::DMARC_REPORTS_ENDPOINT`].
fn validate_route_endpoint(url: &str) -> Result<(), String> {
    if url.starts_with("http://")
        || url.starts_with("https://")
        || url == camelmailer_core::DMARC_REPORTS_ENDPOINT
    {
        return Ok(());
    }
    if url.starts_with("internal://") {
        return Err(format!(
            "Endpoint URL {url:?} is not a known internal target (the only one is {})",
            camelmailer_core::DMARC_REPORTS_ENDPOINT
        ));
    }
    Err(format!(
        "Endpoint URL must be an HTTP(S) URL or {}",
        camelmailer_core::DMARC_REPORTS_ENDPOINT
    ))
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
    endpoint_url: Option<String>,
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
    if let Some(url) = body.endpoint_url.as_deref().filter(|u| !u.is_empty()) {
        if let Err(message) = validate_route_endpoint(url) {
            return render_validation_error(Some(&start), &message);
        }
    }
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
                endpoint_url: body.endpoint_url,
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
        "events": webhook.events,
        "headers": webhook.headers,
    })
}

/// Validate subscribed event names against the worker's event list.
fn validate_webhook_events(events: &[String]) -> Result<(), String> {
    for event in events {
        if !WEBHOOK_EVENTS.contains(&event.as_str()) {
            return Err(format!(
                "Event {event:?} is not a valid webhook event (valid events: {})",
                WEBHOOK_EVENTS.join(", ")
            ));
        }
    }
    Ok(())
}

/// Validate custom delivery headers. Error messages carry only the header
/// NAME — values may be secrets (e.g. Authorization) and are never logged
/// or echoed.
fn validate_webhook_headers(
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    for (name, value) in headers {
        if axum::http::HeaderName::try_from(name.as_str()).is_err() {
            return Err(format!(
                "Header name {name:?} is not a valid HTTP header name"
            ));
        }
        if axum::http::HeaderValue::try_from(value.as_str()).is_err() {
            return Err(format!("Header {name:?} has an invalid value"));
        }
    }
    Ok(())
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
    /// Subscribed event names; empty/omitted = all events.
    events: Option<Vec<String>>,
    /// Extra HTTP headers set on every delivery request.
    headers: Option<std::collections::BTreeMap<String, String>>,
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
    let events = body.events.unwrap_or_default();
    if let Err(message) = validate_webhook_events(&events) {
        return render_validation_error(Some(&start), &message);
    }
    let headers = body.headers.unwrap_or_default();
    if let Err(message) = validate_webhook_headers(&headers) {
        return render_validation_error(Some(&start), &message);
    }
    let all_events = if events.is_empty() {
        body.all_events.unwrap_or(true)
    } else {
        false
    };
    from_result(
        &start,
        state
            .store
            .create_webhook(NewWebhook {
                server_id: server.id,
                name,
                url,
                all_events,
                sign: body.sign.unwrap_or(true),
                events,
                headers,
            })
            .await,
        |webhook| created(&start, json!({ "webhook": webhook_json(&webhook) })),
    )
}

#[derive(Deserialize)]
pub(crate) struct UpdateWebhook {
    name: Option<String>,
    url: Option<String>,
    sign: Option<bool>,
    enabled: Option<bool>,
    events: Option<Vec<String>>,
    headers: Option<std::collections::BTreeMap<String, String>>,
}

pub(crate) async fn webhooks_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
    Json(body): Json<UpdateWebhook>,
) -> ApiResponse {
    let mut webhook = match require_webhook(&state, &start, &org, &server, id).await {
        Ok(webhook) => webhook,
        Err(response) => return response,
    };
    if let Some(name) = body.name.filter(|n| !n.is_empty()) {
        webhook.name = name;
    }
    if let Some(url) = body.url.filter(|u| !u.is_empty()) {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return render_validation_error(Some(&start), "Url must be a valid HTTP(S) URL");
        }
        webhook.url = url;
    }
    if let Some(sign) = body.sign {
        webhook.sign = sign;
    }
    if let Some(enabled) = body.enabled {
        webhook.enabled = enabled;
    }
    if let Some(events) = body.events {
        if let Err(message) = validate_webhook_events(&events) {
            return render_validation_error(Some(&start), &message);
        }
        webhook.all_events = events.is_empty();
        webhook.events = events;
    }
    if let Some(headers) = body.headers {
        if let Err(message) = validate_webhook_headers(&headers) {
            return render_validation_error(Some(&start), &message);
        }
        webhook.headers = headers;
    }
    from_result(
        &start,
        state.store.update_webhook(webhook).await,
        |webhook| ok(&start, json!({ "webhook": webhook_json(&webhook) })),
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
            from_result(
                &start,
                state.store.update_webhook(webhook).await,
                |webhook| ok(&start, json!({ "webhook": webhook_json(&webhook) })),
            )
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

#[derive(Deserialize)]
pub(crate) struct TestWebhook {
    event: Option<String>,
}

/// `POST …/webhooks/{id}/test` — synchronously deliver one sample payload
/// for the chosen event to the webhook URL (custom headers + signature
/// exactly like the worker; the payload carries `"test": true`). The
/// outcome is reported, never retried, and never written to the audit log.
pub(crate) async fn webhooks_test(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
    Json(body): Json<TestWebhook>,
) -> ApiResponse {
    let webhook = match require_webhook(&state, &start, &org, &server, id).await {
        Ok(webhook) => webhook,
        Err(response) => return response,
    };
    let Some(event) = body.event.filter(|e| !e.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: event",
        );
    };
    if !WEBHOOK_EVENTS.contains(&event.as_str()) {
        return render_validation_error(
            Some(&start),
            &format!(
                "Event {event:?} is not a valid webhook event (valid events: {})",
                WEBHOOK_EVENTS.join(", ")
            ),
        );
    }

    let request = crate::webhook_send::build_test_request(
        &webhook,
        &event,
        state.installation_signing_key_pem.as_deref(),
    );
    let started = std::time::Instant::now();
    let outcome = state.webhook_sender.send(request).await;
    let duration_ms = started.elapsed().as_millis() as u64;

    let result = match outcome {
        Ok(response) => json!({
            "delivered": (200..300).contains(&response.status),
            "status_code": response.status,
            "duration_ms": duration_ms,
        }),
        Err(error) => json!({
            "delivered": false,
            "status_code": Value::Null,
            "duration_ms": duration_ms,
            "error": error,
        }),
    };
    ok(&start, json!({ "result": result }))
}

// ------------------------------------------------------- sender addresses

fn sender_address_json(address: &SenderAddress) -> Value {
    json!({
        "id": address.id,
        "uuid": address.uuid,
        "email_address": address.email_address,
        "verified": address.verified,
        "status": if address.verified { "confirmed" } else { "pending" },
    })
}

pub(crate) async fn sender_addresses_index(
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
        state.store.list_sender_addresses(server.id).await,
        |addresses| {
            let result = paginate(&addresses, &params);
            ok(
                &start,
                json!({
                    "sender_addresses": result.items.iter().map(sender_address_json).collect::<Vec<_>>(),
                    "pagination": result.pagination,
                }),
            )
        },
    )
}

#[derive(Deserialize)]
pub(crate) struct CreateSenderAddress {
    email: Option<String>,
}

/// `POST …/sender_addresses` — add a single sender address. A verification
/// token is generated (stored hashed) and, when `app_mail` is enabled, a
/// confirmation link is emailed to exactly that address. When the mail
/// cannot be sent (app_mail disabled, or no frontend URL), the token is
/// returned to the operator in the response — exactly once.
pub(crate) async fn sender_addresses_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Json(body): Json<CreateSenderAddress>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let Some(email) = body
        .email
        .map(|e| e.trim().to_lowercase())
        .filter(|e| !e.is_empty())
    else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: email",
        );
    };
    if !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
        return render_validation_error(Some(&start), "Email address is invalid");
    }
    let token = camelmailer_core::auth::generate_auth_token();
    let address = match state
        .store
        .create_sender_address(NewSenderAddress {
            server_id: server.id,
            email_address: email,
            verification_token_hash: camelmailer_core::auth::hash_token(&token),
        })
        .await
    {
        Ok(address) => address,
        Err(error) => return render_store_error(Some(&start), error),
    };

    let link = state.config.auth.frontend_url.as_deref().map(|base| {
        format!(
            "{}/sender-addresses/confirm?token={}",
            base.trim_end_matches('/'),
            token
        )
    });
    // With app_mail enabled the confirmation link is emailed to the address
    // itself (the token stays out of the response and the logs); otherwise
    // the token is handed to the operator — exactly once.
    let mut mailed = false;
    if state.config.app_mail.enabled {
        match link.as_deref() {
            Some(link) => {
                mailed = crate::app_mailer::deliver(
                    &state,
                    crate::app_mailer::sender_address_confirmation_mail(
                        &address.email_address,
                        link,
                    ),
                )
                .await;
            }
            None => tracing::warn!(
                "app_mail is enabled but auth.frontend_url is not set; cannot email the sender-address confirmation link"
            ),
        }
    }
    let mut data = json!({ "sender_address": sender_address_json(&address) });
    if !mailed {
        data["verification_token"] = json!(token);
    }
    created(&start, data)
}

pub(crate) async fn sender_addresses_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, id)): Path<(String, String, u64)>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    match state.store.sender_address_by_id(server.id, id).await {
        Ok(Some(address)) => from_result(
            &start,
            state.store.delete_sender_address(address.id).await,
            |_| render_deleted(Some(&start)),
        ),
        Ok(None) => render_not_found(Some(&start)),
        Err(error) => render_store_error(Some(&start), error),
    }
}

// ------------------------------------------------------ template copying

fn copied_template_json(template: &Template) -> Value {
    json!({
        "id": template.id,
        "uuid": template.uuid,
        "name": template.name,
        "permalink": template.permalink,
        "subject": template.subject,
        "html_body": template.html_body,
        "text_body": template.text_body,
        "archived": template.archived,
    })
}

#[derive(Deserialize)]
pub(crate) struct CopyTemplate {
    target_server: Option<String>,
    overwrite: Option<bool>,
}

/// `POST …/servers/{server}/templates/{permalink}/copy_to` — copy a
/// template to another server of the SAME organization. A target outside
/// the organization answers 404 (like any unknown permalink, so nothing
/// leaks); an existing permalink on the target is a 422 unless
/// `overwrite: true`.
pub(crate) async fn templates_copy_to(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server, template_permalink)): Path<(String, String, String)>,
    Json(body): Json<CopyTemplate>,
) -> ApiResponse {
    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let Some(server_store) = state.server_store.as_ref() else {
        return render_store_error(
            Some(&start),
            StoreError::Other("message storage is not configured".into()),
        );
    };
    let Some(target_permalink) = body.target_server.filter(|t| !t.is_empty()) else {
        return render_parameter_missing(
            Some(&start),
            "param is missing or the value is empty: target_server",
        );
    };
    let source = match server_store
        .template_by_permalink(server.id, &template_permalink)
        .await
    {
        Ok(Some(template)) => template,
        Ok(None) => return render_not_found(Some(&start)),
        Err(error) => return render_store_error(Some(&start), error),
    };
    // Only servers of the same organization resolve; anything else is an
    // indistinguishable 404.
    let target = match state
        .store
        .server_by_permalink(server.organization_id, &target_permalink)
        .await
    {
        Ok(Some(target)) => target,
        Ok(None) => return render_not_found(Some(&start)),
        Err(error) => return render_store_error(Some(&start), error),
    };
    let existing = match server_store
        .template_by_permalink(target.id, &template_permalink)
        .await
    {
        Ok(existing) => existing,
        Err(error) => return render_store_error(Some(&start), error),
    };
    match existing {
        Some(existing) => {
            if !body.overwrite.unwrap_or(false) {
                return render_validation_error(
                    Some(&start),
                    &format!(
                        "Template {template_permalink:?} already exists on server {target_permalink:?} (pass overwrite: true to replace it)"
                    ),
                );
            }
            let updated = Template {
                id: existing.id,
                uuid: existing.uuid,
                server_id: existing.server_id,
                name: source.name,
                permalink: existing.permalink,
                subject: source.subject,
                html_body: source.html_body,
                text_body: source.text_body,
                archived: source.archived,
                // the copy never imports the source server's layout wiring
                layout_id: existing.layout_id,
            };
            from_result(
                &start,
                server_store.update_template(updated).await,
                |template| {
                    ok(
                        &start,
                        json!({ "template": copied_template_json(&template), "overwritten": true }),
                    )
                },
            )
        }
        None => from_result(
            &start,
            server_store
                .create_template(NewTemplate {
                    server_id: target.id,
                    name: source.name,
                    permalink: source.permalink,
                    subject: source.subject,
                    html_body: source.html_body,
                    text_body: source.text_body,
                    // layouts belong to the source server; the copy starts bare
                    layout_id: None,
                })
                .await,
            |template| {
                created(
                    &start,
                    json!({ "template": copied_template_json(&template), "overwritten": false }),
                )
            },
        ),
    }
}

// -------------------------------------------------- historical message import

/// Cap the number of messages one import request may carry (cloud rate
/// guard — a client over the cap should split the batch and retry).
const MAX_IMPORT_MESSAGES: usize = 500;
/// Cap the total decoded raw-message bytes one import request may carry
/// (cloud size guard). 50 MiB.
const MAX_IMPORT_RAW_BYTES: usize = 50 * 1024 * 1024;

/// A timestamp accepted either as Unix seconds (a JSON number) or as an
/// RFC3339 string.
#[derive(Deserialize)]
#[serde(untagged)]
enum ImportTimestamp {
    Unix(i64),
    Rfc3339(String),
}

impl ImportTimestamp {
    fn parse(&self) -> Result<chrono::DateTime<chrono::Utc>, String> {
        match self {
            ImportTimestamp::Unix(secs) => chrono::DateTime::from_timestamp(*secs, 0)
                .ok_or_else(|| format!("timestamp out of range: {secs}")),
            ImportTimestamp::Rfc3339(text) => chrono::DateTime::parse_from_rfc3339(text)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|error| format!("invalid RFC3339 timestamp {text:?}: {error}")),
        }
    }
}

#[derive(Deserialize)]
struct ImportDeliveryJson {
    status: String,
    #[serde(default)]
    details: Option<String>,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    sent_with_ssl: bool,
    timestamp: ImportTimestamp,
}

#[derive(Deserialize)]
struct ImportOpenJson {
    timestamp: ImportTimestamp,
    #[serde(default)]
    ip: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
}

#[derive(Deserialize)]
struct ImportClickJson {
    url: String,
    timestamp: ImportTimestamp,
}

#[derive(Deserialize)]
struct ImportMessageJson {
    scope: String,
    mail_from: String,
    rcpt_to: String,
    raw_message_base64: String,
    #[serde(default)]
    received_with_ssl: bool,
    #[serde(default)]
    bounce: bool,
    #[serde(default)]
    tag: Option<String>,
    timestamp: ImportTimestamp,
    /// Optional domain name to attribute the message to (resolved to an id
    /// best-effort; ignored if unknown).
    #[serde(default)]
    domain: Option<String>,
    /// Optional credential name to attribute the message to (resolved
    /// best-effort; ignored if unknown).
    #[serde(default)]
    credential_name: Option<String>,
    #[serde(default)]
    deliveries: Vec<ImportDeliveryJson>,
    #[serde(default)]
    opens: Vec<ImportOpenJson>,
    #[serde(default)]
    clicks: Vec<ImportClickJson>,
}

#[derive(Deserialize)]
pub(crate) struct ImportBatch {
    messages: Vec<ImportMessageJson>,
}

/// Build one [`camelmailer_core::ImportMessage`] from its JSON form + the
/// pre-decoded raw and the server's resolved domain/credential name maps.
fn build_import_message(
    server_id: u64,
    item: ImportMessageJson,
    raw_message: Vec<u8>,
    domains: &std::collections::HashMap<String, u64>,
    credentials: &std::collections::HashMap<String, u64>,
) -> Result<camelmailer_core::ImportMessage, String> {
    let scope = match item.scope.as_str() {
        "incoming" => camelmailer_core::MessageScope::Incoming,
        "outgoing" => camelmailer_core::MessageScope::Outgoing,
        other => {
            return Err(format!(
                "invalid scope {other:?} (expected incoming|outgoing)"
            ))
        }
    };
    let created_at = item.timestamp.parse()?;
    let deliveries = item
        .deliveries
        .into_iter()
        .map(|d| {
            Ok(camelmailer_core::ImportDelivery {
                status: d.status,
                details: d.details,
                output: d.output,
                sent_with_ssl: d.sent_with_ssl,
                created_at: d.timestamp.parse()?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let opens = item
        .opens
        .into_iter()
        .map(|o| {
            Ok(camelmailer_core::ImportEvent {
                created_at: o.timestamp.parse()?,
                ip: o.ip,
                user_agent: o.user_agent,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let clicks = item
        .clicks
        .into_iter()
        .map(|c| {
            Ok(camelmailer_core::ImportClick {
                url: c.url,
                created_at: c.timestamp.parse()?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(camelmailer_core::ImportMessage {
        server_id,
        scope,
        mail_from: item.mail_from,
        rcpt_to: item.rcpt_to,
        raw_message,
        received_with_ssl: item.received_with_ssl,
        bounce: item.bounce,
        tag: item.tag,
        domain_id: item
            .domain
            .as_deref()
            .and_then(|name| domains.get(name).copied()),
        credential_id: item
            .credential_name
            .as_deref()
            .and_then(|name| credentials.get(name).copied()),
        created_at,
        deliveries,
        opens,
        clicks,
    })
}

/// `POST …/servers/{server}/messages/import` — import past messages (from a
/// Postal migration) as completed records WITHOUT ever queuing or sending
/// them. Each item carries its raw message (base64) plus its historical
/// deliveries, opens and clicks with their original timestamps. Per-item
/// failures are reported in `failed` rather than failing the whole batch;
/// the batch-size and total-raw-size caps are the cloud rate/size guard and
/// reject the whole request (422) so a client can back off.
pub(crate) async fn messages_import(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Json(batch): Json<ImportBatch>,
) -> ApiResponse {
    use base64::Engine;

    let server = match require_server(&state, &start, &org, &server).await {
        Ok(server) => server,
        Err(response) => return response,
    };
    let Some(server_store) = state.server_store.as_ref() else {
        return render_store_error(
            Some(&start),
            StoreError::Other("message storage is not configured".into()),
        );
    };

    if batch.messages.len() > MAX_IMPORT_MESSAGES {
        return render_validation_error(
            Some(&start),
            &format!(
                "too many messages in one import request: {} (max {MAX_IMPORT_MESSAGES})",
                batch.messages.len()
            ),
        );
    }

    // Decode every raw up front so the total-size cap is enforced BEFORE any
    // row is written. A per-item decode failure is deferred to the import
    // loop (reported in `failed`); the size cap rejects the whole request.
    let engine = base64::engine::general_purpose::STANDARD;
    let mut raws: Vec<Result<Vec<u8>, String>> = Vec::with_capacity(batch.messages.len());
    let mut total_bytes: usize = 0;
    for item in &batch.messages {
        match engine.decode(item.raw_message_base64.as_bytes()) {
            Ok(bytes) => {
                total_bytes = total_bytes.saturating_add(bytes.len());
                raws.push(Ok(bytes));
            }
            Err(error) => raws.push(Err(format!("invalid base64 raw_message: {error}"))),
        }
    }
    if total_bytes > MAX_IMPORT_RAW_BYTES {
        return render_validation_error(
            Some(&start),
            &format!(
                "import request too large: {total_bytes} bytes of raw messages (max {MAX_IMPORT_RAW_BYTES})"
            ),
        );
    }

    // Resolve the server's domain/credential names to ids once (best effort;
    // an unknown name simply leaves the attribution null).
    let domains: std::collections::HashMap<String, u64> =
        match state.store.list_domains(server.id).await {
            Ok(list) => list.into_iter().map(|d| (d.name, d.id)).collect(),
            Err(error) => return render_store_error(Some(&start), error),
        };
    let credentials: std::collections::HashMap<String, u64> =
        match state.store.list_credentials(server.id).await {
            Ok(list) => list.into_iter().map(|c| (c.name, c.id)).collect(),
            Err(error) => return render_store_error(Some(&start), error),
        };

    let mut imported = 0u64;
    let mut failed: Vec<Value> = Vec::new();
    for (index, (item, raw)) in batch.messages.into_iter().zip(raws).enumerate() {
        let raw = match raw {
            Ok(raw) => raw,
            Err(error) => {
                failed.push(json!({ "index": index, "error": error }));
                continue;
            }
        };
        let import = match build_import_message(server.id, item, raw, &domains, &credentials) {
            Ok(import) => import,
            Err(error) => {
                failed.push(json!({ "index": index, "error": error }));
                continue;
            }
        };
        match server_store.import_message(import).await {
            Ok(_) => imported += 1,
            Err(error) => {
                let message = match error {
                    StoreError::Conflict(message) | StoreError::Other(message) => message,
                };
                failed.push(json!({ "index": index, "error": message }));
            }
        }
    }

    created(&start, json!({ "imported": imported, "failed": failed }))
}

// ------------------------------------------------------------ suppressions

fn suppression_json(suppression: &Suppression) -> Value {
    json!({
        "id": suppression.id,
        "type": suppression.suppression_type,
        "address": suppression.address,
        "reason": suppression.reason,
        // null = server-wide; a set id scopes the opt-out to one stream.
        "stream_id": suppression.stream_id,
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
    // Resolve the server's streams so the response can name each
    // suppression's scope (the frontend is on the management surface and
    // cannot reach the messaging-API stream list itself).
    let streams = match &state.server_store {
        Some(server_store) => server_store
            .list_streams(server.id)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let streams_json: Vec<Value> = streams
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.name,
                "permalink": s.permalink,
                "stream_type": s.stream_type,
            })
        })
        .collect();
    from_result(
        &start,
        state.store.list_suppressions(server.id).await,
        |suppressions| {
            let result = paginate(&suppressions, &params);
            ok(
                &start,
                json!({
                    "suppressions": result.items.iter().map(suppression_json).collect::<Vec<_>>(),
                    "streams": streams_json,
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
                // The admin API manages server-wide suppressions; stream-scoped
                // ones are created by the unsubscribe endpoint.
                stream_id: None,
            })
            .await,
        |suppression| {
            created(
                &start,
                json!({ "suppression": suppression_json(&suppression) }),
            )
        },
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
    /// Optional initial password so the account can sign in at
    /// `/api/v2/auth/login` (requires accounts/persistent storage).
    password: Option<String>,
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
    let password = match body.password.filter(|p| !p.is_empty()) {
        None => None,
        Some(password) => {
            if state.auth_store.is_none() {
                return render_validation_error(
                    Some(&start),
                    "Passwords require accounts to be enabled (persistent storage)",
                );
            }
            if (password.len() as u32) < state.config.auth.minimum_password_length {
                return render_validation_error(
                    Some(&start),
                    &format!(
                        "Password must be at least {} characters",
                        state.config.auth.minimum_password_length
                    ),
                );
            }
            match camelmailer_core::auth::hash_password(&password) {
                Ok(digest) => Some(digest),
                Err(error) => return render_store_error(Some(&start), StoreError::Other(error)),
            }
        }
    };
    let user = match state
        .store
        .create_user(NewUser {
            email_address,
            first_name: body.first_name.unwrap_or_default(),
            last_name: body.last_name.unwrap_or_default(),
            admin: body.admin.unwrap_or(false),
        })
        .await
    {
        Ok(user) => user,
        Err(error) => return render_store_error(Some(&start), error),
    };
    if let (Some(digest), Some(auth_store)) = (password, state.auth_store.as_ref()) {
        if let Err(error) = auth_store.set_password_digest(user.id, &digest).await {
            return render_store_error(Some(&start), error);
        }
    }
    created(&start, json!({ "user": user_json(&user) }))
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
