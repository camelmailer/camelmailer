//! RBAC on the admin API: user sessions as principals, role enforcement
//! per organization, members + invitations management, audit feed.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, build_router, ApiState};
use camelmailer_core::auth;
use camelmailer_core::{AdminStore, AuthStore, MemoryStore, NewOrganization, NewUser, Role};
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

async fn harness_with_config(config: camelmailer_config::Config) -> Harness {
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::full(store.clone(), None, Some(store.clone()), None, config);
    let app = build_router(state.clone()).merge(build_auth_router(state));
    Harness { app, store }
}

async fn harness() -> Harness {
    harness_with_config(camelmailer_config::Config::default()).await
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

    async fn member(
        &self,
        org: &camelmailer_core::Organization,
        email: &str,
        role: Role,
    ) -> camelmailer_core::User {
        let user = self.user(email, false).await;
        self.store
            .upsert_membership(org.id, user.id, role)
            .await
            .unwrap();
        user
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
}

// ------------------------------------------------------ principal basics

#[tokio::test]
async fn a_session_token_authenticates_against_the_admin_api() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "viewer@example.com", Role::Viewer).await;
    let token = h.login("viewer@example.com").await;

    let (status, body) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/acme",
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["organization"]["permalink"], "acme");
}

#[tokio::test]
async fn global_admins_see_everything_users_only_their_orgs() {
    let h = harness().await;
    h.org("Acme").await;
    let beta = h.org("Beta").await;
    h.user("root@example.com", true).await;
    h.member(&beta, "user@example.com", Role::Viewer).await;

    let root = h.login("root@example.com").await;
    let (_, body) = h
        .request("GET", "/api/v2/admin/organizations", Some(&root), None)
        .await;
    assert_eq!(body["data"]["organizations"].as_array().unwrap().len(), 2);

    let user = h.login("user@example.com").await;
    let (_, body) = h
        .request("GET", "/api/v2/admin/organizations", Some(&user), None)
        .await;
    let orgs = body["data"]["organizations"].as_array().unwrap();
    assert_eq!(orgs.len(), 1);
    assert_eq!(orgs[0]["permalink"], "beta");
}

#[tokio::test]
async fn non_members_get_404_not_403_for_foreign_orgs() {
    let h = harness().await;
    h.org("Acme").await;
    h.user("outsider@example.com", false).await;
    let token = h.login("outsider@example.com").await;

    // both a foreign org and a nonexistent org answer 404 identically
    let (status, _) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/acme",
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/ghost",
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn global_resources_require_a_global_admin() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "owner@example.com", Role::Owner).await;
    h.user("root@example.com", true).await;

    let owner = h.login("owner@example.com").await;
    for path in [
        "/api/v2/admin/users",
        "/api/v2/admin/ip_pools",
        "/api/v2/admin/admin_api_keys",
        "/api/v2/admin/auth_events",
    ] {
        let (status, body) = h.request("GET", path, Some(&owner), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["error"]["code"], "Forbidden");
    }

    let root = h.login("root@example.com").await;
    for path in [
        "/api/v2/admin/users",
        "/api/v2/admin/ip_pools",
        "/api/v2/admin/admin_api_keys",
        "/api/v2/admin/auth_events",
    ] {
        let (status, _) = h.request("GET", path, Some(&root), None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
    }
}

// ---------------------------------------------------- role enforcement

#[tokio::test]
async fn viewers_read_but_cannot_write() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "viewer@example.com", Role::Viewer).await;
    let token = h.login("viewer@example.com").await;

    let (status, _) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/acme/servers",
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/servers",
            Some(&token),
            Some(json!({ "name": "Prod" })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"]["message"].as_str().unwrap().contains("admin"));
}

