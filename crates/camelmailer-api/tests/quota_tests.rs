//! Organization quota, suspension, monthly usage and lifecycle events:
//! the HTTP send gate across several servers, the admin endpoints with their
//! RBAC, and the outbox entries the web server writes.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_app, ApiState};
use camelmailer_core::{
    AdminStore, AuthStore, CredentialType, DomainOwner, MemoryStore, NewCredential,
    NewOrganization, NewServer, NewUser, Role, ServerMode,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;

const PASSWORD: &str = "correct-horse-battery";
const ADMIN_KEY: &str = "machine-key-1";

fn password_digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| camelmailer_core::auth::hash_password(PASSWORD).unwrap())
}

struct Harness {
    app: Router,
    store: Arc<MemoryStore>,
    org: camelmailer_core::Organization,
    /// One API key per server: alpha, beta.
    keys: [String; 2],
}

async fn harness_with(lifecycle: bool) -> Harness {
    let store = Arc::new(MemoryStore::new());
    let mut config = camelmailer_config::Config::default();
    config.auth.allow_registration = true;
    if lifecycle {
        config.lifecycle_webhooks.urls = vec!["https://hooks.example.com/cm".into()];
        config.lifecycle_webhooks.secret = Some("whsec".into());
    }
    config.validate().unwrap();
    let state = ApiState::full(
        store.clone(),
        Some(store.clone()),
        Some(store.clone()),
        Some(ADMIN_KEY.into()),
        config,
    );
    let org = store
        .create_organization(NewOrganization {
            name: "Acme".into(),
            permalink: "acme".into(),
        })
        .await
        .unwrap();
    // An organization-owned verified domain, so every server may send.
    store.insert_domain(camelmailer_core::Domain {
        id: store.next_id(),
        uuid: "d".into(),
        owner: DomainOwner::Organization(org.id),
        name: "acme.example".into(),
        verified: true,
        verification_token: "v".into(),
        check_dmarc: true,
        check_spf: true,
        dkim_private_key: None,
    });
    let mut keys = Vec::new();
    for name in ["alpha", "beta"] {
        let server = store
            .create_server(NewServer {
                organization_id: org.id,
                name: name.into(),
                permalink: name.into(),
                mode: ServerMode::Live,
            })
            .await
            .unwrap();
        let key = format!("key-{name}-00000000000000");
        store
            .create_credential_record(NewCredential {
                server_id: server.id,
                credential_type: CredentialType::Api,
                name: "api".into(),
                key: Some(key.clone()),
            })
            .await
            .unwrap();
        keys.push(key);
    }
    Harness {
        app: build_app(state, None),
        store,
        org,
        keys: [keys[0].clone(), keys[1].clone()],
    }
}

async fn harness() -> Harness {
    harness_with(false).await
}

impl Harness {
    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
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
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn admin(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.request(method, path, &[("X-Admin-API-Key", ADMIN_KEY)], body)
            .await
    }

    async fn send(&self, server: usize, recipients: &[&str]) -> (StatusCode, Value) {
        self.request(
            "POST",
            "/api/v2/server/messages",
            &[("X-Server-API-Key", &self.keys[server])],
            Some(json!({
                "from": "app@acme.example",
                "to": recipients,
                "subject": "Hi",
                "text_body": "x",
            })),
        )
        .await
    }

