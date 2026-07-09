//! Integration tests against a real PostgreSQL server.
//!
//! Set `CAMELMAILER_TEST_DATABASE_URL` (a role with CREATEDB, e.g.
//! `postgres://camelmailer:camelmailer@127.0.0.1:5433/camelmailer_test`) to
//! run these; they are skipped otherwise. Each test creates its own
//! throwaway database and runs the embedded migrations, so tests run in
//! parallel without interfering.

use camelmailer_core::{
    AdminStore, CredentialType, DomainOwner, MessageScope, NewOrganization, NewServer,
    QueuedMessage, RouteMode, ServerMode, Store,
};
use camelmailer_db::{PgMessageSink, PgStore};
use rand::Rng;
use sqlx::{PgPool, Row};

fn base_url() -> Option<String> {
    std::env::var("CAMELMAILER_TEST_DATABASE_URL").ok()
}

macro_rules! require_db {
    () => {
        match base_url() {
            Some(url) => url,
            None => {
                eprintln!("skipping: CAMELMAILER_TEST_DATABASE_URL is not set");
                return;
            }
        }
    };
}

/// Create a unique throwaway database and run migrations on it.
async fn test_pool(base: &str) -> PgPool {
    let name: String = {
        let mut rng = rand::thread_rng();
        (0..12)
            .map(|_| char::from(b'a' + rng.gen_range(0..26)))
            .collect()
    };
    let db_name = format!("cm_test_{name}");

    let admin_pool = camelmailer_db::connect(base, 1).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {db_name}"))
        .execute(&admin_pool)
        .await
        .unwrap();
    admin_pool.close().await;

    let db_url = {
        let position = base.rfind('/').unwrap();
        format!("{}/{}", &base[..position], db_name)
    };
    let pool = camelmailer_db::connect(&db_url, 5).await.unwrap();
    camelmailer_db::migrate(&pool).await.unwrap();
    pool
}

struct PgFixtures {
    store: PgStore,
    organization: camelmailer_core::Organization,
    server: camelmailer_core::Server,
}

