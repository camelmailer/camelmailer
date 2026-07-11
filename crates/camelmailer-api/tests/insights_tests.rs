//! Deliverability insights (`GET /api/v2/server/messages/{id}/insights`):
//! every rule of the catalog in its ok and warning shape, plus the
//! DNS-failure path (the DMARC check is skipped, the request survives).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_server_router, ApiState};
use camelmailer_core::mime::{Address, BuildParams};
use camelmailer_core::{
    AdminStore, CredentialType, Domain, DomainOwner, MemoryStore, MessageScope, NewCredential,
    NewOrganization, NewServer, QueuedMessage, ServerMode, StaticDnsResolver,
};
use http_body_util::BodyExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

struct Setup {
    app: Router,
    store: Arc<MemoryStore>,
    resolver: Arc<StaticDnsResolver>,
    token: String,
    server_id: camelmailer_core::Id,
}

async fn build() -> Setup {
    let store = Arc::new(MemoryStore::new());
    let org = store
        .create_organization(NewOrganization {
            name: "Org".into(),
            permalink: "org".into(),
        })
        .await
        .unwrap();
    let server = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "Alpha".into(),
            permalink: "alpha".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let token = "tok-alpha-000000000000".to_string();
    store
        .create_credential_record(NewCredential {
            server_id: server.id,
            credential_type: CredentialType::Api,
            name: "api".into(),
            key: Some(token.clone()),
        })
        .await
        .unwrap();
    // example.com is a verified sending domain with its own DKIM key
    store.insert_domain(Domain {
        id: store.next_id(),
        uuid: "d-1".into(),
        owner: DomainOwner::Server(server.id),
        name: "example.com".into(),
        verified: true,
        verification_token: "vt".into(),
        dkim_private_key: Some("-- test key --".into()),
    });

    let resolver = Arc::new(StaticDnsResolver::new());
    let state = ApiState::full_with_resolver(
        store.clone(),
        Some(store.clone()),
        None,
        None,
        camelmailer_config::Config::default(),
        resolver.clone(),
    );
    Setup {
        app: build_server_router(state),
        store,
        resolver,
        token,
        server_id: server.id,
    }
}

fn seed_message(setup: &Setup, params: &BuildParams) -> i64 {
    let raw = camelmailer_core::mime::build_message(params);
    setup
        .store
        .insert_message_record(QueuedMessage {
            server_id: setup.server_id,
            rcpt_to: "rcpt@dest.example".into(),
            mail_from: params.from.email.clone(),
            raw_message: raw,
            received_with_ssl: false,
            scope: MessageScope::Outgoing,
            bounce: false,
            domain_id: None,
            credential_id: None,
            route_id: None,
            tag: None,
            metadata: None,
            stream_id: None,
        })
        .id
}

async fn insights(setup: &Setup, message_id: i64) -> (StatusCode, Value) {
    let response = setup
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v2/server/messages/{message_id}/insights"))
                .header("X-Server-API-Key", &setup.token)
                .body(Body::empty())
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

/// The checks array as an id → (status, detail) map.
fn by_id(body: &Value) -> HashMap<String, (String, String)> {
    body["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| {
            (
                check["id"].as_str().unwrap().to_string(),
                (
                    check["status"].as_str().unwrap().to_string(),
                    check["detail"].as_str().unwrap().to_string(),
                ),
            )
        })
        .collect()
}