    async fn set_limit(&self, limit: Value) {
        let (status, body) = self
            .admin(
                "PATCH",
                "/api/v2/admin/organizations/acme/limits",
                Some(json!({ "monthly_send_limit": limit })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    async fn member(&self, email: &str, role: Role) -> String {
        let user = self
            .store
            .create_user(NewUser {
                email_address: email.into(),
                first_name: "T".into(),
                last_name: "U".into(),
                admin: false,
            })
            .await
            .unwrap();
        self.store
            .set_password_digest(user.id, password_digest())
            .await
            .unwrap();
        self.store
            .upsert_membership(self.org.id, user.id, role)
            .await
            .unwrap();
        let (status, body) = self
            .request(
                "POST",
                "/api/v2/auth/login",
                &[],
                Some(json!({ "email_address": email, "password": PASSWORD })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        format!("Bearer {}", body["data"]["session_token"].as_str().unwrap())
    }

    fn events(&self, event: &str) -> Vec<camelmailer_core::LifecycleEvent> {
        self.store
            .lifecycle_events()
            .into_iter()
            .filter(|e| e.event == event)
            .collect()
    }
}

// ------------------------------------------------------------- the gate

#[tokio::test]
async fn the_monthly_limit_counts_every_server_and_stops_at_100_percent() {
    let h = harness().await;
    h.set_limit(json!(3)).await;

    assert_eq!(h.send(0, &["a@x.example"]).await.0, StatusCode::CREATED);
    assert_eq!(h.send(1, &["b@x.example"]).await.0, StatusCode::CREATED);
    assert_eq!(h.send(0, &["c@x.example"]).await.0, StatusCode::CREATED);

    // The fourth message, on either server, is refused before storage.
    for server in [0, 1] {
        let (status, body) = h.send(server, &["d@x.example"]).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(body["error"]["code"], json!("OrganizationQuotaExceeded"));
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("monthly send limit of 3"));
    }

    // A request that does not fit whole is refused whole.
    h.set_limit(json!(4)).await;
    let (status, _) = h.send(0, &["e@x.example", "f@x.example"]).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(h.send(0, &["e@x.example"]).await.0, StatusCode::CREATED);

    // Clearing the limit (explicit null) lifts the stop.
    h.set_limit(json!(null)).await;
    assert_eq!(h.send(1, &["g@x.example"]).await.0, StatusCode::CREATED);
}

#[tokio::test]
async fn the_stricter_of_server_and_organization_limit_applies() {
    let h = harness().await;
    h.set_limit(json!(100)).await;
    // A server limit below the organization's remainder wins.
    let (status, body) = h
        .admin(
            "PATCH",
            "/api/v2/admin/organizations/acme/servers/alpha",
            Some(json!({ "send_limit": 1 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.send(0, &["a@x.example"]).await.0, StatusCode::CREATED);
    let (status, body) = h.send(0, &["b@x.example"]).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], json!("SendLimitExceeded"));
    // The other server is still bound only by the organization.
    assert_eq!(h.send(1, &["c@x.example"]).await.0, StatusCode::CREATED);
}

#[tokio::test]
async fn a_suspended_organization_sends_nothing_until_unsuspended() {
    let h = harness().await;
    let (status, body) = h
        .admin(
            "POST",
            "/api/v2/admin/organizations/acme/suspend",
            Some(json!({ "reason": "Unpaid invoice" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["limits"]["suspended"], json!(true));

    for server in [0, 1] {
        let (status, body) = h.send(server, &["a@x.example"]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["code"], json!("OrganizationSuspended"));
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unpaid invoice"));
    }
    // Reads on the server API keep working.
    let (status, _) = h
        .request(
            "GET",
            "/api/v2/server",
            &[("X-Server-API-Key", &h.keys[0])],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = h
        .admin("POST", "/api/v2/admin/organizations/acme/unsuspend", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["limits"]["suspension_reason"], json!(null));
    assert_eq!(h.send(0, &["a@x.example"]).await.0, StatusCode::CREATED);
}

// ------------------------------------------------------ admin endpoints

#[tokio::test]
async fn usage_reports_sent_limit_and_remaining_for_the_month() {
    let h = harness().await;
    h.set_limit(json!(10)).await;
    h.send(0, &["a@x.example", "b@x.example"]).await;
    h.send(1, &["c@x.example"]).await;

    let (status, body) = h
        .admin("GET", "/api/v2/admin/organizations/acme/usage", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let usage = &body["data"]["usage"];
    let month = camelmailer_core::UsageMonth::current();
    assert_eq!(usage["month"], json!(month.label()));
    assert_eq!(usage["sent"], json!(3));
    assert_eq!(usage["limit"], json!(10));
    assert_eq!(usage["remaining"], json!(7));
    assert_eq!(usage["suspended"], json!(false));
    assert!(usage["period_start"]
        .as_str()
        .unwrap()
        .starts_with(&format!("{}-01T00:00:00", month.label())));

    // Another month has its own counter.
    let (status, body) = h
        .admin(
            "GET",
            "/api/v2/admin/organizations/acme/usage?month=2001-01",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["usage"]["sent"], json!(0));

    let (status, body) = h
        .admin(
            "GET",
            "/api/v2/admin/organizations/acme/usage?month=2001-13",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Without a limit, limit and remaining are null.
    h.set_limit(json!(null)).await;
    let (_, body) = h
        .admin("GET", "/api/v2/admin/organizations/acme/usage", None)
        .await;
    assert_eq!(body["data"]["usage"]["limit"], json!(null));
    assert_eq!(body["data"]["usage"]["remaining"], json!(null));
}

#[tokio::test]
async fn members_read_usage_and_limits_but_only_installation_admins_change_them() {
    let h = harness().await;
    let viewer = h.member("viewer@acme.example", Role::Viewer).await;
    let owner = h.member("owner@acme.example", Role::Owner).await;

    for path in [
        "/api/v2/admin/organizations/acme/usage",
        "/api/v2/admin/organizations/acme/limits",
    ] {
        let (status, body) = h
            .request("GET", path, &[("authorization", &viewer)], None)
            .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }

    // An owner may not lift their own quota or unsuspend themselves.
    for (method, path, body) in [
        (
            "PATCH",
            "/api/v2/admin/organizations/acme/limits",
            Some(json!({ "monthly_send_limit": null })),
        ),
        ("POST", "/api/v2/admin/organizations/acme/unsuspend", None),
        ("POST", "/api/v2/admin/organizations/acme/suspend", None),
    ] {
        let (status, response) = h
            .request(method, path, &[("authorization", &owner)], body)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {response}");
        assert_eq!(response["error"]["code"], json!("AccessDenied"));
    }

    // A non-member gets 404, like every foreign organization.
    let other = h
        .store
        .create_organization(NewOrganization {
            name: "Other".into(),
            permalink: "other".into(),
        })
        .await
        .unwrap();
    let _ = other;
    let (status, _) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/other/usage",
            &[("authorization", &viewer)],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Validation.
    let (status, body) = h
        .admin(
            "PATCH",
            "/api/v2/admin/organizations/acme/limits",
            Some(json!({ "monthly_send_limit": -1 })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // PATCH round trip: limit, suspension and reason in one call.
    let (status, body) = h
        .admin(
            "PATCH",
            "/api/v2/admin/organizations/acme/limits",
            Some(json!({ "monthly_send_limit": 500, "suspended": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["limits"],
        json!({
            "monthly_send_limit": 500,
            "suspended": true,
            "suspension_reason": "Suspended via Admin API",
            "retention": {
                "message_content_days": null,
                "message_metadata_days": null,
                "webhook_log_days": null,
                "dmarc_report_days": null,
            },
        })
    );
    let (_, body) = h
        .admin("GET", "/api/v2/admin/organizations/acme/limits", None)
        .await;
    assert_eq!(body["data"]["limits"]["monthly_send_limit"], json!(500));
}

// ---------------------------------------------------- lifecycle events

#[tokio::test]
async fn usage_thresholds_fire_once_each_at_80_and_100_percent() {
    let h = harness_with(true).await;
    h.set_limit(json!(5)).await;
    for index in 0..3 {
        assert_eq!(
            h.send(index % 2, &[&format!("r{index}@x.example")]).await.0,
            StatusCode::CREATED
        );
    }
    assert!(h.events("organization.usage_threshold").is_empty());
    h.send(0, &["r3@x.example"]).await; // 4/5 = 80 %
    let events = h.events("organization.usage_threshold");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data["threshold"], json!(80));
    assert_eq!(events[0].data["sent"], json!(4));
    assert_eq!(events[0].data["limit"], json!(5));
    assert_eq!(events[0].data["organization"]["permalink"], json!("acme"));

    h.send(1, &["r4@x.example"]).await; // 5/5 = 100 %
    let events = h.events("organization.usage_threshold");
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].data["threshold"], json!(100));

    // Raising the limit and crossing 80 % again in the same month does not
    // repeat the event.
    h.set_limit(json!(6)).await;
    h.send(0, &["r5@x.example"]).await;
    assert_eq!(h.events("organization.usage_threshold").len(), 2);
}

#[tokio::test]
async fn organization_and_user_lifecycle_events_are_queued_when_configured() {
    let h = harness_with(true).await;
    let (status, body) = h
        .admin(
            "POST",
            "/api/v2/admin/organizations",
            Some(json!({ "name": "Globex" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created = h.events("organization.created");
    assert_eq!(created.len(), 1);
    assert_eq!(
        created[0].data["organization"]["permalink"],
        json!("globex")
    );
    assert_eq!(created[0].data["user"], json!(null));

    let (status, _) = h
        .admin("DELETE", "/api/v2/admin/organizations/globex", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let deleted = h.events("organization.deleted");
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].data["organization"]["name"], json!("Globex"));

    let (status, body) = h
        .request(
            "POST",
            "/api/v2/auth/register",
            &[],
            Some(json!({
                "email_address": "new@example.com",
                "first_name": "New",
                "last_name": "User",
                "password": "a-long-enough-password",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let registered = h.events("user.registered");
    assert_eq!(registered.len(), 1);
    assert_eq!(
        registered[0].data["user"]["email_address"],
        json!("new@example.com")
    );
    assert_eq!(registered[0].data["source"], json!("registration"));
    // The envelope a receiver gets.
    let body: Value = serde_json::from_str(&registered[0].body()).unwrap();
    assert_eq!(body["event"], json!("user.registered"));
    assert_eq!(body["id"], json!(registered[0].id));
}

#[tokio::test]
async fn nothing_is_queued_when_lifecycle_webhooks_are_off() {
    let h = harness().await;
    h.admin(
        "POST",
        "/api/v2/admin/organizations",
        Some(json!({ "name": "Globex" })),
    )
    .await;
    h.admin("DELETE", "/api/v2/admin/organizations/globex", None)
        .await;
    assert!(h.store.lifecycle_events().is_empty());
}

// ------------------------------------------------------------ retention

#[tokio::test]
async fn retention_overrides_are_set_by_admins_and_shown_on_the_organization() {
    let h = harness().await;
    let viewer = h.member("viewer@acme.example", Role::Viewer).await;

    // Self-hosted default: everything kept forever.
    let (status, body) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/acme",
            &[("authorization", &viewer)],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let retention = &body["data"]["retention"];
    assert_eq!(retention["message_content_days"], json!(null));
    assert_eq!(retention["message_metadata_days"], json!(null));
    assert_eq!(retention["api_request_days"], json!(30));

    let (status, body) = h
        .admin(
            "PATCH",
            "/api/v2/admin/organizations/acme/limits",
            Some(json!({ "retention": {
                "message_content_days": 30,
                "message_metadata_days": 365,
                "webhook_log_days": 0,
            }})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["limits"]["retention"],
        json!({
            "message_content_days": 30,
            "message_metadata_days": 365,
            "webhook_log_days": 0,
            "dmarc_report_days": null,
        })
    );

    let (_, body) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/acme",
            &[("authorization", &viewer)],
            None,
        )
        .await;
    let retention = &body["data"]["retention"];
    assert_eq!(retention["message_content_days"], json!(30));
    assert_eq!(retention["message_metadata_days"], json!(365));
    assert_eq!(
        retention["webhook_log_days"],
        json!(null),
        "0 keeps forever"
    );
    assert_eq!(retention["overrides"]["webhook_log_days"], json!(0));

    // null inherits again; out-of-range values are refused.
    let (status, _) = h
        .admin(
            "PATCH",
            "/api/v2/admin/organizations/acme/limits",
            Some(json!({ "retention": { "message_content_days": null } })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .admin(
            "PATCH",
            "/api/v2/admin/organizations/acme/limits",
            Some(json!({ "retention": { "message_content_days": -1 } })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, body) = h
        .admin("GET", "/api/v2/admin/organizations/acme", None)
        .await;
    // Content follows metadata once its own override is gone.
    assert_eq!(
        body["data"]["retention"]["message_content_days"],
        json!(365)
    );
}
