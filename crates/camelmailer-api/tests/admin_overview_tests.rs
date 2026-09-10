//! Instance-wide oversight: `GET /api/v2/admin/overview` and
//! `GET /api/v2/admin/organizations/{permalink}/overview`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, build_router, ApiState};
use camelmailer_core::auth;
use camelmailer_core::{
    AdminStore, AuthStore, Id, MemoryStore, MessageScope, NewOrganization, NewServer, NewUser,
    QueuedMessage, Role, Server, ServerMode,
};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::OnceLock;
use tower::ServiceExt;

const PASSWORD: &str = "correct-horse-battery";

fn password_digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| auth::hash_password(PASSWORD).unwrap())
}

struct Harness {
    app: Router,
    store: Arc<MemoryStore>,
}

/// The overview reads configuration, accounts and per-tenant traffic, so
/// all three storage facets are the same [`MemoryStore`].
async fn harness() -> Harness {
    harness_with_statistics(true).await
}

async fn harness_with_statistics(statistics: bool) -> Harness {
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::full_with_resolver(
        store.clone(),
        statistics.then(|| store.clone() as Arc<dyn camelmailer_core::ServerStore>),
        Some(store.clone()),
        None,
        camelmailer_config::Config::default(),
        Arc::new(camelmailer_core::StaticDnsResolver::new()),
    );
    let app = build_router(state.clone()).merge(build_auth_router(state));
    Harness { app, store }
}

impl Harness {
    async fn user(&self, email: &str, admin: bool) -> camelmailer_core::User {
        let user = self
            .store
            .create_user(NewUser {
                email_address: email.into(),
                first_name: "Test".into(),
                last_name: "User".into(),
                admin,
            })
            .await
            .unwrap();
        self.store
            .set_password_digest(user.id, password_digest())
            .await
            .unwrap();
        user
    }

    async fn org(&self, name: &str) -> camelmailer_core::Organization {
        self.store
            .create_organization(NewOrganization {
                name: name.into(),
                permalink: name.to_lowercase(),
            })
            .await
            .unwrap()
    }

    async fn member(&self, org_id: Id, email: &str, role: Role) -> camelmailer_core::User {
        let user = self.user(email, false).await;
        self.store
            .upsert_membership(org_id, user.id, role)
            .await
            .unwrap();
        user
    }

    async fn server(&self, org_id: Id, name: &str) -> Server {
        self.store
            .create_server(NewServer {
                organization_id: org_id,
                name: name.into(),
                permalink: name.to_lowercase(),
                mode: ServerMode::Live,
            })
            .await
            .unwrap()
    }

    /// One stored message, aged by `hours_ago` and left in `status`.
    fn message(&self, server_id: Id, scope: MessageScope, hours_ago: i64, status: &str) -> i64 {
        let id = self
            .store
            .insert_message_record(QueuedMessage {
                server_id,
                rcpt_to: "rcpt@dest.example".into(),
                mail_from: "sender@src.example".into(),
                raw_message: b"Subject: hi\r\n\r\nbody".to_vec(),
                received_with_ssl: false,
                scope,
                bounce: false,
                domain_id: None,
                credential_id: None,
                route_id: None,
                tag: None,
                metadata: None,
                stream_id: None,
            })
            .id;
        self.store
            .set_message_created_at(id, Utc::now() - Duration::hours(hours_ago));
        self.store.set_message_status(id, status);
        id
    }