async fn fixtures(pool: PgPool) -> PgFixtures {
    let store = PgStore::new(pool);
    let organization = store
        .create_organization(NewOrganization {
            name: "Example Org".into(),
            permalink: "example-org".into(),
        })
        .await
        .unwrap();
    let server = store
        .create_server(NewServer {
            organization_id: organization.id,
            name: "Example Server".into(),
            permalink: "example-server".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    PgFixtures {
        store,
        organization,
        server,
    }
}

fn message_for(server_id: camelmailer_core::Id, rcpt_to: &str) -> QueuedMessage {
    QueuedMessage {
        server_id,
        rcpt_to: rcpt_to.into(),
        mail_from: "sender@example.com".into(),
        raw_message: b"Subject: Test\r\n\r\nBody\r\n".to_vec(),
        received_with_ssl: false,
        scope: MessageScope::Incoming,
        bounce: false,
        domain_id: None,
        credential_id: None,
        route_id: None,
    }
}

// ------------------------------------------------------------ RLS isolation

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rls_scopes_reads_to_the_tenant_context() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let other_server = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();

    let sink = PgMessageSink::new(f.store.clone());
    sink.insert_message(&message_for(f.server.id, "a@tenant-a.example"))
        .await
        .unwrap();
    sink.insert_message(&message_for(other_server.id, "b@tenant-b.example"))
        .await
        .unwrap();

    // messages_for_server issues `SELECT * FROM messages` with no WHERE
    // clause — the visible rows are scoped purely by the RLS policy.
    let tenant_a = sink.messages_for_server(f.server.id).await.unwrap();
    assert_eq!(tenant_a.len(), 1);
    assert_eq!(tenant_a[0].rcpt_to, "a@tenant-a.example");

    let tenant_b = sink.messages_for_server(other_server.id).await.unwrap();
    assert_eq!(tenant_b.len(), 1);
    assert_eq!(tenant_b[0].rcpt_to, "b@tenant-b.example");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rls_hides_all_rows_without_a_tenant_context() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;
    let sink = PgMessageSink::new(f.store.clone());
    sink.insert_message(&message_for(f.server.id, "a@tenant-a.example"))
        .await
        .unwrap();

    // A plain connection without the tenant context set sees nothing —
    // FORCE ROW LEVEL SECURITY applies even to the table owner.
    let count: i64 = sqlx::query("SELECT count(*) AS c FROM messages")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("c");
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rls_rejects_writes_for_a_foreign_tenant() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;
    let other_server = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();

    // Try to insert a row for server A while the transaction's tenant
    // context is server B — the WITH CHECK clause must reject it.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('camelmailer.server_id', $1, true)")
        .bind(other_server.id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let result = sqlx::query(
        "INSERT INTO messages (server_id, token, scope, rcpt_to, mail_from, raw_message)
         VALUES ($1, 'tok', 'incoming', 'x@example.com', 'y@example.com', ''::bytea)",
    )
    .bind(f.server.id as i64)
    .execute(&mut *tx)
    .await;
    let error = result.expect_err("cross-tenant insert must be rejected");
    assert!(
        error.to_string().contains("row-level security"),
        "unexpected error: {error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rls_confines_bulk_updates_and_deletes_to_the_tenant() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;
    let other_server = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();

    let sink = PgMessageSink::new(f.store.clone());
    sink.insert_message(&message_for(f.server.id, "a@tenant-a.example"))
        .await
        .unwrap();
    sink.insert_message(&message_for(other_server.id, "b@tenant-b.example"))
        .await
        .unwrap();

    // An unfiltered UPDATE under tenant A's context must only touch A's rows.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('camelmailer.server_id', $1, true)")
        .bind(f.server.id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let updated = sqlx::query("UPDATE messages SET mail_from = 'rewritten@example.com'")
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    tx.commit().await.unwrap();
    assert_eq!(updated, 1);

    let tenant_b = sink.messages_for_server(other_server.id).await.unwrap();
    assert_eq!(tenant_b[0].mail_from, "sender@example.com");

    // Same for an unfiltered DELETE.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('camelmailer.server_id', $1, true)")
        .bind(f.server.id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let deleted = sqlx::query("DELETE FROM messages")
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    tx.commit().await.unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(
        sink.messages_for_server(other_server.id).await.unwrap().len(),
        1
    );
}

// -------------------------------------------------- Store (SMTP lookups)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_finds_smtp_credentials_servers_and_routes() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let credential = f
        .store
        .create_credential(f.server.id, CredentialType::Smtp, "secret-key")
        .await
        .unwrap();
    let domain = f
        .store
        .create_domain(DomainOwner::Server(f.server.id), "example.com", true)
        .await
        .unwrap();
    let route = f
        .store
        .create_route(f.server.id, Some(domain.id), "info", RouteMode::Endpoint)
        .await
        .unwrap();

    // sync Store trait, as used by the SMTP session
    let found = f.store.find_smtp_credential_by_key("secret-key").unwrap();
    assert_eq!(found.id, credential.id);
    assert!(f.store.find_smtp_credential_by_key("wrong").is_none());

    let found = f.store.find_server_by_token(&f.server.token).unwrap();
    assert_eq!(found.id, f.server.id);

    let resolved = f.store.find_route_by_token(&route.token).unwrap();
    assert_eq!(resolved.route.id, route.id);
    assert_eq!(resolved.domain_name, "example.com");
    assert_eq!(resolved.server.id, f.server.id);

    let resolved = f
        .store
        .find_route_by_name_and_domain("info", "example.com")
        .unwrap();
    assert_eq!(resolved.route.id, route.id);
    assert!(f
        .store
        .find_route_by_name_and_domain("info", "wrong.com")
        .is_none());

    let found = f
        .store
        .find_server_by_permalinks(&f.organization.permalink, &f.server.permalink)
        .unwrap();
    assert_eq!(found.id, f.server.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_matches_ip_credentials_by_longest_prefix() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let wide = f
        .store
        .create_credential(f.server.id, CredentialType::SmtpIp, "1.0.0.0/8")
        .await
        .unwrap();
    let narrow = f
        .store
        .create_credential(f.server.id, CredentialType::SmtpIp, "1.2.3.0/24")
        .await
        .unwrap();

    let found = f.store.find_ip_credential("1.2.3.4".parse().unwrap()).unwrap();
    assert_eq!(found.id, narrow.id);
    let found = f.store.find_ip_credential("1.9.9.9".parse().unwrap()).unwrap();
    assert_eq!(found.id, wide.id);
    assert!(f.store.find_ip_credential("9.9.9.9".parse().unwrap()).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_authenticates_from_domains_only_when_verified() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let verified = f
        .store
        .create_domain(DomainOwner::Server(f.server.id), "example.com", true)
        .await
        .unwrap();
    f.store
        .create_domain(DomainOwner::Server(f.server.id), "unverified.net", false)
        .await
        .unwrap();
    let org_domain = f
        .store
        .create_domain(DomainOwner::Organization(f.organization.id), "org.example", true)
        .await
        .unwrap();

    assert_eq!(
        f.store
            .find_authenticated_domain(f.server.id, &["test@example.com"]),
        Some(verified.id)
    );
    assert_eq!(
        f.store
            .find_authenticated_domain(f.server.id, &["Name <person@org.example>"]),
        Some(org_domain.id)
    );
    assert_eq!(
        f.store
            .find_authenticated_domain(f.server.id, &["test@unverified.net"]),
        None
    );
    assert_eq!(
        f.store
            .find_authenticated_domain(f.server.id, &["test@example.com", "x@unverified.net"]),
        None
    );
}

// ------------------------------------------------------------- AdminStore

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_store_crud_and_conflicts() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    // duplicate permalink → Conflict
    let error = f
        .store
        .create_organization(NewOrganization {
            name: "Duplicate".into(),
            permalink: "example-org".into(),
        })
        .await
        .expect_err("duplicate permalink must conflict");
    assert!(matches!(error, camelmailer_core::StoreError::Conflict(_)));

    let organizations = f.store.list_organizations().await.unwrap();
    assert_eq!(organizations.len(), 1);

    let found = f
        .store
        .organization_by_permalink("example-org")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, f.organization.id);

    // suspend via update_server
    let mut server = f.server.clone();
    server.suspended = true;
    server.suspension_reason = Some("abuse".into());
    f.store.update_server(server).await.unwrap();
    let reloaded = f
        .store
        .server_by_permalink(f.organization.id, "example-server")
        .await
        .unwrap()
        .unwrap();
    assert!(reloaded.suspended);
    assert_eq!(reloaded.suspension_reason.as_deref(), Some("abuse"));

    // deleting the organization cascades to servers
    assert!(f.store.delete_organization(f.organization.id).await.unwrap());
    assert!(f
        .store
        .server_by_permalink(f.organization.id, "example-server")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_api_keys_validate_and_record_use() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;

    f.store
        .create_admin_api_key("ci", "test-admin-key")
        .await
        .unwrap();
    assert!(f.store.admin_api_key_valid("test-admin-key").await.unwrap());
    assert!(!f.store.admin_api_key_valid("wrong").await.unwrap());

    let last_used: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query("SELECT last_used_at FROM admin_api_keys WHERE key = 'test-admin-key'")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("last_used_at");
    assert!(last_used.is_some());
}

// --------------------------------------------- end-to-end: SMTP → Postgres

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_smtp_session_stores_the_message_in_postgres() {
    use camelmailer_smtp::{Reply, Session, SessionConfig};
    use std::sync::Arc;

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    f.store
        .create_domain(DomainOwner::Server(f.server.id), "example.com", true)
        .await
        .unwrap();
    let credential = f
        .store
        .create_credential(f.server.id, CredentialType::Smtp, "smtp-key")
        .await
        .unwrap();

    let store = Arc::new(f.store.clone());
    let sink = Arc::new(PgMessageSink::new(f.store.clone()));
    let config = SessionConfig {
        smtp_hostname: "postal.example.com".into(),
        tls_enabled: false,
        max_message_size: 14,
        return_path_domain: "rp.postal.example.com".into(),
        custom_return_path_prefix: "psrp".into(),
        route_domain: "routes.postal.example.com".into(),
    };
    let mut session = Session::new(config, store, sink.clone(), Some("1.2.3.4".into()));

    // The blocking Store bridge must work from inside the runtime — this is
    // exactly how the tokio SMTP server drives the session.
    let auth = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(format!("\0XX\0{}", credential.key))
    };
    session.handle("HELO client.example.com");
    let reply = session.handle(&format!("AUTH PLAIN {auth}"));
    assert!(matches!(reply, Reply::Line(ref text) if text.starts_with("235 ")));
    session.handle("MAIL FROM: sender@example.com");
    let reply = session.handle("RCPT TO: someone@elsewhere.example");
    assert_eq!(reply, Reply::Line("250 OK".into()));
    session.handle("DATA");
    session.handle("From: sender@example.com");
    session.handle("Subject: E2E");
    session.handle("");
    session.handle("Hello from the Rust SMTP server.");
    session.handle("\r");
    let reply = session.handle(".\r");
    assert_eq!(reply, Reply::Line("250 OK".into()));

    let messages = sink.messages_for_server(f.server.id).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].scope, "outgoing");
    assert_eq!(messages[0].rcpt_to, "someone@elsewhere.example");
    assert_eq!(messages[0].credential_id, Some(credential.id));
    assert!(String::from_utf8_lossy(&messages[0].raw_message).contains("Subject: E2E"));
}

// ------------------------------------------------- suppressions (RLS)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suppressions_are_tenant_isolated_by_rls() {
    use camelmailer_core::NewSuppression;

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;
    let other_server = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();

    f.store
        .create_suppression(NewSuppression {
            server_id: f.server.id,
            suppression_type: "recipient".into(),
            address: "a@tenant-a.example".into(),
            reason: None,
        })
        .await
        .unwrap();
    f.store
        .create_suppression(NewSuppression {
            server_id: other_server.id,
            suppression_type: "recipient".into(),
            address: "b@tenant-b.example".into(),
            reason: None,
        })
        .await
        .unwrap();

    // duplicate within the tenant conflicts
    let error = f
        .store
        .create_suppression(NewSuppression {
            server_id: f.server.id,
            suppression_type: "recipient".into(),
            address: "a@tenant-a.example".into(),
            reason: None,
        })
        .await
        .expect_err("duplicate suppression must conflict");
    assert!(matches!(error, camelmailer_core::StoreError::Conflict(_)));

    // each tenant sees only its own entries
    let tenant_a = f.store.list_suppressions(f.server.id).await.unwrap();
    assert_eq!(tenant_a.len(), 1);
    assert_eq!(tenant_a[0].address, "a@tenant-a.example");
    let tenant_b = f.store.list_suppressions(other_server.id).await.unwrap();
    assert_eq!(tenant_b.len(), 1);

    // without a tenant context the table appears empty (FORCE RLS)
    let count: i64 = sqlx::query("SELECT count(*) AS c FROM suppressions")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("c");
    assert_eq!(count, 0);

    // deleting from tenant A cannot touch tenant B's entry
    assert!(!f
        .store
        .delete_suppression(f.server.id, "b@tenant-b.example")
        .await
        .unwrap());
    assert!(f
        .store
        .delete_suppression(f.server.id, "a@tenant-a.example")
        .await
        .unwrap());
}

// ------------------------------------------- message metadata (phase 3)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_index_subject_and_message_id_at_insert() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let sink = PgMessageSink::new(f.store.clone());

    let mut message = message_for(f.server.id, "user@example.net");
    message.raw_message =
        b"Subject: Quarterly Report\r\nMessage-ID: <q1@org.example>\r\n\r\nBody\r\n".to_vec();
    let id = sink.insert_message(&message).await.unwrap();

    let stored = sink
        .message_by_id(f.server.id, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.subject.as_deref(), Some("Quarterly Report"));
    assert_eq!(
        stored.message_id_header.as_deref(),
        Some("<q1@org.example>")
    );
    assert_eq!(stored.status, "Pending");
    assert_eq!(stored.spam_status, "NotChecked");
    assert_eq!(stored.size, message.raw_message.len() as i64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deliveries_update_message_status_and_are_listed() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let sink = PgMessageSink::new(f.store.clone());
    let id = sink
        .insert_message(&message_for(f.server.id, "user@example.net"))
        .await
        .unwrap();

    sink.record_delivery(f.server.id, id, "SoftFail", "greylisted", "451 try later", false)
        .await
        .unwrap();
    sink.record_delivery(f.server.id, id, "Sent", "accepted", "250 OK", true)
        .await
        .unwrap();

    let message = sink.message_by_id(f.server.id, id).await.unwrap().unwrap();
    assert_eq!(message.status, "Sent");
    assert!(!message.held);

    let deliveries = sink.deliveries_for_message(f.server.id, id).await.unwrap();
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0].status, "SoftFail");
    assert_eq!(deliveries[1].status, "Sent");
    assert_eq!(deliveries[1].output.as_deref(), Some("250 OK"));

    // Held marks the message held
    sink.record_delivery(f.server.id, id, "Held", "suppressed", "", false)
        .await
        .unwrap();
    let message = sink.message_by_id(f.server.id, id).await.unwrap().unwrap();
    assert_eq!(message.status, "Held");
    assert!(message.held);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spam_results_are_stored() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let sink = PgMessageSink::new(f.store.clone());
    let id = sink
        .insert_message(&message_for(f.server.id, "user@example.net"))
        .await
        .unwrap();
    sink.set_spam_result(f.server.id, id, "Spam", 7.5)
        .await
        .unwrap();
    let message = sink.message_by_id(f.server.id, id).await.unwrap().unwrap();
    assert_eq!(message.spam_status, "Spam");
    assert!((message.spam_score - 7.5).abs() < f64::EPSILON);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clicks_and_opens_are_recorded_per_message() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let sink = PgMessageSink::new(f.store.clone());
    let id = sink
        .insert_message(&message_for(f.server.id, "user@example.net"))
        .await
        .unwrap();

    let (link_id, link_token) = sink
        .create_link(f.server.id, id, "https://example.com/offer")
        .await
        .unwrap();
    assert_eq!(link_token.len(), 16);

    sink.record_link_click(f.server.id, link_id, "1.2.3.4", "TestUA/1.0")
        .await
        .unwrap();
    sink.record_link_click(f.server.id, link_id, "1.2.3.5", "TestUA/1.0")
        .await
        .unwrap();
    sink.record_load(f.server.id, id, "1.2.3.4", "TestUA/1.0")
        .await
        .unwrap();

    let (clicks, opens) = sink.activity_counts(f.server.id, id).await.unwrap();
    assert_eq!(clicks, 2);
    assert_eq!(opens, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activity_tables_are_rls_protected() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;
    let sink = PgMessageSink::new(f.store.clone());
    let id = sink
        .insert_message(&message_for(f.server.id, "user@example.net"))
        .await
        .unwrap();
    sink.record_delivery(f.server.id, id, "Sent", "ok", "250 OK", false)
        .await
        .unwrap();
    let (link_id, _) = sink
        .create_link(f.server.id, id, "https://example.com")
        .await
        .unwrap();
    sink.record_link_click(f.server.id, link_id, "1.1.1.1", "UA")
        .await
        .unwrap();
    sink.record_load(f.server.id, id, "1.1.1.1", "UA")
        .await
        .unwrap();

    // without a tenant context every activity table appears empty
    for table in ["deliveries", "links", "link_clicks", "loads"] {
        let count: i64 = sqlx::query(&format!("SELECT count(*) AS c FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("c");
        assert_eq!(count, 0, "{table} must be invisible without tenant context");
    }

    // a foreign tenant context sees nothing either
    let other_server = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let deliveries = sink
        .deliveries_for_message(other_server.id, id)
        .await
        .unwrap();
    assert!(deliveries.is_empty());
}