#[tokio::test]
async fn members_manage_server_resources_but_not_servers() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "member@example.com", Role::Member).await;
    let admin = h.member(&org, "admin@example.com", Role::Admin).await;
    let _ = admin;

    // an admin creates the server
    let admin_token = h.login("admin@example.com").await;
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/servers",
            Some(&admin_token),
            Some(json!({ "name": "Prod" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let member_token = h.login("member@example.com").await;
    // members may not create servers…
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/servers",
            Some(&member_token),
            Some(json!({ "name": "Second" })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // …but they work inside one
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/servers/prod/domains",
            Some(&member_token),
            Some(json!({ "name": "acme.example" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // viewers cannot
    h.member(&org, "viewer@example.com", Role::Viewer).await;
    let viewer_token = h.login("viewer@example.com").await;
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/servers/prod/domains",
            Some(&viewer_token),
            Some(json!({ "name": "two.example" })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn only_owners_delete_the_organization() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "admin@example.com", Role::Admin).await;
    h.member(&org, "owner@example.com", Role::Owner).await;

    let admin_token = h.login("admin@example.com").await;
    let (status, _) = h
        .request(
            "DELETE",
            "/api/v2/admin/organizations/acme",
            Some(&admin_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let owner_token = h.login("owner@example.com").await;
    let (status, body) = h
        .request(
            "DELETE",
            "/api/v2/admin/organizations/acme",
            Some(&owner_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["deleted"], true);
}

// ------------------------------------------------------- org creation

#[tokio::test]
async fn creating_an_organization_makes_the_user_its_owner() {
    let h = harness().await;
    h.user("founder@example.com", false).await;
    let token = h.login("founder@example.com").await;

    let (status, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations",
            Some(&token),
            Some(json!({ "name": "Startup" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // the creator is owner: may delete it
    let (status, _) = h
        .request(
            "DELETE",
            "/api/v2/admin/organizations/startup",
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn org_creation_can_be_restricted_to_admins() {
    let mut config = camelmailer_config::Config::default();
    config.auth.allow_organization_creation = false;
    let h = harness_with_config(config).await;
    h.user("user@example.com", false).await;
    h.user("root@example.com", true).await;

    let token = h.login("user@example.com").await;
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations",
            Some(&token),
            Some(json!({ "name": "Nope" })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let root = h.login("root@example.com").await;
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations",
            Some(&root),
            Some(json!({ "name": "Fine" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

// ------------------------------------------------------------ members

#[tokio::test]
async fn members_endpoint_lists_and_manages_roles() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "owner@example.com", Role::Owner).await;
    let admin = h.member(&org, "admin@example.com", Role::Admin).await;
    let member = h.member(&org, "member@example.com", Role::Member).await;

    let owner_token = h.login("owner@example.com").await;
    let (status, body) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/acme/members",
            Some(&owner_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["members"].as_array().unwrap().len(), 3);

    // a viewer can see the member list but not change it
    h.member(&org, "viewer@example.com", Role::Viewer).await;
    let viewer_token = h.login("viewer@example.com").await;
    let (status, _) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/acme/members",
            Some(&viewer_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .request(
            "PATCH",
            &format!("/api/v2/admin/organizations/acme/members/{}", member.id),
            Some(&viewer_token),
            Some(json!({ "role": "admin" })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // an admin promotes member→admin, but may not grant owner
    let admin_token = h.login("admin@example.com").await;
    let (status, body) = h
        .request(
            "PATCH",
            &format!("/api/v2/admin/organizations/acme/members/{}", member.id),
            Some(&admin_token),
            Some(json!({ "role": "admin" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["member"]["role"], "admin");
    let (status, _) = h
        .request(
            "PATCH",
            &format!("/api/v2/admin/organizations/acme/members/{}", member.id),
            Some(&admin_token),
            Some(json!({ "role": "owner" })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // the owner grants owner
    let (status, _) = h
        .request(
            "PATCH",
            &format!("/api/v2/admin/organizations/acme/members/{}", member.id),
            Some(&owner_token),
            Some(json!({ "role": "owner" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // an admin may remove a plain member but not an owner
    let (status, _) = h
        .request(
            "DELETE",
            &format!("/api/v2/admin/organizations/acme/members/{}", admin.id),
            Some(&owner_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let member_as_owner = member;
    let (status, _) = h
        .request(
            "DELETE",
            &format!(
                "/api/v2/admin/organizations/acme/members/{}",
                member_as_owner.id
            ),
            Some(&owner_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "owner may remove a co-owner");
}

#[tokio::test]
async fn the_last_owner_is_immovable() {
    let h = harness().await;
    let org = h.org("Acme").await;
    let owner = h.member(&org, "owner@example.com", Role::Owner).await;
    let token = h.login("owner@example.com").await;

    let (status, body) = h
        .request(
            "PATCH",
            &format!("/api/v2/admin/organizations/acme/members/{}", owner.id),
            Some(&token),
            Some(json!({ "role": "admin" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("at least one owner"));

    let (status, _) = h
        .request(
            "DELETE",
            &format!("/api/v2/admin/organizations/acme/members/{}", owner.id),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn existing_accounts_can_be_added_directly() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "admin@example.com", Role::Admin).await;
    h.user("colleague@example.com", false).await;
    let token = h.login("admin@example.com").await;

    let (status, body) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/members",
            Some(&token),
            Some(json!({ "email_address": "colleague@example.com", "role": "member" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["member"]["role"], "member");

    // unknown accounts are pointed to invitations
    let (status, body) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/members",
            Some(&token),
            Some(json!({ "email_address": "ghost@example.com", "role": "member" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("invitation"));
}

// --------------------------------------------------------- invitations

#[tokio::test]
async fn invitation_flow_creates_the_account_and_membership() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "admin@example.com", Role::Admin).await;
    let token = h.login("admin@example.com").await;

    let (status, body) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/invitations",
            Some(&token),
            Some(json!({ "email_address": "new@example.com", "role": "member" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let invite_token = body["data"]["invitation"]["invite_token"]
        .as_str()
        .unwrap()
        .to_string();

    // the public preview shows org + role without authentication
    let (status, body) = h
        .request(
            "GET",
            &format!("/api/v2/auth/invitations/{invite_token}"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["invitation"]["organization"]["permalink"],
        "acme"
    );
    assert_eq!(body["data"]["invitation"]["user_exists"], false);

    // accepting without a password fails for a new address
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/auth/invitations/accept",
            None,
            Some(json!({ "token": invite_token })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // accepting creates the account, membership and a session
    let (status, body) = h
        .request(
            "POST",
            "/api/v2/auth/invitations/accept",
            None,
            Some(json!({
                "token": invite_token,
                "first_name": "New",
                "last_name": "Person",
                "password": "brand-new-pass-1",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["account_created"], true);
    let session = body["data"]["session_token"].as_str().unwrap().to_string();

    // the fresh session sees the org
    let (status, body) = h
        .request("GET", "/api/v2/auth/me", Some(&session), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["memberships"][0]["role"], "member");

    // the token is single use
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/auth/invitations/accept",
            None,
            Some(json!({ "token": invite_token, "password": "whatever-123" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn accepting_as_an_existing_account_adds_membership_without_a_session() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "admin@example.com", Role::Admin).await;
    h.user("existing@example.com", false).await;
    let token = h.login("admin@example.com").await;

    let (_, body) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/invitations",
            Some(&token),
            Some(json!({ "email_address": "existing@example.com", "role": "viewer" })),
        )
        .await;
    let invite_token = body["data"]["invitation"]["invite_token"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = h
        .request(
            "POST",
            "/api/v2/auth/invitations/accept",
            None,
            Some(json!({ "token": invite_token })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["account_created"], false);
    assert!(
        body["data"]["session_token"].is_null(),
        "an invite must never yield a session for an existing account"
    );

    // membership exists now
    let user_token = h.login("existing@example.com").await;
    let (_, body) = h
        .request("GET", "/api/v2/auth/me", Some(&user_token), None)
        .await;
    assert_eq!(body["data"]["memberships"][0]["role"], "viewer");
}

#[tokio::test]
async fn invitations_can_be_listed_and_revoked() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "admin@example.com", Role::Admin).await;
    let token = h.login("admin@example.com").await;

    let (_, body) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/invitations",
            Some(&token),
            Some(json!({ "email_address": "a@example.com" })),
        )
        .await;
    let id = body["data"]["invitation"]["id"].as_u64().unwrap();
    let invite_token = body["data"]["invitation"]["invite_token"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = h
        .request(
            "GET",
            "/api/v2/admin/organizations/acme/invitations",
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let invitations = body["data"]["invitations"].as_array().unwrap();
    assert_eq!(invitations.len(), 1);
    assert!(invitations[0].get("invite_token").is_none());

    let (status, _) = h
        .request(
            "DELETE",
            &format!("/api/v2/admin/organizations/acme/invitations/{id}"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // a revoked invitation no longer accepts
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/auth/invitations/accept",
            None,
            Some(json!({ "token": invite_token, "password": "whatever-123" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn admins_cannot_invite_owners_but_owners_can() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "admin@example.com", Role::Admin).await;
    h.member(&org, "owner@example.com", Role::Owner).await;

    let admin_token = h.login("admin@example.com").await;
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/invitations",
            Some(&admin_token),
            Some(json!({ "email_address": "boss@example.com", "role": "owner" })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let owner_token = h.login("owner@example.com").await;
    let (status, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/invitations",
            Some(&owner_token),
            Some(json!({ "email_address": "boss@example.com", "role": "owner" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

// -------------------------------------------------- bootstrap & audit

#[tokio::test]
async fn admin_api_key_can_create_a_user_with_a_password() {
    let h = harness().await;
    // machine-key path: set the DB-backed admin key
    h.store
        .create_admin_api_key("ci", "machine-key")
        .await
        .unwrap();

    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v2/admin/users")
                .header("X-Admin-API-Key", "machine-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "email_address": "boot@example.com",
                        "first_name": "Boot",
                        "admin": true,
                        "password": "bootstrap-pass-1",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let (status, _) = h
        .request(
            "POST",
            "/api/v2/auth/login",
            None,
            Some(json!({
                "email_address": "boot@example.com",
                "password": "bootstrap-pass-1",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // a too-short bootstrap password is rejected by policy
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v2/admin/users")
                .header("X-Admin-API-Key", "machine-key")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "email_address": "b2@example.com", "password": "short" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn the_audit_feed_records_the_journey() {
    let h = harness().await;
    let org = h.org("Acme").await;
    h.member(&org, "owner@example.com", Role::Owner).await;
    h.user("root@example.com", true).await;
    let owner_token = h.login("owner@example.com").await;

    // some activity: an invitation
    let (_, _) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/invitations",
            Some(&owner_token),
            Some(json!({ "email_address": "n@example.com" })),
        )
        .await;

    let root = h.login("root@example.com").await;
    let (status, body) = h
        .request(
            "GET",
            "/api/v2/admin/auth_events?limit=10",
            Some(&root),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let events: Vec<&str> = body["data"]["auth_events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    assert!(events.contains(&"login.success"));
    assert!(events.contains(&"invitation.created"));
}