    async fn login(&self, email: &str) -> String {
        let (status, body) = self
            .request(
                "POST",
                "/api/v2/auth/login",
                None,
                Some(json!({ "email_address": email, "password": PASSWORD })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "login failed: {body}");
        body["data"]["session_token"].as_str().unwrap().to_string()
    }

    async fn request(
        &self,
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
        let response = self
            .app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    async fn get(&self, path: &str, bearer: &str) -> (StatusCode, Value) {
        self.request("GET", path, Some(bearer), None).await
    }
}

/// Two organizations with contrasting traffic, plus a global admin to read
/// the overview with. Acme sends; Quiet exists and never has.
async fn instance_with_traffic() -> (Harness, String) {
    let h = harness().await;
    let admin = h.user("root@instance.test", true).await;

    let acme = h.org("Acme").await;
    h.member(acme.id, "owner@acme.test", Role::Owner).await;
    h.member(acme.id, "dev@acme.test", Role::Member).await;
    let alpha = h.server(acme.id, "Alpha").await;
    let beta = h.server(acme.id, "Beta").await;

    // Alpha: two sends in the last hour (one of them held), one bounce
    // three days back, one delivery 20 days back.
    h.message(alpha.id, MessageScope::Outgoing, 1, "Sent");
    h.message(alpha.id, MessageScope::Outgoing, 1, "Held");
    h.message(alpha.id, MessageScope::Outgoing, 72, "Bounced");
    h.message(alpha.id, MessageScope::Outgoing, 480, "Sent");
    // Alpha also received one message today.
    h.message(alpha.id, MessageScope::Incoming, 2, "Sent");
    // Beta: one hard failure last week, and one message older than every
    // window, which must not be counted anywhere.
    h.message(beta.id, MessageScope::Outgoing, 100, "HardFail");
    h.message(beta.id, MessageScope::Outgoing, 24 * 45, "Sent");

    let quiet = h.org("Quiet").await;
    h.member(quiet.id, "owner@quiet.test", Role::Owner).await;
    h.server(quiet.id, "Idle").await;

    let token = h.login(&admin.email_address).await;
    (h, token)
}

#[tokio::test]
async fn the_overview_rolls_every_organization_up_by_window() {
    let (h, token) = instance_with_traffic().await;
    let (status, body) = h.get("/api/v2/admin/overview", &token).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");

    let overview = &body["data"]["overview"];
    assert!(overview["generated_at"].is_string());
    assert!(overview["windows"]["day"].is_string());

    // Instance totals count every organization, and only messages inside
    // the widest window: the 45-day-old message is excluded.
    let instance = &overview["instance"];
    assert_eq!(instance["organizations"], 2);
    assert_eq!(instance["servers"], 3);
    assert_eq!(instance["suspended_servers"], 0);
    assert_eq!(instance["users"], 4, "one admin plus three members");
    assert_eq!(instance["month"]["total"], 6);
    assert_eq!(instance["month"]["outgoing"], 5);
    assert_eq!(instance["month"]["incoming"], 1);
    assert_eq!(instance["month"]["bounced"], 1);
    assert_eq!(instance["month"]["held"], 1);
    assert_eq!(instance["month"]["failed"], 1);
    assert_eq!(instance["week"]["total"], 5, "the 20-day send drops out");
    assert_eq!(instance["day"]["total"], 3, "two sends plus one inbound");

    // Busiest organization first.
    let organizations = overview["organizations"].as_array().expect("orgs array");
    assert_eq!(organizations.len(), 2);
    assert_eq!(organizations[0]["permalink"], "acme");
    assert_eq!(organizations[1]["permalink"], "quiet");

    let acme = &organizations[0];
    assert_eq!(acme["name"], "Acme");
    assert_eq!(acme["servers"], 2);
    assert_eq!(acme["members"], 2);
    assert_eq!(acme["month"]["total"], 6);
    assert_eq!(acme["day"]["outgoing"], 2);
    assert_eq!(acme["day"]["held"], 1);
    assert!(acme["last_message_at"].is_string());
    assert!(acme["first_message_at"].is_string());

    // An organization that never sent reports zeros and no activity, not
    // an absent row.
    let quiet = &organizations[1];
    assert_eq!(quiet["servers"], 1);
    assert_eq!(quiet["members"], 1);
    assert_eq!(quiet["month"]["total"], 0);
    assert!(quiet["last_message_at"].is_null());
    assert!(quiet["first_message_at"].is_null());
}

#[tokio::test]
async fn the_organization_overview_breaks_traffic_down_by_server() {
    let (h, token) = instance_with_traffic().await;
    let (status, body) = h
        .get("/api/v2/admin/organizations/acme/overview", &token)
        .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");

    let overview = &body["data"]["overview"];
    assert_eq!(overview["organization"]["permalink"], "acme");
    assert_eq!(overview["organization"]["month"]["total"], 6);

    let servers = overview["servers"].as_array().expect("servers array");
    assert_eq!(servers.len(), 2);

    let alpha = servers
        .iter()
        .find(|s| s["permalink"] == "alpha")
        .expect("alpha");
    assert_eq!(alpha["name"], "Alpha");
    assert_eq!(alpha["mode"], "Live");
    assert_eq!(alpha["suspended"], false);
    assert_eq!(alpha["month"]["total"], 5);
    assert_eq!(alpha["month"]["incoming"], 1);
    assert_eq!(alpha["month"]["bounced"], 1);
    assert_eq!(alpha["day"]["total"], 3);

    let beta = servers
        .iter()
        .find(|s| s["permalink"] == "beta")
        .expect("beta");
    assert_eq!(beta["month"]["total"], 1, "the 45-day message is outside");
    assert_eq!(beta["month"]["failed"], 1);
    assert_eq!(beta["day"]["total"], 0);
}

#[tokio::test]
async fn a_suspended_server_is_counted_on_both_levels() {
    let h = harness().await;
    let admin = h.user("root@instance.test", true).await;
    let org = h.org("Acme").await;
    let server = h.server(org.id, "Alpha").await;
    h.store
        .update_server(Server {
            suspended: true,
            suspension_reason: Some("outbound spam".into()),
            ..server
        })
        .await
        .unwrap();
    let token = h.login(&admin.email_address).await;

    let (_, body) = h.get("/api/v2/admin/overview", &token).await;
    assert_eq!(body["data"]["overview"]["instance"]["suspended_servers"], 1);
    assert_eq!(
        body["data"]["overview"]["organizations"][0]["suspended_servers"],
        1
    );

    let (_, body) = h
        .get("/api/v2/admin/organizations/acme/overview", &token)
        .await;
    let alpha = &body["data"]["overview"]["servers"][0];
    assert_eq!(alpha["suspended"], true);
    assert_eq!(alpha["suspension_reason"], "outbound spam");
}

#[tokio::test]
async fn the_instance_overview_is_reserved_for_global_administrators() {
    let (h, _) = instance_with_traffic().await;
    let owner = h.login("owner@acme.test").await;
    let (status, body) = h.get("/api/v2/admin/overview", &owner).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "unexpected body: {body}");
    assert_eq!(body["error"]["code"], "Forbidden");
}

#[tokio::test]
async fn an_organization_overview_is_readable_by_its_own_members() {
    let (h, _) = instance_with_traffic().await;
    let owner = h.login("owner@acme.test").await;
    let (status, body) = h
        .get("/api/v2/admin/organizations/acme/overview", &owner)
        .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert_eq!(
        body["data"]["overview"]["organization"]["permalink"],
        "acme"
    );
}

#[tokio::test]
async fn a_foreign_organization_overview_is_not_found() {
    let (h, _) = instance_with_traffic().await;
    let outsider = h.login("owner@quiet.test").await;
    let (status, body) = h
        .get("/api/v2/admin/organizations/acme/overview", &outsider)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unexpected body: {body}");
}

#[tokio::test]
async fn an_unknown_organization_has_no_overview() {
    let (h, token) = instance_with_traffic().await;
    let (status, _) = h
        .get("/api/v2/admin/organizations/nowhere/overview", &token)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn missing_statistics_storage_is_reported_rather_than_answered_with_zeros() {
    let h = harness_with_statistics(false).await;
    let admin = h.user("root@instance.test", true).await;
    let org = h.org("Acme").await;
    h.server(org.id, "Alpha").await;
    let token = h.login(&admin.email_address).await;

    for path in [
        "/api/v2/admin/overview",
        "/api/v2/admin/organizations/acme/overview",
    ] {
        let (status, body) = h.get(path, &token).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {body}");
        assert_eq!(body["error"]["code"], "StorageUnavailable");
    }
}
