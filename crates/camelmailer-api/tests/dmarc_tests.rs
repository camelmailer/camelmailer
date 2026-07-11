//! DMARC monitoring tests: the domain health check (mock DNS resolver,
//! every traffic-light case), the compliance/report endpoints of the
//! per-server API (aggregation, pagination, tenant scoping) and the
//! inbound-route validation of the internal DMARC target.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_router, build_server_router, ApiState};
use camelmailer_core::{
    AdminStore, CredentialType, MemoryStore, NewCredential, NewDmarcRecord, NewDmarcReport,
    NewOrganization, NewServer, ServerMode, ServerStore, StaticDnsResolver,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const GLOBAL_KEY: &str = "global-admin-key";
const TOKEN_A: &str = "tok-alpha-000000000000";
const TOKEN_B: &str = "tok-beta-0000000000000";

struct Harness {
    admin: Router,
    server: Router,
    store: Arc<MemoryStore>,
    resolver: Arc<StaticDnsResolver>,
    server_a: camelmailer_core::Server,
    server_b: camelmailer_core::Server,
}

/// One org (`acme`) with two servers (`alpha`, `beta`), each with an API
/// token; the admin and per-server routers share the state.
async fn build() -> Harness {
    let store = Arc::new(MemoryStore::new());
    let resolver = Arc::new(StaticDnsResolver::new());

    let org = store
        .create_organization(NewOrganization {
            name: "Acme".into(),
            permalink: "acme".into(),
        })
        .await
        .unwrap();
    let server_a = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "Alpha".into(),
            permalink: "alpha".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let server_b = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "Beta".into(),
            permalink: "beta".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    for (server_id, token) in [(server_a.id, TOKEN_A), (server_b.id, TOKEN_B)] {
        store
            .create_credential_record(NewCredential {
                server_id,
                credential_type: CredentialType::Api,
                name: "api".into(),
                key: Some(token.into()),
            })
            .await
            .unwrap();
    }

    let state = ApiState::full_with_resolver(
        store.clone(),
        Some(store.clone()),
        None,
        Some(GLOBAL_KEY.to_string()),
        camelmailer_config::Config::default(),
        resolver.clone(),
    );
    Harness {
        admin: build_router(state.clone()),
        server: build_server_router(state),
        store,
        resolver,
        server_a,
        server_b,
    }
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    header: (&str, &str),
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header.0, header.1);
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn admin(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    send(app, method, path, ("X-Admin-API-Key", GLOBAL_KEY), body).await
}

async fn server_get(app: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    send(app, "GET", path, ("X-Server-API-Key", token), None).await
}

const BASE: &str = "/api/v2/admin/organizations/acme/servers/alpha";

/// Create a domain on alpha via the API (so it carries its own DKIM key)
/// and return (domain json, expected SPF record, DKIM record name+value).
async fn create_domain(h: &Harness, name: &str) -> Value {
    let (status, body) = admin(
        &h.admin,
        "POST",
        &format!("{BASE}/domains"),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["data"]["domain"].clone()
}

async fn health(h: &Harness, name: &str) -> Value {
    let (status, body) = admin(
        &h.admin,
        "GET",
        &format!("{BASE}/domains/{name}/health"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["data"]["health"].clone()
}

// The default config's SPF source (dns.spf_include).
const SPF_OK: &str = "v=spf1 include:spf.postal.example.com ~all";

// -------------------------------------------------------------- health

#[tokio::test]
async fn health_is_all_green_when_every_record_is_published() {
    let h = build().await;
    let domain = create_domain(&h, "acme.example").await;

    h.resolver.add_txt("acme.example", SPF_OK);
    h.resolver.add_txt(
        domain["dkim_record"]["name"].as_str().unwrap(),
        domain["dkim_record"]["value"].as_str().unwrap(),
    );
    h.resolver.add_txt(
        "_dmarc.acme.example",
        "v=DMARC1; p=quarantine; rua=mailto:dmarc@acme.example",
    );

    let health = health(&h, "acme.example").await;
    assert_eq!(health["checks"]["spf"]["status"], "ok");
    assert_eq!(health["checks"]["dkim"]["status"], "ok");
    assert_eq!(health["checks"]["dmarc"]["status"], "ok");
    assert_eq!(health["overall"], "ok");
    assert_eq!(health["checks"]["dmarc"]["policy"]["p"], "quarantine");
    assert_eq!(health["checks"]["dmarc"]["policy"]["pct"], 100);
    // healthy quarantine policy with no data yet: no escalation suggested
    assert!(health["next_step"]
        .as_str()
        .unwrap()
        .contains("keep monitoring"));
}

#[tokio::test]
async fn health_reports_missing_records_and_recommends_a_starter_policy() {
    let h = build().await;
    create_domain(&h, "acme.example").await;
    // no DNS records published at all

    let health = health(&h, "acme.example").await;
    assert_eq!(health["checks"]["spf"]["status"], "missing");
    assert_eq!(health["checks"]["dkim"]["status"], "missing");
    assert_eq!(health["checks"]["dmarc"]["status"], "missing");
    assert_eq!(health["overall"], "missing");
    // the journey starts with p=none + rua
    let next = health["next_step"].as_str().unwrap();
    assert!(next.contains("_dmarc.acme.example"));
    assert!(next.contains("p=none"));
    assert!(next.contains("rua="));
    // the expected values tell the operator what to publish
    assert_eq!(health["checks"]["spf"]["expected"], SPF_OK);
    assert!(health["checks"]["dkim"]["expected"]
        .as_str()
        .unwrap()
        .starts_with("v=DKIM1; k=rsa; p="));
}

#[tokio::test]
async fn health_flags_spf_problems() {
    let h = build().await;
    let domain = create_domain(&h, "acme.example").await;
    h.resolver.add_txt(
        domain["dkim_record"]["name"].as_str().unwrap(),
        domain["dkim_record"]["value"].as_str().unwrap(),
    );
    h.resolver
        .add_txt("_dmarc.acme.example", "v=DMARC1; p=reject; rua=mailto:d@x");

    // two v=spf1 records → warning
    h.resolver.add_txt("acme.example", SPF_OK);
    h.resolver.add_txt("acme.example", "v=spf1 a -all");
    let result = health(&h, "acme.example").await;
    assert_eq!(result["checks"]["spf"]["status"], "warning");
    assert!(result["checks"]["spf"]["problems"][0]
        .as_str()
        .unwrap()
        .contains("2 v=spf1 records"));
    assert_eq!(result["overall"], "warning");

    // a record without our include and with ?all → both problems named
    let h = build().await;
    create_domain(&h, "acme.example").await;
    h.resolver
        .add_txt("acme.example", "v=spf1 include:other.example ?all");
    let result = health(&h, "acme.example").await;
    assert_eq!(result["checks"]["spf"]["status"], "warning");
    let problems = result["checks"]["spf"]["problems"].as_array().unwrap();
    assert!(problems.iter().any(|p| p
        .as_str()
        .unwrap()
        .contains("include:spf.postal.example.com")));
    assert!(problems
        .iter()
        .any(|p| p.as_str().unwrap().contains("?all")));

    // a record without any all mechanism
    let h = build().await;
    create_domain(&h, "acme.example").await;
    h.resolver
        .add_txt("acme.example", "v=spf1 include:spf.postal.example.com");
    let result = health(&h, "acme.example").await;
    assert_eq!(result["checks"]["spf"]["status"], "warning");
    assert!(result["checks"]["spf"]["problems"][0]
        .as_str()
        .unwrap()
        .contains("no all mechanism"));
}

#[tokio::test]
async fn health_flags_a_dkim_key_mismatch() {
    let h = build().await;
    let domain = create_domain(&h, "acme.example").await;
    h.resolver.add_txt(
        domain["dkim_record"]["name"].as_str().unwrap(),
        "v=DKIM1; k=rsa; p=AAAAB3NzaC1yc2EAAAADAQABAAABAQ",
    );
    let result = health(&h, "acme.example").await;
    assert_eq!(result["checks"]["dkim"]["status"], "warning");
    assert!(result["checks"]["dkim"]["problems"][0]
        .as_str()
        .unwrap()
        .contains("does not match"));
}

#[tokio::test]
async fn health_walks_the_policy_journey_from_none_to_quarantine() {
    let h = build().await;
    create_domain(&h, "acme.example").await;
    h.resolver.add_txt("acme.example", SPF_OK);
    h.resolver.add_txt(
        "_dmarc.acme.example",
        "v=DMARC1; p=none; rua=mailto:dmarc@acme.example",
    );

    // p=none with rua and no data yet: keep collecting
    let result = health(&h, "acme.example").await;
    assert_eq!(result["checks"]["dmarc"]["status"], "ok");
    assert_eq!(result["checks"]["dmarc"]["policy"]["p"], "none");
    assert!(result["next_step"]
        .as_str()
        .unwrap()
        .contains("Keep collecting"));

    // seed high compliance (recent window, enough volume, all aligned)
    let now = chrono::Utc::now();
    ServerStore::store_dmarc_report(
        h.store.as_ref(),
        NewDmarcReport {
            server_id: h.server_a.id,
            domain: "acme.example".into(),
            org_name: Some("google.com".into()),
            org_email: None,
            report_id: "r-1".into(),
            date_range_begin: now - chrono::Duration::days(1),
            date_range_end: now,
            records: vec![NewDmarcRecord {
                source_ip: "203.0.113.10".into(),
                count: 50,
                disposition: "none".into(),
                dkim_result: Some("pass".into()),
                spf_result: Some("pass".into()),
                dkim_aligned: true,
                spf_aligned: true,
                header_from: Some("acme.example".into()),
                envelope_from: None,
            }],
        },
    )
    .await
    .unwrap();

    let result = health(&h, "acme.example").await;
    assert_eq!(result["compliance"]["total"], 50);
    assert_eq!(result["compliance"]["pass_rate"], 1.0);
    assert!(result["next_step"]
        .as_str()
        .unwrap()
        .contains("p=quarantine"));
}

#[tokio::test]
async fn health_recommends_rua_when_the_policy_has_none() {
    let h = build().await;
    create_domain(&h, "acme.example").await;
    h.resolver
        .add_txt("_dmarc.acme.example", "v=DMARC1; p=none");

    // an internal DMARC route exists → its address is suggested
    let (status, _) = admin(
        &h.admin,
        "POST",
        &format!("{BASE}/routes"),
        Some(json!({
            "name": "dmarc",
            "domain": "acme.example",
            "endpoint_url": "internal://dmarc-reports",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let result = health(&h, "acme.example").await;
    assert_eq!(result["checks"]["dmarc"]["status"], "warning");
    assert_eq!(result["rua_address"], "dmarc@acme.example");
    let next = result["next_step"].as_str().unwrap();
    assert!(next.contains("rua=mailto:dmarc@acme.example"));
}

#[tokio::test]
async fn health_turns_dns_failures_into_warnings() {
    let h = build().await;
    create_domain(&h, "acme.example").await;
    h.resolver.fail_with("SERVFAIL");
    let result = health(&h, "acme.example").await;
    for check in ["spf", "dkim", "dmarc"] {
        assert_eq!(result["checks"][check]["status"], "warning");
        assert!(result["checks"][check]["problems"][0]
            .as_str()
            .unwrap()
            .contains("SERVFAIL"));
    }
    assert_eq!(result["overall"], "warning");
}

#[tokio::test]
async fn health_of_an_unknown_domain_is_404() {
    let h = build().await;
    let (status, body) = admin(
        &h.admin,
        "GET",
        &format!("{BASE}/domains/nope.example/health"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotFound");
}

// ---------------------------------------------------- route validation

#[tokio::test]
async fn routes_accept_http_and_the_internal_dmarc_target_only() {
    let h = build().await;

    for (endpoint, expected) in [
        ("https://app.acme.example/inbound", StatusCode::CREATED),
        ("http://app.acme.example/inbound", StatusCode::CREATED),
        ("internal://dmarc-reports", StatusCode::CREATED),
        ("internal://other", StatusCode::UNPROCESSABLE_ENTITY),
        ("ftp://files.acme.example", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let (status, body) = admin(
            &h.admin,
            "POST",
            &format!("{BASE}/routes"),
            Some(json!({ "name": format!("r-{}", body_safe(endpoint)), "endpoint_url": endpoint })),
        )
        .await;
        assert_eq!(status, expected, "endpoint {endpoint}");
        if expected == StatusCode::UNPROCESSABLE_ENTITY {
            assert_eq!(body["error"]["code"], "ValidationError");
            assert!(body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("internal://dmarc-reports"));
        }
    }
}

fn body_safe(endpoint: &str) -> String {
    endpoint
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

// ------------------------------------------- compliance summary + reports

/// Seed alpha with two reports (different domains and ranges).
async fn seed_reports(h: &Harness) -> (i64, i64) {
    let now = chrono::Utc::now();
    let first = ServerStore::store_dmarc_report(
        h.store.as_ref(),
        NewDmarcReport {
            server_id: h.server_a.id,
            domain: "acme.example".into(),
            org_name: Some("google.com".into()),
            org_email: Some("noreply@google.com".into()),
            report_id: "g-1".into(),
            date_range_begin: now - chrono::Duration::days(3),
            date_range_end: now - chrono::Duration::days(2),
            records: vec![
                NewDmarcRecord {
                    source_ip: "203.0.113.10".into(),
                    count: 8,
                    disposition: "none".into(),
                    dkim_result: Some("pass".into()),
                    spf_result: Some("pass".into()),
                    dkim_aligned: true,
                    spf_aligned: true,
                    header_from: Some("acme.example".into()),
                    envelope_from: None,
                },
                NewDmarcRecord {
                    source_ip: "198.51.100.7".into(),
                    count: 2,
                    disposition: "quarantine".into(),
                    dkim_result: Some("fail".into()),
                    spf_result: Some("softfail".into()),
                    dkim_aligned: false,
                    spf_aligned: false,
                    header_from: Some("acme.example".into()),
                    envelope_from: Some("spoof.example".into()),
                },
            ],
        },
    )
    .await
    .unwrap();
    let second = ServerStore::store_dmarc_report(
        h.store.as_ref(),
        NewDmarcReport {
            server_id: h.server_a.id,
            domain: "other.example".into(),
            org_name: Some("Outlook.com".into()),
            org_email: None,
            report_id: "m-1".into(),
            date_range_begin: now - chrono::Duration::days(1),
            date_range_end: now,
            records: vec![NewDmarcRecord {
                source_ip: "203.0.113.10".into(),
                count: 5,
                disposition: "none".into(),
                dkim_result: Some("pass".into()),
                spf_result: Some("fail".into()),
                dkim_aligned: true,
                spf_aligned: false,
                header_from: Some("other.example".into()),
                envelope_from: None,
            }],
        },
    )
    .await
    .unwrap();
    (first.id, second.id)
}

#[tokio::test]
async fn dmarc_summary_aggregates_records_by_source_and_disposition() {
    let h = build().await;
    seed_reports(&h).await;

    let (status, body) = server_get(&h.server, "/api/v2/server/dmarc/summary", TOKEN_A).await;
    assert_eq!(status, StatusCode::OK);
    let summary = &body["data"]["summary"];
    assert_eq!(summary["total"], 15);
    assert_eq!(summary["pass"], 8);
    assert_eq!(summary["fail"], 7);
    assert!((summary["pass_rate"].as_f64().unwrap() - 0.533).abs() < 1e-9);
    assert_eq!(summary["by_disposition"]["none"], 13);
    assert_eq!(summary["by_disposition"]["quarantine"], 2);

    // top source first, with alignment percentages
    let sources = summary["by_source"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["source_ip"], "203.0.113.10");
    assert_eq!(sources[0]["count"], 13);
    assert_eq!(sources[0]["dkim_aligned_pct"], 100.0);
    assert!((sources[0]["spf_aligned_pct"].as_f64().unwrap() - 61.5).abs() < 1e-9);
    assert_eq!(sources[1]["disposition_counts"]["quarantine"], 2);

    // domain filter narrows the data
    let (_, body) = server_get(
        &h.server,
        "/api/v2/server/dmarc/summary?domain=acme.example",
        TOKEN_A,
    )
    .await;
    assert_eq!(body["data"]["summary"]["total"], 10);

    // a time window before the reports finds nothing (Z-suffixed
    // timestamps: a literal + in a query string would decode as a space)
    let ancient = (chrono::Utc::now() - chrono::Duration::days(30))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let old = (chrono::Utc::now() - chrono::Duration::days(20))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (_, body) = server_get(
        &h.server,
        &format!("/api/v2/server/dmarc/summary?from={ancient}&to={old}"),
        TOKEN_A,
    )
    .await;
    assert_eq!(body["data"]["summary"]["total"], 0);
}

#[tokio::test]
async fn dmarc_reports_list_paginates_newest_first() {
    let h = build().await;
    let (first_id, second_id) = seed_reports(&h).await;

    let (status, body) = server_get(&h.server, "/api/v2/server/dmarc/reports", TOKEN_A).await;
    assert_eq!(status, StatusCode::OK);
    let reports = body["data"]["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 2);
    // newest date range first
    assert_eq!(reports[0]["id"], second_id);
    assert_eq!(reports[0]["org_name"], "Outlook.com");
    assert_eq!(reports[0]["record_count"], 1);
    assert_eq!(reports[1]["id"], first_id);
    assert_eq!(reports[1]["record_count"], 2);

    // pagination
    let (_, body) = server_get(
        &h.server,
        "/api/v2/server/dmarc/reports?page=2&per_page=1",
        TOKEN_A,
    )
    .await;
    let reports = body["data"]["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["id"], first_id);
    assert_eq!(body["data"]["pagination"]["total"], 2);
    assert_eq!(body["data"]["pagination"]["total_pages"], 2);

    // domain filter
    let (_, body) = server_get(
        &h.server,
        "/api/v2/server/dmarc/reports?domain=other.example",
        TOKEN_A,
    )
    .await;
    assert_eq!(body["data"]["reports"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn dmarc_report_show_includes_the_records() {
    let h = build().await;
    let (first_id, _) = seed_reports(&h).await;

    let (status, body) = server_get(
        &h.server,
        &format!("/api/v2/server/dmarc/reports/{first_id}"),
        TOKEN_A,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["report"]["report_id"], "g-1");
    let records = body["data"]["records"].as_array().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["source_ip"], "203.0.113.10");
    assert_eq!(records[0]["count"], 8);
    assert_eq!(records[0]["dkim_aligned"], true);
    assert_eq!(records[1]["disposition"], "quarantine");
    assert_eq!(records[1]["envelope_from"], "spoof.example");
}

#[tokio::test]
async fn a_foreign_server_key_sees_no_dmarc_data() {
    let h = build().await;
    let (first_id, _) = seed_reports(&h).await;

    let (_, body) = server_get(&h.server, "/api/v2/server/dmarc/summary", TOKEN_B).await;
    assert_eq!(body["data"]["summary"]["total"], 0);

    let (_, body) = server_get(&h.server, "/api/v2/server/dmarc/reports", TOKEN_B).await;
    assert_eq!(body["data"]["reports"].as_array().unwrap().len(), 0);

    let (status, body) = server_get(
        &h.server,
        &format!("/api/v2/server/dmarc/reports/{first_id}"),
        TOKEN_B,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotFound");
}

#[tokio::test]
async fn dmarc_endpoints_require_a_server_key() {
    let h = build().await;
    let (status, _) = server_get(&h.server, "/api/v2/server/dmarc/summary", "wrong").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn seeded_domains_do_not_interfere_with_health_of_other_servers() {
    let h = build().await;
    // a domain of beta with the same name is invisible under alpha's path
    h.store
        .create_server_domain(h.server_b.id, "beta-only.example", None)
        .await
        .unwrap();
    let (status, _) = admin(
        &h.admin,
        "GET",
        &format!("{BASE}/domains/beta-only.example/health"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