fn good_params() -> BuildParams {
    BuildParams {
        from: Address::new("hello@example.com"),
        to: vec![Address::new("rcpt@dest.example")],
        subject: "Your invoice for March".into(),
        html_body: Some(
            r#"<p><a href="https://example.com/invoice">Invoice</a>
               <a href="https://docs.example.com/help">Help</a>
               <img src="https://cdn.example.com/logo.png"></p>"#
                .into(),
        ),
        text_body: Some("Your invoice: https://example.com/invoice".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_clean_message_passes_every_check() {
    let setup = build().await;
    setup.resolver.add_txt(
        "_dmarc.example.com",
        "v=DMARC1; p=none; rua=mailto:d@example.com",
    );
    let id = seed_message(&setup, &good_params());
    let (status, body) = insights(&setup, id).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["generated_at"].is_string());

    let checks = by_id(&body);
    assert_eq!(checks.len(), 9);
    for id in [
        "plain_text",
        "subject",
        "from_address",
        "links",
        "images",
        "size",
        "sending_domain",
        "dmarc",
        "dkim",
    ] {
        assert_eq!(checks[id].0, "ok", "check {id}: {:?}", checks[id]);
    }
}

#[tokio::test]
async fn missing_text_subject_issues_and_noreply_from_warn() {
    let setup = build().await;
    setup
        .resolver
        .add_txt("_dmarc.example.com", "v=DMARC1; p=none");
    let id = seed_message(
        &setup,
        &BuildParams {
            from: Address::new("no-reply@example.com"),
            to: vec![Address::new("rcpt@dest.example")],
            // longer than 78 characters
            subject: "x".repeat(90),
            html_body: Some("<p>html only</p>".into()),
            text_body: None,
            ..Default::default()
        },
    );
    let (_, body) = insights(&setup, id).await;
    let checks = by_id(&body);
    assert_eq!(checks["plain_text"].0, "warning");
    assert_eq!(checks["subject"].0, "warning");
    // header folding may add whitespace on the wire; the exact count can
    // therefore drift by a few characters — the wording is what matters
    assert!(checks["subject"].1.contains("characters long"));
    assert_eq!(checks["from_address"].0, "warning");
    // the rest of the catalog is unaffected
    assert_eq!(checks["links"].0, "ok");
    assert_eq!(checks["dmarc"].0, "ok");
}

#[tokio::test]
async fn an_empty_subject_warns_too() {
    let setup = build().await;
    let id = seed_message(
        &setup,
        &BuildParams {
            from: Address::new("hello@example.com"),
            to: vec![Address::new("rcpt@dest.example")],
            subject: String::new(),
            text_body: Some("hi".into()),
            ..Default::default()
        },
    );
    let (_, body) = insights(&setup, id).await;
    assert_eq!(by_id(&body)["subject"].0, "warning");
}

#[tokio::test]
async fn shorteners_foreign_links_and_foreign_images_warn() {
    let setup = build().await;
    let id = seed_message(
        &setup,
        &BuildParams {
            from: Address::new("hello@example.com"),
            to: vec![Address::new("rcpt@dest.example")],
            subject: "Hi".into(),
            html_body: Some(
                r#"<a href="https://bit.ly/xyz">short</a>
                   <img src="https://cdn.thirdparty.example/i.png">"#
                    .into(),
            ),
            text_body: Some("hi".into()),
            ..Default::default()
        },
    );
    let (_, body) = insights(&setup, id).await;
    let checks = by_id(&body);
    assert_eq!(checks["links"].0, "warning");
    assert!(
        checks["links"].1.contains("bit.ly"),
        "{}",
        checks["links"].1
    );
    assert_eq!(checks["images"].0, "warning");
    assert!(
        checks["images"].1.contains("cdn.thirdparty.example"),
        "{}",
        checks["images"].1
    );

    // foreign (non-shortener) links warn with the alignment wording
    let id = seed_message(
        &setup,
        &BuildParams {
            from: Address::new("hello@example.com"),
            to: vec![Address::new("rcpt@dest.example")],
            subject: "Hi".into(),
            html_body: Some(r#"<a href="https://other.example/x">x</a>"#.into()),
            text_body: Some("hi".into()),
            ..Default::default()
        },
    );
    let (_, body) = insights(&setup, id).await;
    let checks = by_id(&body);
    assert_eq!(checks["links"].0, "warning");
    assert!(checks["links"].1.contains("other.example"));
}

#[tokio::test]
async fn oversized_bodies_warn() {
    let setup = build().await;
    let id = seed_message(
        &setup,
        &BuildParams {
            from: Address::new("hello@example.com"),
            to: vec![Address::new("rcpt@dest.example")],
            subject: "Big".into(),
            text_body: Some("y".repeat(150 * 1024)),
            ..Default::default()
        },
    );
    let (_, body) = insights(&setup, id).await;
    assert_eq!(by_id(&body)["size"].0, "warning");
}

#[tokio::test]
async fn unverified_from_domains_fail_domain_dmarc_and_dkim_checks() {
    let setup = build().await;
    // no domain record, no DMARC TXT, no installation key
    let id = seed_message(
        &setup,
        &BuildParams {
            from: Address::new("hello@unverified.example"),
            to: vec![Address::new("rcpt@dest.example")],
            subject: "Hi".into(),
            text_body: Some("hi".into()),
            ..Default::default()
        },
    );
    let (_, body) = insights(&setup, id).await;
    let checks = by_id(&body);
    assert_eq!(checks["sending_domain"].0, "warning");
    assert_eq!(checks["dmarc"].0, "warning");
    assert!(checks["dmarc"].1.contains("_dmarc.unverified.example"));
    assert_eq!(checks["dkim"].0, "warning");
}

#[tokio::test]
async fn dns_failures_skip_the_dmarc_check() {
    let setup = build().await;
    setup.resolver.fail_with("SERVFAIL");
    let id = seed_message(&setup, &good_params());
    let (status, body) = insights(&setup, id).await;
    // the DNS failure never kills the request
    assert_eq!(status, StatusCode::OK);
    let checks = by_id(&body);
    // the DMARC check cannot be evaluated — it is skipped, not failed
    assert!(!checks.contains_key("dmarc"));
    // every other check still evaluated
    assert_eq!(checks.len(), 8);
    assert_eq!(checks["plain_text"].0, "ok");
}

#[tokio::test]
async fn unknown_messages_are_not_found() {
    let setup = build().await;
    let (status, body) = insights(&setup, 424242).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotFound");
}
