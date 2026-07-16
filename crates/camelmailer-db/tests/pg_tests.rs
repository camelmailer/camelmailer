//! Integration tests against a real PostgreSQL server.
//!
//! Set `CAMELMAILER_TEST_DATABASE_URL` (a role with CREATEDB, e.g.
//! `postgres://camelmailer:camelmailer@127.0.0.1:5433/camelmailer_test`) to
//! run these; they are skipped otherwise. Each test creates its own
//! throwaway database and runs the embedded migrations, so tests run in
//! parallel without interfering.

use camelmailer_core::{
    AdminStore, CredentialType, DomainOwner, MessageScope, NewOrgEmailDomain, NewOrgSsoConnection,
    NewOrganization, NewServer, OrgSsoConnectionUpdate, OrgSsoStore, QueuedMessage, Role,
    RouteMode, ServerMode, SsoKind, Store,
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
    let pool = camelmailer_db::connect(&db_url, 2).await.unwrap();
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
        tag: None,
        metadata: None,
        stream_id: None,
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
        sink.messages_for_server(other_server.id)
            .await
            .unwrap()
            .len(),
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
        .create_domain(DomainOwner::Server(f.server.id), "example.com", true, None)
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

    let found = f
        .store
        .find_ip_credential("1.2.3.4".parse().unwrap())
        .unwrap();
    assert_eq!(found.id, narrow.id);
    let found = f
        .store
        .find_ip_credential("1.9.9.9".parse().unwrap())
        .unwrap();
    assert_eq!(found.id, wide.id);
    assert!(f
        .store
        .find_ip_credential("9.9.9.9".parse().unwrap())
        .is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_authenticates_from_domains_only_when_verified() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let verified = f
        .store
        .create_domain(DomainOwner::Server(f.server.id), "example.com", true, None)
        .await
        .unwrap();
    f.store
        .create_domain(
            DomainOwner::Server(f.server.id),
            "unverified.net",
            false,
            None,
        )
        .await
        .unwrap();
    let org_domain = f
        .store
        .create_domain(
            DomainOwner::Organization(f.organization.id),
            "org.example",
            true,
            None,
        )
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn domains_store_dkim_keys_and_stable_verification_tokens() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let with_key = f
        .store
        .create_server_domain(f.server.id, "keyed.example", Some("PEM".into()))
        .await
        .unwrap();
    assert_eq!(with_key.dkim_private_key.as_deref(), Some("PEM"));
    assert!(!with_key.verification_token.is_empty());
    assert!(!with_key.verified);

    let without_key = f
        .store
        .create_server_domain(f.server.id, "plain.example", None)
        .await
        .unwrap();
    assert!(without_key.dkim_private_key.is_none());
    assert!(!without_key.verification_token.is_empty());
    assert_ne!(
        with_key.verification_token, without_key.verification_token,
        "tokens must be per-domain"
    );

    // the token and the key are stable across reads (by name and by id)
    let reread = f
        .store
        .domain_by_name(f.server.id, "keyed.example")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reread.verification_token, with_key.verification_token);
    assert_eq!(reread.dkim_private_key, with_key.dkim_private_key);
    let by_id = f.store.domain_by_id(with_key.id).await.unwrap().unwrap();
    assert_eq!(by_id.verification_token, with_key.verification_token);
    assert_eq!(by_id.dkim_private_key, with_key.dkim_private_key);

    // verifying does not touch the token or the key
    f.store
        .set_domain_verified(with_key.id, true)
        .await
        .unwrap();
    let verified = f.store.domain_by_id(with_key.id).await.unwrap().unwrap();
    assert!(verified.verified);
    assert_eq!(verified.verification_token, with_key.verification_token);
    assert_eq!(verified.dkim_private_key, with_key.dkim_private_key);

    let error = f
        .store
        .create_server_domain(f.server.id, "keyed.example", None)
        .await
        .expect_err("duplicate domain must conflict");
    assert!(matches!(error, camelmailer_core::StoreError::Conflict(_)));
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

    // suspend via update_server (also round-trips broadcast_physical_address)
    let mut server = f.server.clone();
    server.suspended = true;
    server.suspension_reason = Some("abuse".into());
    server.broadcast_physical_address = Some("Acme Inc, 1 Main St".into());
    f.store.update_server(server).await.unwrap();
    let reloaded = f
        .store
        .server_by_permalink(f.organization.id, "example-server")
        .await
        .unwrap()
        .unwrap();
    assert!(reloaded.suspended);
    assert_eq!(reloaded.suspension_reason.as_deref(), Some("abuse"));
    assert_eq!(
        reloaded.broadcast_physical_address.as_deref(),
        Some("Acme Inc, 1 Main St")
    );

    // deleting the organization cascades to servers
    assert!(f
        .store
        .delete_organization(f.organization.id)
        .await
        .unwrap());
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
        .create_domain(DomainOwner::Server(f.server.id), "example.com", true, None)
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
            stream_id: None,
        })
        .await
        .unwrap();
    f.store
        .create_suppression(NewSuppression {
            server_id: other_server.id,
            suppression_type: "recipient".into(),
            address: "b@tenant-b.example".into(),
            reason: None,
            stream_id: None,
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
            stream_id: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_scoped_suppressions_and_unsubscribe_tokens() {
    use camelmailer_core::{NewStream, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let stream = ServerStore::create_stream(
        &f.store,
        NewStream {
            server_id: f.server.id,
            name: "Broadcast".into(),
            permalink: "broadcast".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        },
    )
    .await
    .unwrap();

    // A stream-scoped unsubscribe token round-trips.
    let token = ServerStore::create_unsubscribe_token(
        &f.store,
        f.server.id,
        Some(stream.id),
        "r@dest.example",
    )
    .await
    .unwrap();
    assert_eq!(
        ServerStore::resolve_unsubscribe_token(&f.store, &token)
            .await
            .unwrap(),
        Some((f.server.id, Some(stream.id), "r@dest.example".to_string()))
    );
    assert_eq!(
        ServerStore::resolve_unsubscribe_token(&f.store, "nope")
            .await
            .unwrap(),
        None
    );

    // Recording is idempotent and creates the stream-scoped suppression.
    assert!(ServerStore::record_unsubscribe(&f.store, &token)
        .await
        .unwrap());
    assert!(ServerStore::record_unsubscribe(&f.store, &token)
        .await
        .unwrap());
    assert!(!ServerStore::record_unsubscribe(&f.store, "nope")
        .await
        .unwrap());

    // Blocks only the matching stream — not another stream, not server-wide.
    assert!(ServerStore::address_suppressed(
        &f.store,
        f.server.id,
        "r@dest.example",
        Some(stream.id)
    )
    .await
    .unwrap());
    assert!(
        !ServerStore::address_suppressed(&f.store, f.server.id, "r@dest.example", None)
            .await
            .unwrap()
    );
    let other_stream = f.server.default_stream_id;
    assert!(!ServerStore::address_suppressed(
        &f.store,
        f.server.id,
        "r@dest.example",
        other_stream
    )
    .await
    .unwrap());

    // A server-wide suppression blocks every stream (today's bounce behaviour).
    f.store
        .create_suppression(camelmailer_core::NewSuppression {
            server_id: f.server.id,
            suppression_type: "recipient".into(),
            address: "bounce@dest.example".into(),
            reason: None,
            stream_id: None,
        })
        .await
        .unwrap();
    assert!(ServerStore::address_suppressed(
        &f.store,
        f.server.id,
        "bounce@dest.example",
        Some(stream.id)
    )
    .await
    .unwrap());
    assert!(
        ServerStore::address_suppressed(&f.store, f.server.id, "bounce@dest.example", None)
            .await
            .unwrap()
    );

    // The idempotent record left exactly one suppression row.
    let list = f.store.list_suppressions(f.server.id).await.unwrap();
    let unsub: Vec<_> = list
        .iter()
        .filter(|s| s.suppression_type == "unsubscribe")
        .collect();
    assert_eq!(unsub.len(), 1);
    assert_eq!(unsub[0].stream_id, Some(stream.id));
}

// ------------------------------------------- subscriptions (phase 4, RLS)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_gate_broadcast_and_are_tenant_isolated() {
    use camelmailer_core::{NewStream, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let stream = ServerStore::create_stream(
        &f.store,
        NewStream {
            server_id: f.server.id,
            name: "Broadcast".into(),
            permalink: "broadcast".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        },
    )
    .await
    .unwrap();

    // upsert opt-in, then flip status in place (no duplicate).
    let created = ServerStore::upsert_subscription(
        &f.store,
        f.server.id,
        stream.id,
        "r@dest.example",
        "subscribed",
    )
    .await
    .unwrap();
    assert_eq!(created.status, "subscribed");
    assert!(created.created_at.is_some());
    assert!(
        ServerStore::is_subscribed(&f.store, f.server.id, stream.id, "r@dest.example")
            .await
            .unwrap()
    );

    let flipped = ServerStore::upsert_subscription(
        &f.store,
        f.server.id,
        stream.id,
        "r@dest.example",
        "unsubscribed",
    )
    .await
    .unwrap();
    assert_eq!(flipped.id, created.id);
    assert_eq!(flipped.status, "unsubscribed");
    assert!(
        !ServerStore::is_subscribed(&f.store, f.server.id, stream.id, "r@dest.example")
            .await
            .unwrap()
    );
    assert_eq!(
        ServerStore::list_subscriptions(&f.store, f.server.id, stream.id)
            .await
            .unwrap()
            .len(),
        1
    );

    // record_unsubscribe flips the subscription to unsubscribed and the gate
    // then reads closed.
    ServerStore::upsert_subscription(
        &f.store,
        f.server.id,
        stream.id,
        "r@dest.example",
        "subscribed",
    )
    .await
    .unwrap();
    assert!(
        ServerStore::is_subscribed(&f.store, f.server.id, stream.id, "r@dest.example")
            .await
            .unwrap()
    );
    let token = ServerStore::create_unsubscribe_token(
        &f.store,
        f.server.id,
        Some(stream.id),
        "r@dest.example",
    )
    .await
    .unwrap();
    assert!(ServerStore::record_unsubscribe(&f.store, &token)
        .await
        .unwrap());
    assert!(
        !ServerStore::is_subscribed(&f.store, f.server.id, stream.id, "r@dest.example")
            .await
            .unwrap()
    );

    // remove is a boolean; a second remove is a no-op.
    assert!(
        ServerStore::remove_subscription(&f.store, f.server.id, stream.id, "r@dest.example")
            .await
            .unwrap()
    );
    assert!(
        !ServerStore::remove_subscription(&f.store, f.server.id, stream.id, "r@dest.example")
            .await
            .unwrap()
    );

    // Tenant isolation: a second server sees none of the first's rows, even
    // with a raw (bypass-free) count.
    let other = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: camelmailer_core::ServerMode::Live,
        })
        .await
        .unwrap();
    ServerStore::upsert_subscription(
        &f.store,
        f.server.id,
        stream.id,
        "keep@dest.example",
        "subscribed",
    )
    .await
    .unwrap();
    assert_eq!(
        ServerStore::list_subscriptions(&f.store, other.id, stream.id)
            .await
            .unwrap()
            .len(),
        0
    );
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

    let stored = sink.message_by_id(f.server.id, id).await.unwrap().unwrap();
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

    sink.record_delivery(
        f.server.id,
        id,
        "SoftFail",
        "greylisted",
        "451 try later",
        false,
        None,
    )
    .await
    .unwrap();
    sink.record_delivery(f.server.id, id, "Sent", "accepted", "250 OK", true, None)
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
    sink.record_delivery(f.server.id, id, "Held", "suppressed", "", false, None)
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
async fn campaign_stats_aggregate_over_attributed_messages() {
    use camelmailer_core::{NewCampaign, NewStream, NewSuppression, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let sink = PgMessageSink::new(f.store.clone());

    let stream = ServerStore::create_stream(
        &f.store,
        NewStream {
            server_id: f.server.id,
            name: "Broadcast".into(),
            permalink: "broadcast".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        },
    )
    .await
    .unwrap();

    // A suppression created BEFORE the campaign must not count towards its
    // unsubscribes (it predates the campaign's created_at).
    f.store
        .create_suppression(NewSuppression {
            server_id: f.server.id,
            suppression_type: "unsubscribe".into(),
            address: "old@dest.example".into(),
            reason: None,
            stream_id: Some(stream.id),
        })
        .await
        .unwrap();

    // Three messages attributed to the campaign, plus one unattributed message
    // that must never appear in the stats.
    let mut ids = Vec::new();
    for rcpt in ["a@dest.example", "b@dest.example", "c@dest.example"] {
        let mut message = message_for(f.server.id, rcpt);
        message.scope = MessageScope::Outgoing;
        message.stream_id = Some(stream.id);
        let sent = f.store.store_outgoing(message).await.unwrap();
        ids.push(sent.id);
    }
    let unattributed = message_for(f.server.id, "z@dest.example");
    sink.insert_message(&unattributed).await.unwrap();

    let campaign = f
        .store
        .create_campaign(NewCampaign {
            server_id: f.server.id,
            stream_id: stream.id,
            name: Some("Digest".into()),
            subject: Some("Hi".into()),
            from_address: Some("news@org.example".into()),
            html_body: Some("<p>x</p>".into()),
            text_body: None,
            total: 3,
        })
        .await
        .unwrap();
    f.store
        .set_campaign_progress(
            f.server.id,
            campaign.id,
            3,
            "sent",
            Some(chrono::Utc::now()),
        )
        .await
        .unwrap();
    for id in &ids {
        f.store
            .set_message_campaign(f.server.id, *id, campaign.id)
            .await
            .unwrap();
    }

    // message a: delivered + opened; message b: delivered + clicked;
    // message c: bounced (failed).
    sink.record_delivery(f.server.id, ids[0], "Sent", "ok", "250 OK", true, None)
        .await
        .unwrap();
    sink.record_delivery(f.server.id, ids[1], "Sent", "ok", "250 OK", true, None)
        .await
        .unwrap();
    sink.record_delivery(f.server.id, ids[2], "Bounced", "550", "", false, None)
        .await
        .unwrap();
    sink.record_load(f.server.id, ids[0], "1.2.3.4", "UA")
        .await
        .unwrap();
    let (link_id, _) = sink
        .create_link(f.server.id, ids[1], "https://example.com/o")
        .await
        .unwrap();
    sink.record_link_click(f.server.id, link_id, "1.2.3.4", "UA")
        .await
        .unwrap();

    // A stream-scoped unsubscribe created after the campaign counts; one on a
    // different stream does not.
    f.store
        .create_suppression(NewSuppression {
            server_id: f.server.id,
            suppression_type: "unsubscribe".into(),
            address: "new@dest.example".into(),
            reason: None,
            stream_id: Some(stream.id),
        })
        .await
        .unwrap();

    let stats = f
        .store
        .campaign_stats(f.server.id, campaign.id)
        .await
        .unwrap();
    assert_eq!(stats.total, 3);
    assert_eq!(stats.sent, 3);
    assert_eq!(stats.delivered, 2);
    assert_eq!(stats.failed, 1);
    assert_eq!(stats.opened, 1);
    assert_eq!(stats.clicked, 1);
    assert_eq!(stats.unsubscribed, 1);

    // Listing and lookup are tenant-scoped and newest-first.
    let list = f
        .store
        .list_campaigns(f.server.id, stream.id)
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, campaign.id);
    assert!(f
        .store
        .get_campaign(f.server.id, campaign.id)
        .await
        .unwrap()
        .is_some());
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
    sink.record_delivery(f.server.id, id, "Sent", "ok", "250 OK", false, None)
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

// ------------------------------------------- server API tokens (P1)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_for_api_token_resolves_and_records_use() {
    use camelmailer_core::{CredentialType, NewCredential};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;

    let token = "server-api-token-abc123";
    f.store
        .create_credential_record(NewCredential {
            server_id: f.server.id,
            credential_type: CredentialType::Api,
            name: "api".into(),
            key: Some(token.into()),
        })
        .await
        .unwrap();

    // resolves to the owning server
    let resolved = f.store.server_for_api_token(token).await.unwrap().unwrap();
    assert_eq!(resolved.id, f.server.id);
    assert!(f
        .store
        .server_for_api_token("wrong")
        .await
        .unwrap()
        .is_none());

    // last_used_at was stamped
    use sqlx::Row;
    let last_used: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query("SELECT last_used_at FROM credentials WHERE key = $1")
            .bind(token)
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("last_used_at");
    assert!(last_used.is_some());

    // ... and surfaces on the trait-level credential listing
    let listed = f.store.list_credentials(f.server.id).await.unwrap();
    assert!(listed[0].last_used_at.is_some());

    // held credentials do not resolve
    sqlx::query("UPDATE credentials SET hold = true WHERE key = $1")
        .bind(token)
        .execute(&pool)
        .await
        .unwrap();
    assert!(f.store.server_for_api_token(token).await.unwrap().is_none());
}

// ------------------------------------------- message read APIs (P3)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_store_reads_are_filtered_and_tenant_scoped() {
    use camelmailer_core::{MessageFilter, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let other = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();

    // two outgoing messages for our server (distinct tags) + one for the other
    let mut welcome = message_for(f.server.id, "a@dest.example");
    welcome.scope = MessageScope::Outgoing;
    welcome.tag = Some("welcome".into());
    welcome.raw_message = b"Subject: Hello A\r\n\r\nBody\r\n".to_vec();
    let (welcome_id, _) = f
        .store
        .store_outgoing(welcome)
        .await
        .map(|m| (m.id, m.token))
        .unwrap();

    let mut promo = message_for(f.server.id, "b@dest.example");
    promo.scope = MessageScope::Outgoing;
    promo.tag = Some("promo".into());
    promo.raw_message = b"Subject: Hello B\r\n\r\nBody\r\n".to_vec();
    f.store.store_outgoing(promo).await.unwrap();

    let mut theirs = message_for(other.id, "c@dest.example");
    theirs.scope = MessageScope::Outgoing;
    f.store.store_outgoing(theirs).await.unwrap();

    // list is scoped to the tenant and newest-first
    let all = f
        .store
        .messages(f.server.id, &MessageFilter::default())
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert!(all[0].id > all[1].id);

    // filter by tag
    let tagged = f
        .store
        .messages(
            f.server.id,
            &MessageFilter {
                tag: Some("promo".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].rcpt_to, "b@dest.example");

    // ILIKE substring on subject
    let by_subject = f
        .store
        .messages(
            f.server.id,
            &MessageFilter {
                query: Some("hello a".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(by_subject.len(), 1);
    assert_eq!(by_subject[0].id, welcome_id);

    // a delivery + open + click recorded on our message are readable
    let sink = PgMessageSink::new(f.store.clone());
    sink.record_delivery(f.server.id, welcome_id, "Sent", "250 OK", "", true, None)
        .await
        .unwrap();
    sink.record_load(f.server.id, welcome_id, "1.2.3.4", "Mail")
        .await
        .unwrap();
    let (link_id, _) = sink
        .create_link(f.server.id, welcome_id, "https://example.com")
        .await
        .unwrap();
    sink.record_link_click(f.server.id, link_id, "1.2.3.4", "Mail")
        .await
        .unwrap();

    assert_eq!(
        f.store
            .deliveries(f.server.id, welcome_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.store.opens(f.server.id, welcome_id).await.unwrap().len(),
        1
    );
    let clicks = f.store.clicks(f.server.id, welcome_id).await.unwrap();
    assert_eq!(clicks.len(), 1);
    assert_eq!(clicks[0].url.as_deref(), Some("https://example.com"));

    // the other tenant sees none of it (RLS)
    assert!(f
        .store
        .message(other.id, welcome_id)
        .await
        .unwrap()
        .is_none());
    assert!(f
        .store
        .deliveries(other.id, welcome_id)
        .await
        .unwrap()
        .is_empty());
    assert!(f
        .store
        .opens(other.id, welcome_id)
        .await
        .unwrap()
        .is_empty());
    assert!(f
        .store
        .clicks(other.id, welcome_id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_store_stats_and_bounces_are_tenant_scoped() {
    use camelmailer_core::{MessageFilter, ServerStore, StatsFilter};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let other = f
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

    // two outgoing messages for our server: one Sent (with open + click),
    // one Bounced. Plus one message for the other tenant.
    let mut sent = message_for(f.server.id, "a@dest.example");
    sent.scope = MessageScope::Outgoing;
    let (sent_id, _) = f
        .store
        .store_outgoing(sent)
        .await
        .map(|m| (m.id, m.token))
        .unwrap();
    sink.record_delivery(f.server.id, sent_id, "Sent", "250 OK", "", true, None)
        .await
        .unwrap();
    sink.record_load(f.server.id, sent_id, "1.2.3.4", "Mail")
        .await
        .unwrap();
    let (link_id, _) = sink
        .create_link(f.server.id, sent_id, "https://example.com")
        .await
        .unwrap();
    sink.record_link_click(f.server.id, link_id, "1.2.3.4", "Mail")
        .await
        .unwrap();

    let mut bounced = message_for(f.server.id, "b@dest.example");
    bounced.scope = MessageScope::Outgoing;
    let (bounced_id, _) = f
        .store
        .store_outgoing(bounced)
        .await
        .map(|m| (m.id, m.token))
        .unwrap();
    sink.record_delivery(f.server.id, bounced_id, "Bounced", "550", "", false, None)
        .await
        .unwrap();

    let mut theirs = message_for(other.id, "c@dest.example");
    theirs.scope = MessageScope::Outgoing;
    f.store.store_outgoing(theirs).await.unwrap();

    // stats for our tenant
    let stats = f
        .store
        .message_stats(f.server.id, &StatsFilter::default())
        .await
        .unwrap();
    assert_eq!(stats.total, 2);
    assert_eq!(stats.outgoing, 2);
    assert_eq!(stats.sent, 1);
    assert_eq!(stats.bounced, 1);
    assert_eq!(stats.opens, 1);
    assert_eq!(stats.unique_opens, 1);
    assert_eq!(stats.clicks, 1);
    assert_eq!(stats.unique_clicks, 1);

    // bounces list contains only the bounced message
    let bounces = f
        .store
        .bounces(f.server.id, &MessageFilter::default())
        .await
        .unwrap();
    assert_eq!(bounces.len(), 1);
    assert_eq!(bounces[0].id, bounced_id);
    assert!(f
        .store
        .bounce(f.server.id, bounced_id)
        .await
        .unwrap()
        .is_some());
    assert!(f
        .store
        .bounce(f.server.id, sent_id)
        .await
        .unwrap()
        .is_none());

    // delivery stats read the queue (each store_outgoing enqueued one row)
    let queue = f.store.delivery_stats(f.server.id).await.unwrap();
    assert_eq!(queue.queued, 2);

    // the other tenant sees only its own data
    let their_stats = f
        .store
        .message_stats(other.id, &StatsFilter::default())
        .await
        .unwrap();
    assert_eq!(their_stats.total, 1);
    assert_eq!(their_stats.sent, 0);
    assert!(f
        .store
        .bounces(other.id, &MessageFilter::default())
        .await
        .unwrap()
        .is_empty());
    assert!(f
        .store
        .bounce(other.id, bounced_id)
        .await
        .unwrap()
        .is_none());
    assert_eq!(f.store.delivery_stats(other.id).await.unwrap().queued, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_streams_default_crud_and_scoping() {
    use camelmailer_core::{MessageFilter, NewStream, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let other = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();

    // create_server backfilled a default transactional stream + pointer
    let streams = f.store.list_streams(f.server.id).await.unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].permalink, "outbound");
    assert_eq!(f.server.default_stream_id, Some(streams[0].id));

    // create a second stream, duplicate permalink rejected
    let broadcast = f
        .store
        .create_stream(NewStream {
            server_id: f.server.id,
            name: "Broadcasts".into(),
            permalink: "broadcasts".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        })
        .await
        .unwrap();
    assert!(f
        .store
        .create_stream(NewStream {
            server_id: f.server.id,
            name: "Dup".into(),
            permalink: "broadcasts".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        })
        .await
        .is_err());

    // send a message into the broadcast stream and filter by it
    let mut msg = message_for(f.server.id, "a@dest.example");
    msg.scope = MessageScope::Outgoing;
    msg.stream_id = Some(broadcast.id);
    let (msg_id, _) = f
        .store
        .store_outgoing(msg)
        .await
        .map(|m| (m.id, m.token))
        .unwrap();

    let in_stream = f
        .store
        .messages(
            f.server.id,
            &MessageFilter {
                stream_id: Some(broadcast.id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(in_stream.len(), 1);
    assert_eq!(in_stream[0].id, msg_id);
    assert_eq!(in_stream[0].stream_id, Some(broadcast.id));

    // archive via update_stream
    let archived = f
        .store
        .update_stream(camelmailer_core::MessageStream {
            archived: true,
            ..broadcast.clone()
        })
        .await
        .unwrap();
    assert!(archived.archived);
    assert!(
        f.store
            .stream_by_permalink(f.server.id, "broadcasts")
            .await
            .unwrap()
            .unwrap()
            .archived
    );

    // the other tenant sees only its own (default) stream
    let their_streams = f.store.list_streams(other.id).await.unwrap();
    assert_eq!(their_streams.len(), 1);
    assert_eq!(their_streams[0].permalink, "outbound");
    assert!(f
        .store
        .stream_by_permalink(other.id, "broadcasts")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_ip_pool_round_trips_and_resolves_source_ip() {
    use camelmailer_core::{NewIpAddress, NewStream, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    // Server's own pool (today's behaviour): lowest priority number wins.
    let server_pool = f.store.create_ip_pool("server", false).await.unwrap();
    f.store
        .create_ip_address(NewIpAddress {
            ip_pool_id: server_pool.id,
            ipv4: "10.0.0.9".into(),
            ipv6: None,
            hostname: "b.example".into(),
            priority: 5,
        })
        .await
        .unwrap();
    f.store
        .create_ip_address(NewIpAddress {
            ip_pool_id: server_pool.id,
            ipv4: "10.0.0.1".into(),
            ipv6: None,
            hostname: "a.example".into(),
            priority: 0,
        })
        .await
        .unwrap();
    f.store
        .set_server_ip_pool(f.server.id, Some(server_pool.id))
        .await
        .unwrap();

    // A separate broadcast pool for the stream to source from.
    let stream_pool = f.store.create_ip_pool("stream", false).await.unwrap();
    f.store
        .create_ip_address(NewIpAddress {
            ip_pool_id: stream_pool.id,
            ipv4: "10.0.0.2".into(),
            ipv6: None,
            hostname: "c.example".into(),
            priority: 0,
        })
        .await
        .unwrap();

    // create_stream with an ip_pool_id round-trips and persists.
    let with_pool = f
        .store
        .create_stream(NewStream {
            server_id: f.server.id,
            name: "Broadcast".into(),
            permalink: "broadcast".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: Some(stream_pool.id),
        })
        .await
        .unwrap();
    assert_eq!(with_pool.ip_pool_id, Some(stream_pool.id));
    assert_eq!(
        f.store
            .stream_by_permalink(f.server.id, "broadcast")
            .await
            .unwrap()
            .unwrap()
            .ip_pool_id,
        Some(stream_pool.id)
    );

    let without_pool = f
        .store
        .create_stream(NewStream {
            server_id: f.server.id,
            name: "Transactional".into(),
            permalink: "txn".into(),
            stream_type: "transactional".into(),
            ip_pool_id: None,
        })
        .await
        .unwrap();
    assert_eq!(without_pool.ip_pool_id, None);

    // The stream's own pool wins when set.
    assert_eq!(
        ServerStore::source_ip_for(&f.store, f.server.id, Some(with_pool.id))
            .await
            .unwrap(),
        Some("10.0.0.2".to_string())
    );
    // A stream without a pool falls back to the server pool exactly like the
    // legacy source_ip_for_server (which is IP-typed).
    assert_eq!(
        ServerStore::source_ip_for(&f.store, f.server.id, Some(without_pool.id))
            .await
            .unwrap(),
        Some("10.0.0.1".to_string())
    );
    assert_eq!(
        ServerStore::source_ip_for(&f.store, f.server.id, None)
            .await
            .unwrap(),
        Some("10.0.0.1".to_string())
    );
    assert_eq!(
        f.store.source_ip_for_server(f.server.id).await,
        Some("10.0.0.1".parse().unwrap())
    );

    // Clearing the server pool leaves no source when the stream has none.
    f.store.set_server_ip_pool(f.server.id, None).await.unwrap();
    assert_eq!(
        ServerStore::source_ip_for(&f.store, f.server.id, None)
            .await
            .unwrap(),
        None
    );
    // But the broadcast stream still resolves via its own pool.
    assert_eq!(
        ServerStore::source_ip_for(&f.store, f.server.id, Some(with_pool.id))
            .await
            .unwrap(),
        Some("10.0.0.2".to_string())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_bypass_retry_requeue_and_scope() {
    use camelmailer_core::{MessageFilter, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;
    let other = f
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

    // an incoming message that has already been drained from the queue
    let incoming = message_for(f.server.id, "support@tenant-a.example");
    let id = sink.insert_message(&incoming).await.unwrap();
    sqlx::query("DELETE FROM queued_messages WHERE message_id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    // and an outgoing message for the same server
    let mut outgoing = message_for(f.server.id, "x@dest.example");
    outgoing.scope = MessageScope::Outgoing;
    let out_id = sink.insert_message(&outgoing).await.unwrap();

    // inbound list carries only the incoming message
    let inbound = f
        .store
        .inbound_messages(f.server.id, &MessageFilter::default())
        .await
        .unwrap();
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0].id, id);

    // bypass re-queues it and flags it
    let bypassed = f
        .store
        .bypass_message(f.server.id, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bypassed.status, "Pending");
    assert!(bypassed.bypassed);
    let queued: i64 =
        sqlx::query("SELECT count(*) AS c FROM queued_messages WHERE message_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("c");
    assert_eq!(queued, 1);

    // retry on an outgoing message is rejected (scope guard)
    assert!(f
        .store
        .retry_message(f.server.id, out_id)
        .await
        .unwrap()
        .is_none());

    // the other tenant can neither see nor requeue server A's inbound message
    assert!(f
        .store
        .inbound_messages(other.id, &MessageFilter::default())
        .await
        .unwrap()
        .is_empty());
    assert!(f
        .store
        .inbound_message(other.id, id)
        .await
        .unwrap()
        .is_none());
    assert!(f
        .store
        .bypass_message(other.id, id)
        .await
        .unwrap()
        .is_none());
    // no extra queue row was created by the rejected cross-tenant bypass
    let queued: i64 =
        sqlx::query("SELECT count(*) AS c FROM queued_messages WHERE message_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("c");
    assert_eq!(queued, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn templates_crud_and_scoping() {
    use camelmailer_core::{NewTemplate, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let other = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();

    let template = f
        .store
        .create_template(NewTemplate {
            server_id: f.server.id,
            name: "Welcome".into(),
            permalink: "welcome".into(),
            subject: Some("Hi {{ name }}".into()),
            html_body: Some("<p>{{ name }}</p>".into()),
            text_body: None,
            layout_id: None,
        })
        .await
        .unwrap();
    assert_eq!(template.permalink, "welcome");

    // duplicate permalink rejected
    assert!(f
        .store
        .create_template(NewTemplate {
            server_id: f.server.id,
            name: "Dup".into(),
            permalink: "welcome".into(),
            subject: None,
            html_body: None,
            text_body: None,
            layout_id: None,
        })
        .await
        .is_err());

    // update (archive + change subject)
    let updated = f
        .store
        .update_template(camelmailer_core::Template {
            subject: Some("Hello {{ name }}".into()),
            archived: true,
            ..template.clone()
        })
        .await
        .unwrap();
    assert!(updated.archived);
    let reloaded = f
        .store
        .template_by_permalink(f.server.id, "welcome")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.subject.as_deref(), Some("Hello {{ name }}"));
    assert!(reloaded.archived);

    // the other tenant sees nothing
    assert!(f.store.list_templates(other.id).await.unwrap().is_empty());
    assert!(f
        .store
        .template_by_permalink(other.id, "welcome")
        .await
        .unwrap()
        .is_none());
}

// ------------------------------------------------------------------- auth

#[tokio::test(flavor = "multi_thread")]
async fn pg_auth_account_state_round_trips() {
    use camelmailer_core::{AuthStore, NewUser};
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;
    let user = f
        .store
        .create_user(NewUser {
            email_address: "Ada@Example.com".into(),
            first_name: "Ada".into(),
            last_name: "L".into(),
            admin: false,
        })
        .await
        .unwrap();

    // duplicate email → Conflict (same behaviour as MemoryStore)
    let error = f
        .store
        .create_user(NewUser {
            email_address: "Ada@Example.com".into(),
            first_name: "Ada".into(),
            last_name: "L".into(),
            admin: false,
        })
        .await
        .expect_err("duplicate email must conflict");
    assert!(matches!(error, camelmailer_core::StoreError::Conflict(_)));

    // case-insensitive email lookup
    let found = f.store.user_by_email("ada@example.COM").await.unwrap();
    assert_eq!(found.unwrap().id, user.id);
    assert!(f.store.user_by_email("nobody@x").await.unwrap().is_none());

    // defaults exist without a user_auth row
    let auth = f.store.user_auth(user.id).await.unwrap().unwrap();
    assert_eq!(auth.password_digest, None);
    assert!(!auth.totp_enabled);
    assert!(f.store.user_auth(999_999).await.unwrap().is_none());

    f.store
        .set_password_digest(user.id, "$argon2id$test")
        .await
        .unwrap();
    f.store
        .set_totp(user.id, Some("SECRET32"), true)
        .await
        .unwrap();
    let locked = chrono::Utc::now() + chrono::Duration::minutes(15);
    f.store
        .set_login_state(user.id, 3, Some(locked), None)
        .await
        .unwrap();
    let auth = f.store.user_auth(user.id).await.unwrap().unwrap();
    assert_eq!(auth.password_digest.as_deref(), Some("$argon2id$test"));
    assert_eq!(auth.totp_secret.as_deref(), Some("SECRET32"));
    assert!(auth.totp_enabled);
    assert_eq!(auth.failed_login_attempts, 3);
    assert!(auth.locked_until.is_some());
    // last_login_at survives a state write that passes None
    f.store
        .set_login_state(user.id, 0, None, Some(chrono::Utc::now()))
        .await
        .unwrap();
    f.store
        .set_login_state(user.id, 1, None, None)
        .await
        .unwrap();
    let auth = f.store.user_auth(user.id).await.unwrap().unwrap();
    assert!(auth.last_login_at.is_some());
    assert_eq!(auth.failed_login_attempts, 1);

    // disabled flag round trip (SCIM active=false)
    assert!(!auth.disabled);
    f.store.set_user_disabled(user.id, true).await.unwrap();
    assert!(f.store.user_auth(user.id).await.unwrap().unwrap().disabled);
    f.store.set_user_disabled(user.id, false).await.unwrap();
    assert!(!f.store.user_auth(user.id).await.unwrap().unwrap().disabled);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_organization_require_two_factor_round_trips() {
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;

    // defaults to off (same as MemoryStore)
    assert!(!f.organization.require_two_factor);
    let reread = f
        .store
        .organization_by_permalink("example-org")
        .await
        .unwrap()
        .unwrap();
    assert!(!reread.require_two_factor);

    // update_organization round trip
    let updated = f
        .store
        .update_organization(camelmailer_core::Organization {
            require_two_factor: true,
            ..f.organization.clone()
        })
        .await
        .unwrap();
    assert!(updated.require_two_factor);
    let reread = f
        .store
        .organization_by_permalink("example-org")
        .await
        .unwrap()
        .unwrap();
    assert!(reread.require_two_factor);
    assert_eq!(reread.name, "Example Org");

    // the joined membership lookup carries the flag too
    {
        use camelmailer_core::{AuthStore, NewUser, Role};
        let user = f
            .store
            .create_user(NewUser {
                email_address: "member@example.com".into(),
                first_name: "M".into(),
                last_name: "Ember".into(),
                admin: false,
            })
            .await
            .unwrap();
        f.store
            .upsert_membership(f.organization.id, user.id, Role::Member)
            .await
            .unwrap();
        let memberships = f.store.memberships_for_user(user.id).await.unwrap();
        assert_eq!(memberships.len(), 1);
        assert!(memberships[0].1.require_two_factor);
    }

    // a permalink collision on update is a conflict, like on create
    let other = f
        .store
        .create_organization(NewOrganization {
            name: "Other".into(),
            permalink: "other-org".into(),
        })
        .await
        .unwrap();
    let error = f
        .store
        .update_organization(camelmailer_core::Organization {
            permalink: "example-org".into(),
            ..other
        })
        .await
        .expect_err("duplicate permalink must conflict");
    assert!(matches!(error, camelmailer_core::StoreError::Conflict(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_user_has_two_factor_covers_totp_and_passkeys() {
    use camelmailer_core::{AuthStore, NewUser, NewWebAuthnCredential};
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;
    let user = f
        .store
        .create_user(NewUser {
            email_address: "2fa@example.com".into(),
            first_name: "Two".into(),
            last_name: "Factor".into(),
            admin: false,
        })
        .await
        .unwrap();

    assert!(!f.store.user_has_two_factor(user.id).await.unwrap());

    // enrolled but not activated does not count
    f.store
        .set_totp(user.id, Some("SECRET32"), false)
        .await
        .unwrap();
    assert!(!f.store.user_has_two_factor(user.id).await.unwrap());

    // activated TOTP counts, disabling clears it again
    f.store
        .set_totp(user.id, Some("SECRET32"), true)
        .await
        .unwrap();
    assert!(f.store.user_has_two_factor(user.id).await.unwrap());
    f.store.set_totp(user.id, None, false).await.unwrap();
    assert!(!f.store.user_has_two_factor(user.id).await.unwrap());

    // a registered passkey counts on its own
    let credential = f
        .store
        .add_webauthn_credential(NewWebAuthnCredential {
            user_id: user.id,
            name: "MacBook".into(),
            credential_id: "cred-2fa".into(),
            credential_json: "{}".into(),
        })
        .await
        .unwrap();
    assert!(f.store.user_has_two_factor(user.id).await.unwrap());
    f.store
        .delete_webauthn_credential(user.id, credential.id)
        .await
        .unwrap();
    assert!(!f.store.user_has_two_factor(user.id).await.unwrap());

    // unknown users simply have no second factor
    assert!(!f.store.user_has_two_factor(999_999).await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_saml_requests_and_assertion_replay_cache() {
    use camelmailer_core::AuthStore;
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;
    let now = chrono::Utc::now();

    // request ids are single use and expire
    f.store
        .create_saml_request("_req-1", now + chrono::Duration::minutes(10))
        .await
        .unwrap();
    assert!(f.store.consume_saml_request("_req-1", now).await.unwrap());
    assert!(!f.store.consume_saml_request("_req-1", now).await.unwrap());
    assert!(!f.store.consume_saml_request("_nope", now).await.unwrap());
    f.store
        .create_saml_request("_req-2", now - chrono::Duration::minutes(1))
        .await
        .unwrap();
    assert!(!f.store.consume_saml_request("_req-2", now).await.unwrap());

    // assertion replay cache
    let expires = now + chrono::Duration::minutes(5);
    assert!(f
        .store
        .register_saml_assertion("_a1", expires, now)
        .await
        .unwrap());
    assert!(!f
        .store
        .register_saml_assertion("_a1", expires, now)
        .await
        .unwrap());
    assert!(f
        .store
        .register_saml_assertion("_a2", expires, now)
        .await
        .unwrap());
    // after expiry the id may be seen again (cache cleanup)
    assert!(f
        .store
        .register_saml_assertion("_a1", expires, now + chrono::Duration::minutes(6))
        .await
        .unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_auth_sessions_lifecycle() {
    use camelmailer_core::{AuthStore, NewAuthSession, NewUser};
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;
    let user = f
        .store
        .create_user(NewUser {
            email_address: "s@example.com".into(),
            first_name: "S".into(),
            last_name: "U".into(),
            admin: false,
        })
        .await
        .unwrap();

    let expires = chrono::Utc::now() + chrono::Duration::days(14);
    let session = f
        .store
        .create_session(NewAuthSession {
            user_id: user.id,
            token_hash: "hash-1".into(),
            expires_at: expires,
            ip_address: Some("10.1.2.3".into()),
            user_agent: Some("tests".into()),
        })
        .await
        .unwrap();

    let (found, found_user) = f.store.session_with_user("hash-1").await.unwrap().unwrap();
    assert_eq!(found.id, session.id);
    assert_eq!(found_user.id, user.id);
    assert!(f.store.session_with_user("nope").await.unwrap().is_none());

    let new_expiry = expires + chrono::Duration::days(1);
    f.store
        .touch_session(session.id, chrono::Utc::now(), new_expiry)
        .await
        .unwrap();
    let (touched, _) = f.store.session_with_user("hash-1").await.unwrap().unwrap();
    assert!((touched.expires_at - new_expiry).num_seconds().abs() < 2);

    assert!(f.store.delete_session("hash-1").await.unwrap());
    assert!(!f.store.delete_session("hash-1").await.unwrap());

    for n in 0..2 {
        f.store
            .create_session(NewAuthSession {
                user_id: user.id,
                token_hash: format!("bulk-{n}"),
                expires_at: expires,
                ip_address: None,
                user_agent: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(f.store.delete_sessions_for_user(user.id).await.unwrap(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_auth_memberships_and_invitations() {
    use camelmailer_core::{AuthStore, NewInvitation, NewUser, Role, StoreError};
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;
    let user = f
        .store
        .create_user(NewUser {
            email_address: "m@example.com".into(),
            first_name: "M".into(),
            last_name: "U".into(),
            admin: false,
        })
        .await
        .unwrap();

    let membership = f
        .store
        .upsert_membership(f.organization.id, user.id, Role::Member)
        .await
        .unwrap();
    assert_eq!(membership.role, Role::Member);
    let updated = f
        .store
        .upsert_membership(f.organization.id, user.id, Role::Owner)
        .await
        .unwrap();
    assert_eq!(updated.id, membership.id);
    assert_eq!(updated.role, Role::Owner);

    let mine = f.store.memberships_for_user(user.id).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].1.id, f.organization.id);
    let members = f
        .store
        .memberships_for_organization(f.organization.id)
        .await
        .unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].1.id, user.id);
    assert!(f
        .store
        .membership(f.organization.id, user.id)
        .await
        .unwrap()
        .is_some());

    // invitations: pending duplicates conflict (case-insensitive)
    let new_invite = |email: &str, hash: &str| NewInvitation {
        organization_id: f.organization.id,
        email_address: email.to_string(),
        role: Role::Member,
        token_hash: hash.to_string(),
        invited_by_user_id: user.id,
        expires_at: chrono::Utc::now() + chrono::Duration::days(7),
    };
    let invitation = f
        .store
        .create_invitation(new_invite("new@example.com", "inv-1"))
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .create_invitation(new_invite("NEW@example.com", "inv-2"))
            .await,
        Err(StoreError::Conflict(_))
    ));
    let found = f
        .store
        .invitation_by_token_hash("inv-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, invitation.id);
    f.store
        .mark_invitation_accepted(invitation.id)
        .await
        .unwrap();
    f.store
        .create_invitation(new_invite("new@example.com", "inv-3"))
        .await
        .unwrap();
    assert_eq!(
        f.store
            .list_invitations(f.organization.id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(!f
        .store
        .delete_invitation(999_999, invitation.id)
        .await
        .unwrap());
    assert!(f
        .store
        .delete_invitation(f.organization.id, invitation.id)
        .await
        .unwrap());

    assert!(f
        .store
        .delete_membership(f.organization.id, user.id)
        .await
        .unwrap());
    assert!(!f
        .store
        .delete_membership(f.organization.id, user.id)
        .await
        .unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_auth_resets_oidc_and_audit() {
    use camelmailer_core::{AuthStore, NewAuthEvent, NewUser};
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;
    let user = f
        .store
        .create_user(NewUser {
            email_address: "r@example.com".into(),
            first_name: "R".into(),
            last_name: "U".into(),
            admin: false,
        })
        .await
        .unwrap();

    // password resets are single-use and expire
    f.store
        .create_password_reset(
            user.id,
            "reset-1",
            chrono::Utc::now() + chrono::Duration::hours(2),
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .consume_password_reset("reset-1", chrono::Utc::now())
            .await
            .unwrap(),
        Some(user.id)
    );
    assert_eq!(
        f.store
            .consume_password_reset("reset-1", chrono::Utc::now())
            .await
            .unwrap(),
        None
    );
    f.store
        .create_password_reset(
            user.id,
            "reset-2",
            chrono::Utc::now() - chrono::Duration::hours(1),
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .consume_password_reset("reset-2", chrono::Utc::now())
            .await
            .unwrap(),
        None
    );

    // oidc sub linking + one-shot states
    assert!(f.store.user_by_oidc_sub("sub-1").await.unwrap().is_none());
    f.store.set_oidc_sub(user.id, "sub-1").await.unwrap();
    assert_eq!(
        f.store.user_by_oidc_sub("sub-1").await.unwrap().unwrap().id,
        user.id
    );
    f.store
        .create_oidc_state(
            "state-1",
            "verifier",
            "nonce",
            chrono::Utc::now() + chrono::Duration::minutes(10),
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .consume_oidc_state("state-1", chrono::Utc::now())
            .await
            .unwrap(),
        Some(("verifier".into(), "nonce".into()))
    );
    assert_eq!(
        f.store
            .consume_oidc_state("state-1", chrono::Utc::now())
            .await
            .unwrap(),
        None
    );

    // social SSO identities: several providers per account, upsert per
    // provider, foreign subjects conflict
    assert!(f
        .store
        .user_by_sso_identity("google", "g-1")
        .await
        .unwrap()
        .is_none());
    f.store
        .link_sso_identity(user.id, "google", "g-1")
        .await
        .unwrap();
    f.store
        .link_sso_identity(user.id, "github", "h-1")
        .await
        .unwrap();
    assert_eq!(
        f.store
            .user_by_sso_identity("google", "g-1")
            .await
            .unwrap()
            .unwrap()
            .id,
        user.id
    );
    assert!(f
        .store
        .user_by_sso_identity("github", "g-1")
        .await
        .unwrap()
        .is_none());
    f.store
        .link_sso_identity(user.id, "google", "g-2")
        .await
        .unwrap();
    assert!(f
        .store
        .user_by_sso_identity("google", "g-1")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        f.store
            .user_by_sso_identity("google", "g-2")
            .await
            .unwrap()
            .unwrap()
            .id,
        user.id
    );
    let other = f
        .store
        .create_user(NewUser {
            email_address: "other@example.com".into(),
            first_name: "O".into(),
            last_name: "U".into(),
            admin: false,
        })
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .link_sso_identity(other.id, "google", "g-2")
            .await
            .unwrap_err(),
        camelmailer_core::StoreError::Conflict(_)
    ));

    // audit log, newest first
    for event in ["login.success", "logout"] {
        f.store
            .record_auth_event(NewAuthEvent {
                user_id: Some(user.id),
                email_address: Some("r@example.com".into()),
                event: event.into(),
                ip_address: Some("10.0.0.9".into()),
                user_agent: None,
            })
            .await
            .unwrap();
    }
    let events = f.store.list_auth_events(10).await.unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, "logout");
    assert_eq!(events[1].event, "login.success");
}

// ------------------------------------- webhook events + custom headers

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_events_and_headers_roundtrip() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let mut headers = std::collections::BTreeMap::new();
    headers.insert("Authorization".to_string(), "Bearer hunter2".to_string());
    let webhook = f
        .store
        .create_webhook(camelmailer_core::NewWebhook {
            server_id: f.server.id,
            name: "hook".into(),
            url: "https://hooks.example/cb".into(),
            all_events: false,
            sign: true,
            events: vec!["MessageSent".into(), "MessageHeld".into()],
            headers: headers.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        webhook.events,
        vec!["MessageSent".to_string(), "MessageHeld".to_string()]
    );
    assert_eq!(webhook.headers, headers);

    let listed = f.store.list_webhooks(f.server.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].events, webhook.events);
    assert_eq!(listed[0].headers, headers);
    assert!(listed[0].subscribes_to("MessageSent"));
    assert!(!listed[0].subscribes_to("MessageDelayed"));

    // update replaces both fields
    let mut updated = webhook.clone();
    updated.events = Vec::new();
    updated.headers.insert("X-Extra".into(), "1".into());
    f.store.update_webhook(updated).await.unwrap();
    let fetched = f
        .store
        .webhook_by_id(f.server.id, webhook.id)
        .await
        .unwrap()
        .unwrap();
    assert!(fetched.events.is_empty());
    assert_eq!(
        fetched.headers.get("X-Extra").map(String::as_str),
        Some("1")
    );
    assert!(fetched.subscribes_to("MessageDelayed"));

    // a pre-events webhook shape defaults to all events, no headers
    let broad = f
        .store
        .create_webhook(camelmailer_core::NewWebhook::all(
            f.server.id,
            "all",
            "https://hooks.example/all",
            false,
        ))
        .await
        .unwrap();
    assert!(broad.events.is_empty());
    assert!(broad.headers.is_empty());
}

// ---------------------------------------------------- sender addresses

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_addresses_create_confirm_and_authorize() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let address = f
        .store
        .create_sender_address(camelmailer_core::NewSenderAddress {
            server_id: f.server.id,
            email_address: "solo@external.example".into(),
            verification_token_hash: "hash-1".into(),
        })
        .await
        .unwrap();
    assert!(!address.verified);

    // duplicates conflict
    let error = f
        .store
        .create_sender_address(camelmailer_core::NewSenderAddress {
            server_id: f.server.id,
            email_address: "solo@external.example".into(),
            verification_token_hash: "hash-2".into(),
        })
        .await
        .expect_err("duplicate must conflict");
    assert!(matches!(error, camelmailer_core::StoreError::Conflict(_)));

    // pending addresses do not authorize (async and sync paths)
    assert!(!f
        .store
        .confirmed_sender_address(f.server.id, "solo@external.example")
        .await
        .unwrap());
    assert!(!f
        .store
        .find_confirmed_sender_address(f.server.id, &["solo@external.example"]));

    // an unknown token confirms nothing; the right one is single-use
    assert!(f
        .store
        .confirm_sender_address("wrong")
        .await
        .unwrap()
        .is_none());
    let confirmed = f
        .store
        .confirm_sender_address("hash-1")
        .await
        .unwrap()
        .expect("token must confirm");
    assert!(confirmed.verified);
    assert!(confirmed.verification_token_hash.is_none());
    assert!(f
        .store
        .confirm_sender_address("hash-1")
        .await
        .unwrap()
        .is_none());

    // confirmed: authorizes case-insensitively, exact address, per server
    assert!(f
        .store
        .confirmed_sender_address(f.server.id, "Solo@External.example")
        .await
        .unwrap());
    assert!(!f
        .store
        .confirmed_sender_address(f.server.id, "other@external.example")
        .await
        .unwrap());
    assert!(f
        .store
        .find_confirmed_sender_address(f.server.id, &["Solo <solo@external.example>"]));
    assert!(!f.store.find_confirmed_sender_address(f.server.id, &[]));

    // list + scoped fetch + delete
    let listed = f.store.list_sender_addresses(f.server.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].verified);
    assert!(f
        .store
        .sender_address_by_id(f.server.id, address.id)
        .await
        .unwrap()
        .is_some());
    assert!(f
        .store
        .sender_address_by_id(f.server.id + 1, address.id)
        .await
        .unwrap()
        .is_none());
    assert!(f.store.delete_sender_address(address.id).await.unwrap());
    assert!(!f.store.delete_sender_address(address.id).await.unwrap());
    assert!(!f
        .store
        .confirmed_sender_address(f.server.id, "solo@external.example")
        .await
        .unwrap());
}

// ------------------------------------------------------------ billing

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn billing_customer_id_roundtrip() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    // Absent by default (nullable column).
    assert_eq!(
        f.store
            .organization_billing_customer_id(f.organization.id)
            .await
            .unwrap(),
        None
    );

    // Set, read back, overwrite (idempotent customer reuse).
    f.store
        .set_organization_billing_customer_id(f.organization.id, "cus_123")
        .await
        .unwrap();
    assert_eq!(
        f.store
            .organization_billing_customer_id(f.organization.id)
            .await
            .unwrap()
            .as_deref(),
        Some("cus_123")
    );
    f.store
        .set_organization_billing_customer_id(f.organization.id, "cus_456")
        .await
        .unwrap();
    assert_eq!(
        f.store
            .organization_billing_customer_id(f.organization.id)
            .await
            .unwrap()
            .as_deref(),
        Some("cus_456")
    );

    // Unknown organizations: get is None, set is an error — the same
    // behaviour as MemoryStore.
    assert_eq!(
        f.store
            .organization_billing_customer_id(9999)
            .await
            .unwrap(),
        None
    );
    assert!(f
        .store
        .set_organization_billing_customer_id(9999, "cus_x")
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_webauthn_credentials_and_states() {
    use camelmailer_core::{AuthStore, NewUser, NewWebAuthnCredential, StoreError};
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;
    let user = f
        .store
        .create_user(NewUser {
            email_address: "w@example.com".into(),
            first_name: "W".into(),
            last_name: "U".into(),
            admin: false,
        })
        .await
        .unwrap();
    let other = f
        .store
        .create_user(NewUser {
            email_address: "o@example.com".into(),
            first_name: "O".into(),
            last_name: "U".into(),
            admin: false,
        })
        .await
        .unwrap();
    let new = |user_id, name: &str, credential_id: &str| NewWebAuthnCredential {
        user_id,
        name: name.to_string(),
        credential_id: credential_id.to_string(),
        credential_json: "{\"cred\":\"data\"}".to_string(),
    };

    // add + duplicate credential id conflicts (also across users)
    let credential = f
        .store
        .add_webauthn_credential(new(user.id, "MacBook", "cred-a"))
        .await
        .unwrap();
    assert_eq!(credential.user_id, user.id);
    assert_eq!(credential.name, "MacBook");
    assert_eq!(credential.last_used_at, None);
    f.store
        .add_webauthn_credential(new(user.id, "YubiKey", "cred-b"))
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .add_webauthn_credential(new(other.id, "Clone", "cred-a"))
            .await,
        Err(StoreError::Conflict(_))
    ));

    // list is per-user, oldest first
    let mine = f.store.list_webauthn_credentials(user.id).await.unwrap();
    assert_eq!(mine.len(), 2);
    assert_eq!(mine[0].name, "MacBook");
    assert_eq!(mine[1].name, "YubiKey");
    assert!(f
        .store
        .list_webauthn_credentials(other.id)
        .await
        .unwrap()
        .is_empty());

    // lookup by credential id
    let found = f
        .store
        .webauthn_credential_by_credential_id("cred-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, credential.id);
    assert!(f
        .store
        .webauthn_credential_by_credential_id("nope")
        .await
        .unwrap()
        .is_none());

    // counter/backup-flag update stamps last_used_at
    f.store
        .update_webauthn_credential(credential.id, "{\"cred\":\"updated\"}", chrono::Utc::now())
        .await
        .unwrap();
    let found = f
        .store
        .webauthn_credential_by_credential_id("cred-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.credential_json, "{\"cred\":\"updated\"}");
    assert!(found.last_used_at.is_some());

    // deletion is scoped to the owner
    assert!(!f
        .store
        .delete_webauthn_credential(other.id, credential.id)
        .await
        .unwrap());
    assert!(f
        .store
        .delete_webauthn_credential(user.id, credential.id)
        .await
        .unwrap());
    assert!(!f
        .store
        .delete_webauthn_credential(user.id, credential.id)
        .await
        .unwrap());

    // ceremony states: upsert on the same key, single use, expiring
    f.store
        .create_webauthn_state(
            "login:chal-1",
            Some(user.id),
            "{\"state\":1}",
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();
    f.store
        .create_webauthn_state(
            "login:chal-1",
            Some(user.id),
            "{\"state\":2}",
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .consume_webauthn_state("login:chal-1", chrono::Utc::now())
            .await
            .unwrap(),
        Some((Some(user.id), "{\"state\":2}".into()))
    );
    assert_eq!(
        f.store
            .consume_webauthn_state("login:chal-1", chrono::Utc::now())
            .await
            .unwrap(),
        None
    );
    f.store
        .create_webauthn_state(
            "reg:chal-2",
            None,
            "{}",
            chrono::Utc::now() - chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .consume_webauthn_state("reg:chal-2", chrono::Utc::now())
            .await
            .unwrap(),
        None
    );
}

// ------------------------------------------------- DMARC reports (RLS)

fn dmarc_report_for(
    server_id: camelmailer_core::Id,
    domain: &str,
    external_id: &str,
) -> camelmailer_core::NewDmarcReport {
    let now = chrono::Utc::now();
    camelmailer_core::NewDmarcReport {
        server_id,
        domain: domain.into(),
        org_name: Some("google.com".into()),
        org_email: Some("noreply@google.com".into()),
        report_id: external_id.into(),
        date_range_begin: now - chrono::Duration::days(1),
        date_range_end: now,
        records: vec![
            camelmailer_core::NewDmarcRecord {
                source_ip: "203.0.113.10".into(),
                count: 7,
                disposition: "none".into(),
                dkim_result: Some("pass".into()),
                spf_result: Some("pass".into()),
                dkim_aligned: true,
                spf_aligned: true,
                header_from: Some(domain.to_string()),
                envelope_from: None,
            },
            camelmailer_core::NewDmarcRecord {
                source_ip: "198.51.100.7".into(),
                count: 3,
                disposition: "quarantine".into(),
                dkim_result: Some("fail".into()),
                spf_result: Some("softfail".into()),
                dkim_aligned: false,
                spf_aligned: false,
                header_from: Some(domain.to_string()),
                envelope_from: Some("spoof.example".into()),
            },
        ],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dmarc_reports_are_tenant_isolated_by_rls() {
    use camelmailer_core::ServerStore;

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

    let mine = f
        .store
        .store_dmarc_report(dmarc_report_for(f.server.id, "tenant-a.example", "a-1"))
        .await
        .unwrap();
    f.store
        .store_dmarc_report(dmarc_report_for(other_server.id, "tenant-b.example", "b-1"))
        .await
        .unwrap();

    // each tenant lists exactly its own reports (queries carry no WHERE
    // server_id — RLS does the filtering)
    let filter = camelmailer_core::DmarcFilter::default();
    let tenant_a = f.store.dmarc_reports(f.server.id, &filter).await.unwrap();
    assert_eq!(tenant_a.len(), 1);
    assert_eq!(tenant_a[0].domain, "tenant-a.example");
    assert_eq!(tenant_a[0].record_count, 2);
    let tenant_b = f
        .store
        .dmarc_reports(other_server.id, &filter)
        .await
        .unwrap();
    assert_eq!(tenant_b.len(), 1);
    assert_eq!(tenant_b[0].domain, "tenant-b.example");

    // a foreign report id resolves to nothing (indistinguishable from
    // not existing)
    assert!(f
        .store
        .dmarc_report(other_server.id, mine.id)
        .await
        .unwrap()
        .is_none());
    let (report, records) = f
        .store
        .dmarc_report(f.server.id, mine.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.report_id, "a-1");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].source_ip, "203.0.113.10");
    assert!(records[0].dkim_aligned);
    assert_eq!(records[1].disposition, "quarantine");

    // record queries are scoped the same way
    let rows_a = f.store.dmarc_records(f.server.id, &filter).await.unwrap();
    assert_eq!(rows_a.len(), 2);
    let rows_b = f
        .store
        .dmarc_records(other_server.id, &filter)
        .await
        .unwrap();
    assert_eq!(rows_b.len(), 2);
    assert!(rows_b.iter().all(|r| r.report_id != mine.id));

    // the domain filter narrows within the tenant
    let filtered = f
        .store
        .dmarc_reports(
            f.server.id,
            &camelmailer_core::DmarcFilter {
                domain: Some("tenant-b.example".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(filtered.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dmarc_tables_hide_all_rows_without_a_tenant_context() {
    use camelmailer_core::ServerStore;

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;
    f.store
        .store_dmarc_report(dmarc_report_for(f.server.id, "tenant-a.example", "a-1"))
        .await
        .unwrap();

    // FORCE ROW LEVEL SECURITY: even the table owner sees nothing
    // without the tenant context — on both tables.
    for table in ["dmarc_reports", "dmarc_report_records"] {
        let count: i64 = sqlx::query(&format!("SELECT count(*) AS c FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("c");
        assert_eq!(count, 0, "{table} must be empty without a tenant context");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dmarc_tables_reject_writes_for_a_foreign_tenant() {
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

    // Insert a report for server A inside server B's tenant context —
    // the WITH CHECK clause must reject it.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('camelmailer.server_id', $1, true)")
        .bind(other_server.id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let result = sqlx::query(
        "INSERT INTO dmarc_reports
             (server_id, domain, report_id, date_range_begin, date_range_end)
         VALUES ($1, 'x.example', 'r-1', now(), now())",
    )
    .bind(f.server.id as i64)
    .execute(&mut *tx)
    .await;
    let error = result.expect_err("cross-tenant report insert must be rejected");
    assert!(
        error.to_string().contains("row-level security"),
        "unexpected error: {error}"
    );
    drop(tx);

    // the records table enforces the same policy
    let report = {
        use camelmailer_core::ServerStore;
        f.store
            .store_dmarc_report(dmarc_report_for(f.server.id, "tenant-a.example", "a-1"))
            .await
            .unwrap()
    };
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('camelmailer.server_id', $1, true)")
        .bind(other_server.id.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    let result = sqlx::query(
        "INSERT INTO dmarc_report_records
             (server_id, report_id, source_ip, count, disposition,
              dkim_aligned, spf_aligned)
         VALUES ($1, $2, '1.2.3.4', 1, 'none', true, true)",
    )
    .bind(f.server.id as i64)
    .bind(report.id)
    .execute(&mut *tx)
    .await;
    let error = result.expect_err("cross-tenant record insert must be rejected");
    assert!(
        error.to_string().contains("row-level security"),
        "unexpected error: {error}"
    );
}

// ---------------------------------------------------------- message shares

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn message_shares_round_trip_by_token_hash() {
    use camelmailer_core::ServerStore;
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;

    let sent = f
        .store
        .store_outgoing(QueuedMessage {
            scope: MessageScope::Outgoing,
            ..message_for(f.server.id, "share@example.net")
        })
        .await
        .unwrap();

    let token_hash = camelmailer_core::auth::hash_token("share-token");
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(48);
    let share = f
        .store
        .create_message_share(camelmailer_core::NewMessageShare {
            server_id: f.server.id,
            message_id: sent.id,
            token_hash: token_hash.clone(),
            expires_at,
        })
        .await
        .unwrap();
    assert_eq!(share.server_id, f.server.id);
    assert_eq!(share.message_id, sent.id);

    // resolvable by hash (cross-tenant lookup), not by the raw token
    let found = f
        .store
        .message_share_by_token_hash(&token_hash)
        .await
        .unwrap()
        .expect("share must resolve by hash");
    assert_eq!(found.id, share.id);
    assert_eq!(found.message_id, sent.id);
    assert!((found.expires_at - expires_at).num_seconds().abs() < 2);
    assert!(f
        .store
        .message_share_by_token_hash("share-token")
        .await
        .unwrap()
        .is_none());

    // duplicate hashes are rejected (UNIQUE)
    let duplicate = f
        .store
        .create_message_share(camelmailer_core::NewMessageShare {
            server_id: f.server.id,
            message_id: sent.id,
            token_hash,
            expires_at,
        })
        .await;
    assert!(duplicate.is_err());

    // the resolved share's tenant context reads the message under RLS
    let message = f
        .store
        .message(found.server_id, found.message_id)
        .await
        .unwrap()
        .expect("shared message must load in its tenant context");
    assert_eq!(message.rcpt_to, "share@example.net");
}

// -------------------- observability: last_used, tags, bounces, request log

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn record_credential_use_stamps_last_used_at_for_smtp_auth() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let credential = f
        .store
        .create_credential(f.server.id, CredentialType::Smtp, "smtp-secret")
        .await
        .unwrap();
    assert!(credential.last_used_at.is_none());
    let listed = f.store.list_credentials(f.server.id).await.unwrap();
    assert!(listed[0].last_used_at.is_none());

    // the SMTP session calls this after a successful AUTH
    f.store.record_credential_use(credential.id);

    let listed = f.store.list_credentials(f.server.id).await.unwrap();
    assert!(listed[0].last_used_at.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounce_categories_are_persisted_and_surfaced() {
    use camelmailer_core::{MessageFilter, ServerStore, StatsFilter};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool).await;
    let sink = PgMessageSink::new(f.store.clone());
    let outgoing = |rcpt: &str| {
        let mut message = message_for(f.server.id, rcpt);
        message.scope = MessageScope::Outgoing;
        message
    };

    // terminal 5xx reject -> hard
    let hard = sink
        .insert_message(&outgoing("a@dest.example"))
        .await
        .unwrap();
    sink.record_delivery(
        f.server.id,
        hard,
        "HardFail",
        "message rejected by the remote server",
        "550 5.1.1 user unknown",
        false,
        Some("hard"),
    )
    .await
    .unwrap();

    // exhausted 4xx retries -> soft
    let soft = sink
        .insert_message(&outgoing("b@dest.example"))
        .await
        .unwrap();
    sink.record_delivery(
        f.server.id,
        soft,
        "HardFail",
        "delivery attempts exhausted",
        "421 try later",
        false,
        Some("soft"),
    )
    .await
    .unwrap();

    // an unclassified bounce counts as undetermined
    let dsn = sink
        .insert_message(&outgoing("c@dest.example"))
        .await
        .unwrap();
    sink.record_delivery(
        f.server.id,
        dsn,
        "Bounced",
        "bounce received",
        "",
        false,
        None,
    )
    .await
    .unwrap();

    // a transient SoftFail keeps no category (the message may still send)
    let retrying = sink
        .insert_message(&outgoing("d@dest.example"))
        .await
        .unwrap();
    sink.record_delivery(
        f.server.id,
        retrying,
        "SoftFail",
        "temporary delivery failure",
        "451 greylisted",
        false,
        None,
    )
    .await
    .unwrap();

    let record = f.store.message(f.server.id, hard).await.unwrap().unwrap();
    assert_eq!(record.bounce_category.as_deref(), Some("hard"));
    let record = f
        .store
        .message(f.server.id, retrying)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.bounce_category, None);

    // the DSN processing hook classifies after the fact
    sink.set_bounce_category(f.server.id, dsn, "undetermined")
        .await
        .unwrap();
    let record = f.store.message(f.server.id, dsn).await.unwrap().unwrap();
    assert_eq!(record.bounce_category.as_deref(), Some("undetermined"));

    // stats break bounces down by category (HardFail terminal failures
    // and Bounced messages; the retrying SoftFail is in no bucket)
    let stats = f
        .store
        .message_stats(f.server.id, &StatsFilter::default())
        .await
        .unwrap();
    assert_eq!(stats.bounces_hard, 1);
    assert_eq!(stats.bounces_soft, 1);
    assert_eq!(stats.bounces_undetermined, 1);

    // the bounce listing carries the category
    let bounces = f
        .store
        .bounces(f.server.id, &MessageFilter::default())
        .await
        .unwrap();
    assert_eq!(bounces.len(), 1);
    assert_eq!(bounces[0].bounce_category.as_deref(), Some("undetermined"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tags_and_tag_scoped_stats_are_tenant_isolated_by_rls() {
    use camelmailer_core::{ServerStore, StatsFilter};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;
    let other = f
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
    let tagged = |server_id, tag: &str| {
        let mut message = message_for(server_id, "user@dest.example");
        message.scope = MessageScope::Outgoing;
        message.tag = Some(tag.into());
        message
    };

    sink.insert_message(&tagged(f.server.id, "welcome"))
        .await
        .unwrap();
    sink.insert_message(&tagged(f.server.id, "welcome"))
        .await
        .unwrap();
    sink.insert_message(&tagged(f.server.id, "promo"))
        .await
        .unwrap();
    sink.insert_message(&tagged(other.id, "other-tag"))
        .await
        .unwrap();
    // an out-of-window message: age it inside the tenant's RLS context
    let stale = sink
        .insert_message(&tagged(f.server.id, "stale"))
        .await
        .unwrap();
    {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SELECT set_config('camelmailer.server_id', $1, true)")
            .bind(f.server.id.to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE messages SET created_at = now() - interval '40 days' WHERE id = $1")
            .bind(stale)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    let since = chrono::Utc::now() - chrono::Duration::days(30);
    let tags = f.store.tags(f.server.id, since).await.unwrap();
    let pairs: Vec<(String, i64)> = tags.into_iter().map(|t| (t.tag, t.count)).collect();
    assert_eq!(
        pairs,
        vec![("welcome".to_string(), 2), ("promo".to_string(), 1)]
    );

    // the other tenant sees only its own tag
    let tags = f.store.tags(other.id, since).await.unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].tag, "other-tag");

    // tag-scoped stats
    let stats = f
        .store
        .message_stats(
            f.server.id,
            &StatsFilter {
                tag: Some("welcome".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(stats.total, 2);
    let stats = f
        .store
        .message_stats(
            f.server.id,
            &StatsFilter {
                tag: Some("other-tag".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(stats.total, 0, "a foreign tenant's tag matches nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_request_log_roundtrip_scoping_and_retention() {
    use camelmailer_core::{ApiRequestFilter, NewApiRequest, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let f = fixtures(pool.clone()).await;
    let other = f
        .store
        .create_server(NewServer {
            organization_id: f.organization.id,
            name: "Other".into(),
            permalink: "other".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let entry = |server_id, method: &str, status| NewApiRequest {
        server_id,
        method: method.into(),
        path: "/api/v2/server/messages".into(),
        status_code: status,
        duration_ms: 7,
        user_agent: Some("pg-test".into()),
    };
    f.store
        .record_api_request(entry(f.server.id, "GET", 200))
        .await
        .unwrap();
    f.store
        .record_api_request(entry(f.server.id, "POST", 422))
        .await
        .unwrap();
    f.store
        .record_api_request(entry(other.id, "GET", 200))
        .await
        .unwrap();

    // newest first, explicitly scoped to the server
    let all = f
        .store
        .api_requests(f.server.id, &ApiRequestFilter::default())
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].method, "POST");
    assert_eq!(all[0].user_agent.as_deref(), Some("pg-test"));

    // status-class + case-insensitive method filters
    let four = f
        .store
        .api_requests(
            f.server.id,
            &ApiRequestFilter {
                status_class: Some(4),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(four.len(), 1);
    assert_eq!(four[0].status_code, 422);
    let gets = f
        .store
        .api_requests(
            f.server.id,
            &ApiRequestFilter {
                method: Some("get".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(gets.len(), 1);

    // the foreign server never sees these entries
    let theirs = f
        .store
        .api_requests(other.id, &ApiRequestFilter::default())
        .await
        .unwrap();
    assert_eq!(theirs.len(), 1);

    // retention: entries older than the cutoff are pruned across tenants
    sqlx::query("UPDATE api_requests SET created_at = now() - interval '31 days' WHERE id = $1")
        .bind(all[1].id)
        .execute(&pool)
        .await
        .unwrap();
    let removed = f
        .store
        .prune_api_requests(chrono::Utc::now() - chrono::Duration::days(30))
        .await
        .unwrap();
    assert_eq!(removed, 1);
    let remaining = f
        .store
        .api_requests(f.server.id, &ApiRequestFilter::default())
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, all[0].id);
}

// -------------------------------------------------------------- org SSO

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn org_sso_routes_verified_domains_and_manages_connections() {
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;
    let store = &f.store;
    let org_a = f.organization.id;
    let org_b = store
        .create_organization(NewOrganization {
            name: "Other Org".into(),
            permalink: "other-org".into(),
        })
        .await
        .unwrap()
        .id;

    // Claim a domain for org A; it is normalized and does not route while
    // unverified.
    let domain = store
        .create_org_email_domain(NewOrgEmailDomain {
            organization_id: org_a,
            domain: "Acme.COM".into(),
            verification_token: "tok-a".into(),
        })
        .await
        .unwrap();
    assert_eq!(domain.domain, "acme.com");
    assert!(!domain.verified);
    assert_eq!(
        store
            .organization_for_verified_email_domain("user@acme.com")
            .await
            .unwrap(),
        None
    );

    // Verify it: now it routes to org A (case-insensitive).
    store
        .mark_org_email_domain_verified(domain.id)
        .await
        .unwrap();
    assert_eq!(
        store
            .organization_for_verified_email_domain("ACME.com")
            .await
            .unwrap(),
        Some(org_a)
    );

    // Org B may claim the same domain, but the partial unique index blocks
    // a second verification.
    let dup = store
        .create_org_email_domain(NewOrgEmailDomain {
            organization_id: org_b,
            domain: "acme.com".into(),
            verification_token: "tok-b".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        store.mark_org_email_domain_verified(dup.id).await,
        Err(camelmailer_core::StoreError::Conflict(_))
    ));
    assert_eq!(
        store
            .organization_for_verified_email_domain("acme.com")
            .await
            .unwrap(),
        Some(org_a)
    );

    // Connections are org-scoped; JSONB config round-trips.
    let created = store
        .create_org_sso_connection(NewOrgSsoConnection {
            organization_id: org_a,
            kind: SsoKind::Oidc,
            name: "Acme Okta".into(),
            enabled: true,
            config: serde_json::json!({ "issuer": "https://acme.okta.com", "client_id": "abc" }),
            default_role: Role::Member,
            auto_provision: true,
        })
        .await
        .unwrap();
    assert_eq!(created.kind, SsoKind::Oidc);
    assert_eq!(created.config["issuer"], "https://acme.okta.com");
    assert_eq!(
        store.list_org_sso_connections(org_a).await.unwrap().len(),
        1
    );
    assert!(store
        .list_org_sso_connections(org_b)
        .await
        .unwrap()
        .is_empty());

    // COALESCE update: change name + disable, config left untouched.
    let updated = store
        .update_org_sso_connection(
            created.id,
            OrgSsoConnectionUpdate {
                name: Some("Renamed".into()),
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.name, "Renamed");
    assert!(!updated.enabled);
    assert_eq!(updated.config, created.config);
    assert_eq!(updated.default_role, Role::Member);

    assert!(store.delete_org_sso_connection(created.id).await.unwrap());
    assert!(store
        .list_org_sso_connections(org_a)
        .await
        .unwrap()
        .is_empty());

    // Deleting the org cascades its SSO rows away.
    store
        .create_org_sso_connection(NewOrgSsoConnection {
            organization_id: org_b,
            kind: SsoKind::Saml,
            name: "B SAML".into(),
            enabled: true,
            config: serde_json::json!({}),
            default_role: Role::Member,
            auto_provision: false,
        })
        .await
        .unwrap();
    assert!(store.delete_organization(org_b).await.unwrap());
    assert!(store
        .list_org_sso_connections(org_b)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .list_org_email_domains(org_b)
        .await
        .unwrap()
        .is_empty());
}

// --------------------------------------------------------------- layouts

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layouts_round_trip_and_unhook_templates_on_delete() {
    use camelmailer_core::{NewLayout, NewTemplate, ServerStore};
    let base = require_db!();
    let f = fixtures(test_pool(&base).await).await;
    let store = &f.store;

    let layout = store
        .create_layout(NewLayout {
            server_id: f.server.id,
            name: "Brand".into(),
            permalink: "brand".into(),
            html_wrapper: "<header>Acme</header>{{{ content }}}".into(),
            text_wrapper: None,
        })
        .await
        .unwrap();
    // duplicate permalink on the same server is a conflict
    assert!(store
        .create_layout(NewLayout {
            server_id: f.server.id,
            name: "Dup".into(),
            permalink: "brand".into(),
            html_wrapper: "{{{ content }}}".into(),
            text_wrapper: None,
        })
        .await
        .is_err());

    let template = store
        .create_template(NewTemplate {
            server_id: f.server.id,
            name: "Wrapped".into(),
            permalink: "wrapped".into(),
            subject: None,
            html_body: Some("<p>Hi</p>".into()),
            text_body: None,
            layout_id: Some(layout.id),
        })
        .await
        .unwrap();
    assert_eq!(template.layout_id, Some(layout.id));
    // round-trips through a fresh read
    let read = store
        .template_by_permalink(f.server.id, "wrapped")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.layout_id, Some(layout.id));

    // deleting the layout unhooks the template (FK ON DELETE SET NULL)
    assert!(store.delete_layout(f.server.id, layout.id).await.unwrap());
    let read = store
        .template_by_permalink(f.server.id, "wrapped")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.layout_id, None);
    assert!(store.list_layouts(f.server.id).await.unwrap().is_empty());
}
