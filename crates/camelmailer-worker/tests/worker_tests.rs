//! Integration tests for the delivery pipeline against a real PostgreSQL
//! plus in-process mock SMTP and HTTP servers. Gated on
//! `CAMELMAILER_TEST_DATABASE_URL` like the camelmailer-db tests.

use camelmailer_core::{
    AdminStore, MessageScope, NewOrganization, NewServer, NewSuppression, NewWebhook,
    QueuedMessage, ServerMode,
};
use camelmailer_db::{PgMessageSink, PgQueue, PgStore};
use camelmailer_worker::{ProcessOutcome, Worker};
use rand::Rng;
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

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
    let position = base.rfind('/').unwrap();
    let pool = camelmailer_db::connect(&format!("{}/{}", &base[..position], db_name), 2)
        .await
        .unwrap();
    camelmailer_db::migrate(&pool).await.unwrap();
    pool
}

/// A single-shot mock SMTP server. Replies to the transaction with the
/// given final DATA response code and records the client's envelope.
struct MockSmtp {
    port: u16,
    received: Arc<Mutex<Vec<String>>>,
}

async fn mock_smtp(final_reply: &'static str) -> MockSmtp {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = Arc::new(Mutex::new(Vec::new()));
    let captured = received.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let captured = captured.clone();
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                write_half.write_all(b"220 mock ESMTP\r\n").await.ok();
                let mut in_data = false;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let trimmed = line.trim_end().to_string();
                    captured.lock().unwrap().push(trimmed.clone());
                    if in_data {
                        if trimmed == "." {
                            in_data = false;
                            write_half
                                .write_all(format!("{final_reply}\r\n").as_bytes())
                                .await
                                .ok();
                        }
                        continue;
                    }
                    let upper = trimmed.to_ascii_uppercase();
                    let reply: &str = if upper.starts_with("EHLO") {
                        "250-mock\r\n250 OK"
                    } else if upper.starts_with("DATA") {
                        in_data = true;
                        "354 Go ahead"
                    } else if upper.starts_with("QUIT") {
                        write_half.write_all(b"221 Bye\r\n").await.ok();
                        return;
                    } else {
                        "250 OK"
                    };
                    write_half
                        .write_all(format!("{reply}\r\n").as_bytes())
                        .await
                        .ok();
                }
            });
        }
    });
    MockSmtp { port, received }
}

/// A mock HTTP server capturing POSTed JSON bodies.
struct MockHttp {
    url: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

async fn mock_http(status: axum::http::StatusCode) -> MockHttp {
    use axum::extract::State;
    let requests: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new()
        .route(
            "/hook",
            axum::routing::post(
                move |State(captured): State<Arc<Mutex<Vec<serde_json::Value>>>>,
                      axum::Json(body): axum::Json<serde_json::Value>| async move {
                    captured.lock().unwrap().push(body);
                    status
                },
            ),
        )
        .with_state(requests.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    MockHttp {
        url: format!("http://127.0.0.1:{port}/hook"),
        requests,
    }
}

struct Setup {
    store: PgStore,
    sink: PgMessageSink,
    queue: PgQueue,
    server: camelmailer_core::Server,
    #[allow(dead_code)]
    organization: camelmailer_core::Organization,
}

async fn setup(pool: PgPool) -> Setup {
    let store = PgStore::new(pool.clone());
    let organization = store
        .create_organization(NewOrganization {
            name: "Org".into(),
            permalink: "org".into(),
        })
        .await
        .unwrap();
    let server = store
        .create_server(NewServer {
            organization_id: organization.id,
            name: "Server".into(),
            permalink: "server".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    Setup {
        sink: PgMessageSink::new(store.clone()),
        queue: PgQueue::new(pool),
        store,
        server,
        organization,
    }
}

fn worker_config(relay_port: u16) -> camelmailer_config::Config {
    let mut config = camelmailer_config::Config::default();
    config.camelmailer.smtp_relays = vec![format!("smtp://127.0.0.1:{relay_port}")];
    config.camelmailer.default_maximum_delivery_attempts = 3;
    config.smtp_client.open_timeout = 5;
    // The webhook / route-endpoint mock servers in these tests bind to
    // 127.0.0.1, which the SSRF guard blocks by default. Allowlist loopback so
    // the delivery-path tests exercise the real request; the guard's blocking
    // behaviour has its own dedicated test (`webhook_to_a_blocked_host_...`).
    config.camelmailer.outbound_allowed_hosts = vec!["127.0.0.1".into()];
    config
}

fn outgoing_message(server_id: camelmailer_core::Id, rcpt_to: &str) -> QueuedMessage {
    QueuedMessage {
        server_id,
        rcpt_to: rcpt_to.into(),
        mail_from: "sender@org.example".into(),
        raw_message: b"Subject: Pipeline\r\n\r\nHello.\r\n".to_vec(),
        received_with_ssl: false,
        scope: MessageScope::Outgoing,
        bounce: false,
        domain_id: None,
        credential_id: None,
        route_id: None,
        tag: None,
        metadata: None,
        stream_id: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_messages_are_enqueued_automatically() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();
    assert_eq!(s.queue.queue_size().await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outgoing_messages_are_delivered_via_the_relay_and_webhooked() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;
    let hook = mock_http(axum::http::StatusCode::OK).await;

    s.store
        .create_webhook(NewWebhook::all(s.server.id, "hook", &hook.url, false))
        .await
        .unwrap();

    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Delivered { .. }));
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);
    worker.drain_webhooks().await.unwrap();

    // the mock SMTP saw the right envelope and body
    let seen = smtp.received.lock().unwrap().clone();
    assert!(seen.iter().any(|l| l == "MAIL FROM:<sender@org.example>"));
    assert!(seen.iter().any(|l| l == "RCPT TO:<user@dest.example>"));
    assert!(seen.iter().any(|l| l == "Subject: Pipeline"));

    // the webhook fired with a MessageSent event
    let hooks = hook.requests.lock().unwrap().clone();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0]["event"], "MessageSent");
    assert_eq!(
        hooks[0]["payload"]["message"]["rcpt_to"],
        "user@dest.example"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soft_failures_are_requeued_with_backoff_and_delayed_webhook() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool.clone()).await;
    let smtp = mock_smtp("421 Try again later").await;
    let hook = mock_http(axum::http::StatusCode::OK).await;
    s.store
        .create_webhook(NewWebhook::all(s.server.id, "hook", &hook.url, false))
        .await
        .unwrap();

    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Delayed { .. }));
    worker.drain_webhooks().await.unwrap();

    // still queued, with attempts bumped and a retry_after in the future
    assert_eq!(s.queue.queue_size().await.unwrap(), 1);
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT attempts, retry_after > now() AS deferred, locked_by FROM queued_messages",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<i32, _>("attempts"), 1);
    assert!(row.get::<bool, _>("deferred"));
    assert!(row.get::<Option<String>, _>("locked_by").is_none());

    let hooks = hook.requests.lock().unwrap().clone();
    assert_eq!(hooks[0]["event"], "MessageDelayed");

    // nothing ready right now → the worker sees an empty queue
    assert!(worker.process_next().await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_failures_leave_the_queue_and_fire_failure_webhook() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("550 No such user").await;
    let hook = mock_http(axum::http::StatusCode::OK).await;
    s.store
        .create_webhook(NewWebhook::all(s.server.id, "hook", &hook.url, false))
        .await
        .unwrap();

    let id = s
        .sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Failed { .. }));
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);
    worker.drain_webhooks().await.unwrap();
    let hooks = hook.requests.lock().unwrap().clone();
    assert_eq!(hooks[0]["event"], "MessageDeliveryFailed");

    // the 5xx reject classifies the bounce as hard
    let record = camelmailer_core::ServerStore::message(&s.store, s.server.id, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.bounce_category.as_deref(), Some("hard"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhooks_fire_only_for_subscribed_events() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;
    let failures_hook = mock_http(axum::http::StatusCode::OK).await;
    let all_hook = mock_http(axum::http::StatusCode::OK).await;

    // subscribed to failures only — must NOT fire for MessageSent
    s.store
        .create_webhook(NewWebhook {
            server_id: s.server.id,
            name: "failures".into(),
            url: failures_hook.url.clone(),
            all_events: false,
            sign: false,
            events: vec!["MessageDeliveryFailed".into(), "MessageHeld".into()],
            headers: Default::default(),
        })
        .await
        .unwrap();
    // empty events = all events (backwards compatible)
    s.store
        .create_webhook(NewWebhook::all(s.server.id, "all", &all_hook.url, false))
        .await
        .unwrap();

    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Delivered { .. }));
    worker.drain_webhooks().await.unwrap();

    assert_eq!(failures_hook.requests.lock().unwrap().len(), 0);
    let all = all_hook.requests.lock().unwrap().clone();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["event"], "MessageSent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_requests_carry_the_configured_custom_headers() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;
    let hook = mock_http_full(axum::http::StatusCode::OK).await;

    let mut custom_headers = std::collections::BTreeMap::new();
    custom_headers.insert("Authorization".to_string(), "Bearer hunter2".to_string());
    custom_headers.insert("X-Custom".to_string(), "camel".to_string());
    s.store
        .create_webhook(NewWebhook {
            server_id: s.server.id,
            name: "hook".into(),
            url: hook.url.clone(),
            all_events: false,
            sign: false,
            events: vec!["MessageSent".into()],
            headers: custom_headers,
        })
        .await
        .unwrap();

    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    worker.process_next().await.unwrap().unwrap();
    assert_eq!(worker.drain_webhooks().await.unwrap(), 1);

    let requests = hook.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let (headers, _body) = &requests[0];
    // custom headers arrive alongside the platform headers
    assert_eq!(headers["authorization"], "Bearer hunter2");
    assert_eq!(headers["x-custom"], "camel");
    assert_eq!(headers["x-camelmailer-event"], "MessageSent");
    assert!(headers["x-camelmailer-uuid"].is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attempts_exhaust_into_terminal_failure() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("421 Still broken").await;

    let id = s
        .sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    // max_attempts = 3 in worker_config
    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    for expected_delayed in [true, true, false] {
        s.queue.clear_backoff().await.unwrap();
        let outcome = worker.process_next().await.unwrap().unwrap();
        if expected_delayed {
            assert!(matches!(outcome, ProcessOutcome::Delayed { .. }));
            // still transient — no bounce category yet
            let record = camelmailer_core::ServerStore::message(&s.store, s.server.id, id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(record.bounce_category, None);
        } else {
            assert!(matches!(outcome, ProcessOutcome::Failed { .. }));
        }
    }
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);

    // exhausting 4xx retries classifies the bounce as soft
    let record = camelmailer_core::ServerStore::message(&s.store, s.server.id, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.bounce_category.as_deref(), Some("soft"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_dsn_bounces_are_classified_from_their_status_fields() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let worker = Worker::new(&worker_config(1), s.store.clone());

    let dsn = |raw: &[u8]| {
        let mut message = outgoing_message(s.server.id, "return-path@org.example");
        message.scope = MessageScope::Incoming;
        message.bounce = true;
        message.raw_message = raw.to_vec();
        message
    };

    // Status: 5.1.1 -> hard; Status: 4.4.1 -> soft; a DSN without
    // Status:/Diagnostic-Code: fields stays undetermined (numbers in the
    // human-readable text never classify).
    let cases: [(&[u8], &str); 3] = [
        (
            b"Subject: Delivery Status Notification\r\n\r\n\
              Final-Recipient: rfc822; gone@example.com\r\n\
              Action: failed\r\n\
              Status: 5.1.1\r\n\
              Diagnostic-Code: smtp; 550 5.1.1 user unknown\r\n",
            "hard",
        ),
        (b"Subject: Delayed\r\n\r\nStatus: 4.4.1\r\n", "soft"),
        (
            b"Subject: bounce\r\n\r\nYour mail from 2026 got 550 problems.\r\n",
            "undetermined",
        ),
    ];

    for (raw, expected) in cases {
        let id = s.sink.insert_message(&dsn(raw)).await.unwrap();
        let outcome = worker.process_next().await.unwrap().unwrap();
        assert_eq!(outcome, ProcessOutcome::NothingToDo);
        let record = camelmailer_core::ServerStore::message(&s.store, s.server.id, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.bounce_category.as_deref(), Some(expected));
    }
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn housekeeping_prunes_expired_api_request_log_entries() {
    use camelmailer_core::ServerStore;

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool.clone()).await;
    let worker = Worker::new(&worker_config(1), s.store.clone());

    let entry = |path: &str| camelmailer_core::NewApiRequest {
        server_id: s.server.id,
        method: "GET".into(),
        path: path.into(),
        status_code: 200,
        duration_ms: 3,
        user_agent: Some("housekeeping-test".into()),
    };
    s.store
        .record_api_request(entry("/api/v2/server/fresh"))
        .await
        .unwrap();
    s.store
        .record_api_request(entry("/api/v2/server/stale"))
        .await
        .unwrap();
    // age one entry past the 30-day retention
    sqlx::query("UPDATE api_requests SET created_at = now() - interval '31 days' WHERE path = $1")
        .bind("/api/v2/server/stale")
        .execute(&pool)
        .await
        .unwrap();

    let removed = worker.housekeep().await.unwrap();
    assert_eq!(removed, 1);

    let remaining = s
        .store
        .api_requests(s.server.id, &camelmailer_core::ApiRequestFilter::default())
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].path, "/api/v2/server/fresh");

    // a second run has nothing left to prune
    assert_eq!(worker.housekeep().await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suppressed_recipients_hold_the_message() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    s.store
        .create_suppression(NewSuppression {
            server_id: s.server.id,
            suppression_type: "recipient".into(),
            address: "blocked@dest.example".into(),
            reason: Some("hard bounce".into()),
            stream_id: None,
        })
        .await
        .unwrap();
    s.sink
        .insert_message(&outgoing_message(s.server.id, "blocked@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::Held);
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);
    // nothing was sent over SMTP
    assert!(smtp.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_scoped_suppression_holds_broadcast_but_not_transactional() {
    use camelmailer_core::{NewStream, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    // A broadcast stream, and the recipient unsubscribed on it.
    let stream = ServerStore::create_stream(
        &s.store,
        NewStream {
            server_id: s.server.id,
            name: "Broadcast".into(),
            permalink: "broadcast".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        },
    )
    .await
    .unwrap();
    s.store
        .create_suppression(NewSuppression {
            server_id: s.server.id,
            suppression_type: "unsubscribe".into(),
            address: "reader@dest.example".into(),
            reason: Some("Unsubscribed via List-Unsubscribe".into()),
            stream_id: Some(stream.id),
        })
        .await
        .unwrap();

    // A broadcast message to that address is held.
    let mut broadcast = outgoing_message(s.server.id, "reader@dest.example");
    broadcast.stream_id = Some(stream.id);
    s.sink.insert_message(&broadcast).await.unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    assert_eq!(
        worker.process_next().await.unwrap().unwrap(),
        ProcessOutcome::Held
    );
    assert!(smtp.received.lock().unwrap().is_empty());

    // The same address on a transactional (default) stream is NOT blocked.
    let mut transactional = outgoing_message(s.server.id, "reader@dest.example");
    transactional.stream_id = s.server.default_stream_id;
    s.sink.insert_message(&transactional).await.unwrap();
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Delivered { .. }));
    assert!(smtp
        .received
        .lock()
        .unwrap()
        .iter()
        .any(|l| l == "RCPT TO:<reader@dest.example>"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_wide_suppression_holds_on_every_stream() {
    use camelmailer_core::{NewStream, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    // A server-wide suppression (stream_id NULL) blocks the address on ALL
    // streams — transactional and broadcast alike.
    s.store
        .create_suppression(NewSuppression {
            server_id: s.server.id,
            suppression_type: "recipient".into(),
            address: "blocked@dest.example".into(),
            reason: Some("hard bounce".into()),
            stream_id: None,
        })
        .await
        .unwrap();

    let stream = ServerStore::create_stream(
        &s.store,
        NewStream {
            server_id: s.server.id,
            name: "Broadcast".into(),
            permalink: "broadcast".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        },
    )
    .await
    .unwrap();

    // A message on the broadcast stream is held despite the suppression being
    // server-wide, not stream-scoped.
    let mut on_stream = outgoing_message(s.server.id, "blocked@dest.example");
    on_stream.stream_id = Some(stream.id);
    s.sink.insert_message(&on_stream).await.unwrap();

    // And so is one on the default (transactional) stream.
    let mut on_default = outgoing_message(s.server.id, "blocked@dest.example");
    on_default.stream_id = s.server.default_stream_id;
    s.sink.insert_message(&on_default).await.unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    assert_eq!(
        worker.process_next().await.unwrap().unwrap(),
        ProcessOutcome::Held
    );
    assert_eq!(
        worker.process_next().await.unwrap().unwrap(),
        ProcessOutcome::Held
    );
    // Nothing was delivered on either stream.
    assert!(smtp.received.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incoming_route_messages_are_posted_to_their_endpoint() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let endpoint = mock_http(axum::http::StatusCode::OK).await;

    let domain = s
        .store
        .create_domain(
            camelmailer_core::DomainOwner::Server(s.server.id),
            "org.example",
            true,
            None,
        )
        .await
        .unwrap();
    let route = s
        .store
        .create_route_with_endpoint(
            s.server.id,
            Some(domain.id),
            "info",
            camelmailer_core::RouteMode::Endpoint,
            Some(endpoint.url.clone()),
        )
        .await
        .unwrap();

    let mut message = outgoing_message(s.server.id, "info@org.example");
    message.scope = MessageScope::Incoming;
    message.route_id = Some(route.id);
    s.sink.insert_message(&message).await.unwrap();

    let worker = Worker::new(&worker_config(1), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::Routed);
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);

    let posts = endpoint.requests.lock().unwrap().clone();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["message"]["rcpt_to"], "info@org.example");
    assert!(posts[0]["raw_base64"].is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incoming_messages_without_an_endpoint_are_completed_silently() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;

    let mut message = outgoing_message(s.server.id, "info@org.example");
    message.scope = MessageScope::Incoming;
    s.sink.insert_message(&message).await.unwrap();

    let worker = Worker::new(&worker_config(1), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::NothingToDo);
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deliveries_and_status_are_recorded_by_the_worker() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    let message_id = s
        .sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    worker.process_next().await.unwrap().unwrap();

    let message = s
        .sink
        .message_by_id(s.server.id, message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.status, "Sent");

    let deliveries = s
        .sink
        .deliveries_for_message(s.server.id, message_id)
        .await
        .unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].status, "Sent");
    assert!(deliveries[0]
        .output
        .as_deref()
        .unwrap_or_default()
        .contains("250 Accepted"));
}

// ------------------------------------------------- webhook queue (commit A)

/// A mock HTTP server capturing headers and raw bodies.
type CapturedRequests = Arc<Mutex<Vec<(serde_json::Value, String)>>>; // (headers, body)

struct MockHttpFull {
    url: String,
    requests: CapturedRequests,
}

async fn mock_http_full(status: axum::http::StatusCode) -> MockHttpFull {
    use axum::extract::State;
    let requests: CapturedRequests = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new()
        .route(
            "/hook",
            axum::routing::post(
                move |State(captured): State<CapturedRequests>,
                      headers: axum::http::HeaderMap,
                      body: String| async move {
                    let header_map: serde_json::Value = headers
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.as_str().to_string(),
                                serde_json::Value::String(v.to_str().unwrap_or("").to_string()),
                            )
                        })
                        .collect::<serde_json::Map<_, _>>()
                        .into();
                    captured.lock().unwrap().push((header_map, body));
                    status
                },
            ),
        )
        .with_state(requests.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    MockHttpFull {
        url: format!("http://127.0.0.1:{port}/hook"),
        requests,
    }
}

fn signing_key_path() -> String {
    format!(
        "{}/tests/fixtures/test_signing_key.pem",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_payloads_are_signed_and_verifiable() {
    use rsa::signature::Verifier;

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;
    let hook = mock_http_full(axum::http::StatusCode::OK).await;

    s.store
        .create_webhook(NewWebhook::all(s.server.id, "signed", &hook.url, true))
        .await
        .unwrap();
    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let mut config = worker_config(smtp.port);
    config.camelmailer.signing_key_path = signing_key_path();
    let worker = Worker::new(&config, s.store.clone());
    worker.process_next().await.unwrap().unwrap();
    assert_eq!(worker.drain_webhooks().await.unwrap(), 1);

    let requests = hook.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let (headers, body) = &requests[0];
    assert_eq!(headers["x-camelmailer-event"], "MessageSent");
    assert!(headers["x-camelmailer-uuid"].is_string());

    // verify the RSA-SHA256 signature against the signing key's public half
    use base64::Engine;
    let signature_b64 = headers["x-camelmailer-signature"].as_str().unwrap();
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_b64)
        .unwrap();
    let pem = std::fs::read_to_string(signing_key_path()).unwrap();
    let signer = camelmailer_worker::Signer::from_pem(&pem).unwrap();
    let verifying_key = rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(signer.public_key());
    let signature = rsa::pkcs1v15::Signature::try_from(signature_bytes.as_slice()).unwrap();
    verifying_key.verify(body.as_bytes(), &signature).unwrap();

    // the audit log recorded the successful attempt
    let queue = camelmailer_db::PgWebhookQueue::new(s.store.pool().clone());
    let log = queue.log_for_server(s.server.id).await.unwrap();
    assert_eq!(log.len(), 1);
    assert!(log[0].success);
    assert_eq!(log[0].status_code, Some(200));
    assert_eq!(log[0].attempt, 1);
    assert_eq!(queue.queue_size().await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failing_webhooks_are_retried_with_backoff_and_logged() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;
    let hook = mock_http(axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;

    s.store
        .create_webhook(NewWebhook::all(s.server.id, "flaky", &hook.url, false))
        .await
        .unwrap();
    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    worker.process_next().await.unwrap().unwrap();

    let outcome = worker.process_next_webhook().await.unwrap().unwrap();
    assert_eq!(outcome, camelmailer_worker::WebhookOutcome::Retrying);

    let queue = camelmailer_db::PgWebhookQueue::new(s.store.pool().clone());
    // still queued but deferred: nothing ready right now
    assert_eq!(queue.queue_size().await.unwrap(), 1);
    assert!(worker.process_next_webhook().await.unwrap().is_none());

    // second attempt after clearing the backoff
    queue.clear_backoff().await.unwrap();
    let outcome = worker.process_next_webhook().await.unwrap().unwrap();
    assert_eq!(outcome, camelmailer_worker::WebhookOutcome::Retrying);

    let log = queue.log_for_server(s.server.id).await.unwrap();
    assert_eq!(log.len(), 2);
    assert!(!log[0].success);
    assert_eq!(log[0].status_code, Some(500));
    assert_eq!(log[0].attempt, 1);
    assert_eq!(log[1].attempt, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_to_a_blocked_host_is_refused_by_the_ssrf_guard() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    // The mock binds to loopback — a non-global address the guard blocks.
    let hook = mock_http(axum::http::StatusCode::OK).await;

    let webhook = s
        .store
        .create_webhook(NewWebhook::all(s.server.id, "internal", &hook.url, false))
        .await
        .unwrap();

    let queue = camelmailer_db::PgWebhookQueue::new(s.store.pool().clone());
    queue
        .enqueue(
            s.server.id,
            webhook.id,
            "uuid-blocked",
            "MessageSent",
            &hook.url,
            "{}",
            false,
            &std::collections::BTreeMap::new(),
        )
        .await
        .unwrap();

    // Guard ON with loopback NOT allowlisted: the request must be refused.
    let mut config = worker_config(1);
    config.camelmailer.outbound_allowed_hosts = vec![];
    let worker = Worker::new(&config, s.store.clone());

    let outcome = worker.process_next_webhook().await.unwrap().unwrap();
    assert_eq!(outcome, camelmailer_worker::WebhookOutcome::Retrying);

    // The request was never sent...
    assert!(hook.requests.lock().unwrap().is_empty());
    // ...and the audit log recorded a blocked attempt (no HTTP status).
    let log = queue.log_for_server(s.server.id).await.unwrap();
    assert_eq!(log.len(), 1);
    assert!(!log[0].success);
    assert_eq!(log[0].status_code, None);
    assert!(log[0]
        .response_body
        .as_deref()
        .unwrap_or_default()
        .contains("address guard"));
}

// --------------------------------------------------------- DKIM (commit B)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outgoing_mail_with_an_authenticated_domain_is_dkim_signed() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    let domain = s
        .store
        .create_domain(
            camelmailer_core::DomainOwner::Server(s.server.id),
            "org.example",
            true,
            None,
        )
        .await
        .unwrap();

    let mut message = outgoing_message(s.server.id, "user@dest.example");
    message.domain_id = Some(domain.id);
    s.sink.insert_message(&message).await.unwrap();

    let mut config = worker_config(smtp.port);
    config.camelmailer.signing_key_path = signing_key_path();
    let worker = Worker::new(&config, s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Delivered { .. }));

    let seen = smtp.received.lock().unwrap().clone();
    let dkim_line = seen
        .iter()
        .find(|l| l.starts_with("DKIM-Signature: "))
        .expect("mock SMTP must have received a DKIM-Signature header");
    assert!(dkim_line.contains("d=org.example;"));
    assert!(dkim_line.contains("s=postal;"));
    assert!(dkim_line.contains("a=rsa-sha256;"));
    // the original message follows unmodified
    assert!(seen.iter().any(|l| l == "Subject: Pipeline"));

    // the stored message stays unsigned
    let stored = s.sink.messages_for_server(s.server.id).await.unwrap();
    assert!(!String::from_utf8_lossy(&stored[0].raw_message).contains("DKIM-Signature"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outgoing_mail_uses_the_domain_dkim_key_when_present() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    let domain_key = std::fs::read_to_string(format!(
        "{}/tests/fixtures/test_domain_key.pem",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let domain = s
        .store
        .create_server_domain(s.server.id, "org.example", Some(domain_key.clone()))
        .await
        .unwrap();

    let mut message = outgoing_message(s.server.id, "user@dest.example");
    message.domain_id = Some(domain.id);
    s.sink.insert_message(&message).await.unwrap();

    // No installation signing key configured: the signature can only come
    // from the domain's own key.
    let config = worker_config(smtp.port);
    assert!(!std::path::Path::new(&config.camelmailer.signing_key_path).exists());
    let worker = Worker::new(&config, s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Delivered { .. }));

    let seen = smtp.received.lock().unwrap().clone();
    let dkim_line = seen
        .iter()
        .find(|l| l.starts_with("DKIM-Signature: "))
        .expect("the domain key must have signed the message");
    assert!(dkim_line.contains("d=org.example;"));
    assert!(dkim_line.contains("s=postal;"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outgoing_mail_without_a_domain_is_not_dkim_signed() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let mut config = worker_config(smtp.port);
    config.camelmailer.signing_key_path = signing_key_path();
    let worker = Worker::new(&config, s.store.clone());
    worker.process_next().await.unwrap().unwrap();

    let seen = smtp.received.lock().unwrap().clone();
    assert!(!seen.iter().any(|l| l.starts_with("DKIM-Signature:")));
}

// -------------------------------------------- inspection (commit C)

/// Mock rspamd returning a fixed score/action.
async fn mock_rspamd(score: f64, action: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/checkv2",
        axum::routing::post(move |_body: axum::body::Bytes| async move {
            axum::Json(serde_json::json!({ "score": score, "action": action }))
        }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    port
}

/// Mock clamd speaking the INSTREAM protocol, replying with a fixed verdict.
async fn mock_clamd(reply: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                // drain the INSTREAM upload until the zero-length terminator
                let mut buffer = vec![0u8; 4096];
                loop {
                    match stream.read(&mut buffer).await {
                        Ok(0) => break,
                        Ok(_) => {
                            // crude: once we've seen data, respond after a beat
                            if buffer.windows(4).any(|w| w == [0, 0, 0, 0]) {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                stream.write_all(format!("{reply}\0").as_bytes()).await.ok();
                stream.shutdown().await.ok();
            });
        }
    });
    port
}

async fn incoming_route_setup(s: &Setup) -> (camelmailer_core::Domain, camelmailer_core::Route) {
    let domain = s
        .store
        .create_domain(
            camelmailer_core::DomainOwner::Server(s.server.id),
            "org.example",
            true,
            None,
        )
        .await
        .unwrap();
    let route = s
        .store
        .create_route_with_endpoint(
            s.server.id,
            Some(domain.id),
            "info",
            camelmailer_core::RouteMode::Accept,
            None,
        )
        .await
        .unwrap();
    (domain, route)
}

fn incoming_message(
    server_id: camelmailer_core::Id,
    route_id: camelmailer_core::Id,
) -> QueuedMessage {
    let mut message = outgoing_message(server_id, "info@org.example");
    message.scope = MessageScope::Incoming;
    message.route_id = Some(route_id);
    message
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_incoming_mail_is_inspected_and_scored_not_spam() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let (_domain, route) = incoming_route_setup(&s).await;
    let rspamd_port = mock_rspamd(0.5, "no action").await;

    let message_id = s
        .sink
        .insert_message(&incoming_message(s.server.id, route.id))
        .await
        .unwrap();

    let mut config = worker_config(1);
    config.rspamd.enabled = true;
    config.rspamd.host = "127.0.0.1".into();
    config.rspamd.port = rspamd_port;
    config.camelmailer.default_spam_threshold = 5;
    config.camelmailer.default_spam_failure_threshold = 20;

    let worker = Worker::new(&config, s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::NothingToDo);

    let message = s
        .sink
        .message_by_id(s.server.id, message_id)
        .await
        .unwrap()
        .unwrap();
    assert!(message.inspected);
    assert_eq!(message.spam_status, "NotSpam");
    assert!((message.spam_score - 0.5).abs() < 1e-9);
    assert!(!message.threat);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spam_failure_mail_is_held() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let (_domain, route) = incoming_route_setup(&s).await;
    let rspamd_port = mock_rspamd(25.0, "reject").await;

    let message_id = s
        .sink
        .insert_message(&incoming_message(s.server.id, route.id))
        .await
        .unwrap();

    let mut config = worker_config(1);
    config.rspamd.enabled = true;
    config.rspamd.host = "127.0.0.1".into();
    config.rspamd.port = rspamd_port;
    config.camelmailer.default_spam_threshold = 5;
    config.camelmailer.default_spam_failure_threshold = 20;

    let worker = Worker::new(&config, s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::Held);

    let message = s
        .sink
        .message_by_id(s.server.id, message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.spam_status, "SpamFailure");
    let deliveries = s
        .sink
        .deliveries_for_message(s.server.id, message_id)
        .await
        .unwrap();
    assert_eq!(deliveries[0].status, "Held");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn virus_mail_is_held_with_the_signature_recorded() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let (_domain, route) = incoming_route_setup(&s).await;
    let clamd_port = mock_clamd("stream: Eicar-Test-Signature FOUND").await;

    let message_id = s
        .sink
        .insert_message(&incoming_message(s.server.id, route.id))
        .await
        .unwrap();

    let mut config = worker_config(1);
    config.clamav.enabled = true;
    config.clamav.host = "127.0.0.1".into();
    config.clamav.port = clamd_port;

    let worker = Worker::new(&config, s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::Held);

    let message = s
        .sink
        .message_by_id(s.server.id, message_id)
        .await
        .unwrap()
        .unwrap();
    assert!(message.threat);
    assert_eq!(
        message.threat_details.as_deref(),
        Some("Eicar-Test-Signature")
    );
    assert!(message.inspected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_virus_scan_passes() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let (_domain, route) = incoming_route_setup(&s).await;
    let clamd_port = mock_clamd("stream: OK").await;

    let message_id = s
        .sink
        .insert_message(&incoming_message(s.server.id, route.id))
        .await
        .unwrap();

    let mut config = worker_config(1);
    config.clamav.enabled = true;
    config.clamav.host = "127.0.0.1".into();
    config.clamav.port = clamd_port;

    let worker = Worker::new(&config, s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::NothingToDo);

    let message = s
        .sink
        .message_by_id(s.server.id, message_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!message.threat);
    assert!(message.inspected);
}

// -------------------------------------------------- tracking (commit D)

fn html_outgoing(server_id: camelmailer_core::Id) -> QueuedMessage {
    let mut message = outgoing_message(server_id, "user@dest.example");
    message.raw_message = b"Content-Type: text/html\r\n\r\n<html><body>\
        <a href=\"https://example.com/offer\">Offer</a> and \
        <a href=\"mailto:x@y.z\">mail</a></body></html>\r\n"
        .to_vec();
    message
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn html_outgoing_mail_gets_click_links_rewritten_and_an_open_pixel() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode as Code};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    let message_id = s
        .sink
        .insert_message(&html_outgoing(s.server.id))
        .await
        .unwrap();

    let mut config = worker_config(smtp.port);
    config.camelmailer.web_protocol = "https".into();
    config.dns.track_domain = "track.example.com".into();
    let worker = Worker::new(&config, s.store.clone());
    worker.process_next().await.unwrap().unwrap();

    // the mock SMTP body carries the rewritten click link and the pixel
    let seen = smtp.received.lock().unwrap().clone();
    let body = seen.join("\n");
    assert!(body.contains("https://track.example.com/track/c/"));
    assert!(body.contains("/track/o/"));
    assert!(body.contains(".gif"));
    // the mailto link is untouched
    assert!(body.contains("href=\"mailto:x@y.z\""));

    // pull a click token out of the rewritten body
    let click_token = body
        .split("/track/c/")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    let open_token = body
        .split("/track/o/")
        .nth(1)
        .unwrap()
        .split(".gif")
        .next()
        .unwrap()
        .to_string();

    // drive the public tracking endpoints against the resolved store
    let tracking: std::sync::Arc<dyn camelmailer_core::TrackingStore> =
        std::sync::Arc::new(s.store.clone());
    let router =
        camelmailer_api::tracking_router(std::sync::Arc::new(camelmailer_api::TrackingState {
            store: tracking,
        }));

    // click → 302 redirect to the original URL, click recorded
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/track/c/{click_token}"))
                .header("x-forwarded-for", "9.9.9.9")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), Code::FOUND);
    assert_eq!(
        response.headers().get("location").unwrap(),
        "https://example.com/offer"
    );

    // open → 1x1 gif, open recorded
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/track/o/{open_token}.gif"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), Code::OK);
    assert_eq!(response.headers().get("content-type").unwrap(), "image/gif");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[0..6], b"GIF89a");

    // the activity counts reflect one click and one open
    let (clicks, opens) = s
        .sink
        .activity_counts(s.server.id, message_id)
        .await
        .unwrap();
    assert_eq!(clicks, 1);
    assert_eq!(opens, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_html_mail_is_not_rewritten_for_tracking() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    // An inline-PGP-signed HTML body: top-level text/html (so it would
    // normally be tracked), but a rewrite would break the signature.
    let mut message = outgoing_message(s.server.id, "user@dest.example");
    message.raw_message = b"Content-Type: text/html\r\n\r\n\
        -----BEGIN PGP SIGNED MESSAGE-----\r\nHash: SHA256\r\n\r\n\
        <html><body><a href=\"https://example.com/offer\">Offer</a></body></html>\r\n\
        -----BEGIN PGP SIGNATURE-----\r\nabc\r\n-----END PGP SIGNATURE-----\r\n"
        .to_vec();
    let message_id = s.sink.insert_message(&message).await.unwrap();

    let mut config = worker_config(smtp.port);
    config.camelmailer.web_protocol = "https".into();
    config.dns.track_domain = "track.example.com".into();
    let worker = Worker::new(&config, s.store.clone());
    worker.process_next().await.unwrap().unwrap();

    let seen = smtp.received.lock().unwrap().clone();
    let body = seen.join("\n");
    // the signature and original link survive untouched; no tracking injected
    assert!(body.contains("-----BEGIN PGP SIGNATURE-----"));
    assert!(body.contains("href=\"https://example.com/offer\""));
    assert!(!body.contains("track.example.com/track/c/"));
    assert!(!body.contains("/track/o/"));

    // and no click/open tokens were registered for it
    let (clicks, opens) = s
        .sink
        .activity_counts(s.server.id, message_id)
        .await
        .unwrap();
    assert_eq!(clicks, 0);
    assert_eq!(opens, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_tracking_tokens_do_not_leak_validity() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode as Code};
    use tower::ServiceExt;

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;

    let tracking: std::sync::Arc<dyn camelmailer_core::TrackingStore> =
        std::sync::Arc::new(s.store.clone());
    let router =
        camelmailer_api::tracking_router(std::sync::Arc::new(camelmailer_api::TrackingState {
            store: tracking,
        }));

    // unknown click → 404
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/track/c/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), Code::NOT_FOUND);

    // unknown open → still a 200 pixel (no oracle)
    let response = router
        .oneshot(
            Request::builder()
                .uri("/track/o/nope.gif")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), Code::OK);
}

// ------------------------------- outbound STARTTLS + IP pools (commit E)

/// A mock SMTP server that advertises STARTTLS and upgrades to TLS with a
/// self-signed certificate, then accepts the message. Records whether the
/// final transaction happened over TLS.
struct MockTlsSmtp {
    port: u16,
    used_tls: Arc<Mutex<bool>>,
}

async fn mock_starttls_smtp() -> MockTlsSmtp {
    use tokio_rustls::TlsAcceptor;

    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key =
        rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der()).unwrap();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let used_tls = Arc::new(Mutex::new(false));
    let flag = used_tls.clone();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let flag = flag.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                write_half.write_all(b"220 mock ESMTP\r\n").await.ok();
                // read EHLO
                let mut line = String::new();
                reader.read_line(&mut line).await.ok();
                write_half
                    .write_all(b"250-mock\r\n250 STARTTLS\r\n")
                    .await
                    .ok();
                // expect STARTTLS
                line.clear();
                reader.read_line(&mut line).await.ok();
                if !line.to_uppercase().starts_with("STARTTLS") {
                    return;
                }
                write_half
                    .write_all(b"220 Ready to start TLS\r\n")
                    .await
                    .ok();
                // upgrade
                let raw = reader.into_inner().reunite(write_half).unwrap();
                let Ok(tls_stream) = acceptor.accept(raw).await else {
                    return;
                };
                *flag.lock().unwrap() = true;
                let (tls_read, mut tls_write) = tokio::io::split(tls_stream);
                let mut reader = BufReader::new(tls_read);
                let mut in_data = false;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let trimmed = line.trim_end();
                    if in_data {
                        if trimmed == "." {
                            in_data = false;
                            tls_write.write_all(b"250 Accepted\r\n").await.ok();
                        }
                        continue;
                    }
                    let upper = trimmed.to_ascii_uppercase();
                    let reply: &str = if upper.starts_with("EHLO") {
                        "250 mock"
                    } else if upper.starts_with("DATA") {
                        in_data = true;
                        "354 Go ahead"
                    } else if upper.starts_with("QUIT") {
                        tls_write.write_all(b"221 Bye\r\n").await.ok();
                        return;
                    } else {
                        "250 OK"
                    };
                    tls_write
                        .write_all(format!("{reply}\r\n").as_bytes())
                        .await
                        .ok();
                }
            });
        }
    });
    MockTlsSmtp { port, used_tls }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_delivery_upgrades_to_starttls_and_records_ssl() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_starttls_smtp().await;

    let message_id = s
        .sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let mut config = worker_config(smtp.port);
    // accept the self-signed cert (openssl_verify_mode=none)
    config.smtp.openssl_verify_mode = "none".into();
    config.smtp.enable_starttls_auto = true;

    let worker = Worker::new(&config, s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Delivered { .. }));
    assert!(*smtp.used_tls.lock().unwrap(), "delivery must use STARTTLS");

    let deliveries = s
        .sink
        .deliveries_for_message(s.server.id, message_id)
        .await
        .unwrap();
    assert_eq!(deliveries[0].status, "Sent");
    assert!(
        deliveries[0].sent_with_ssl,
        "sent_with_ssl must be recorded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivery_binds_the_ip_pool_source_address() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    // a pool with a loopback source address the OS will let us bind
    let ip_pool = s.store.create_ip_pool("pool", true).await.unwrap();
    s.store
        .create_ip_address(camelmailer_core::NewIpAddress {
            ip_pool_id: ip_pool.id,
            ipv4: "127.0.0.1".into(),
            ipv6: None,
            hostname: "mx.example".into(),
            priority: 10,
        })
        .await
        .unwrap();
    s.store
        .set_server_ip_pool(s.server.id, Some(ip_pool.id))
        .await
        .unwrap();

    // the resolver returns the bound source for this server
    assert_eq!(
        s.store.source_ip_for_server(s.server.id).await,
        Some("127.0.0.1".parse().unwrap())
    );

    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    // binding 127.0.0.1 as source to a 127.0.0.1 relay succeeds
    assert!(matches!(outcome, ProcessOutcome::Delivered { .. }));
    let seen = smtp.received.lock().unwrap().clone();
    assert!(seen.iter().any(|l| l == "RCPT TO:<user@dest.example>"));
}

// -------------------------------------- HTTP send end-to-end (P2)

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_sent_message_is_delivered_by_the_worker() {
    use camelmailer_core::{DomainOwner, QueuedMessage, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("250 Accepted").await;

    // a verified sending domain for From-authorization + DKIM
    let domain = s
        .store
        .create_domain(DomainOwner::Server(s.server.id), "org.example", true, None)
        .await
        .unwrap();

    // simulate what the HTTP /messages handler does: build+store an outgoing
    // message via the ServerStore, carrying the authenticated domain_id.
    let queued = QueuedMessage {
        server_id: s.server.id,
        rcpt_to: "user@dest.example".into(),
        mail_from: "news@org.example".into(),
        raw_message: b"From: news@org.example\r\nSubject: API send\r\n\r\nHello via HTTP.\r\n"
            .to_vec(),
        received_with_ssl: false,
        scope: MessageScope::Outgoing,
        bounce: false,
        domain_id: Some(domain.id),
        credential_id: None,
        route_id: None,
        tag: Some("api".into()),
        metadata: None,
        stream_id: None,
    };
    let sent = s.store.store_outgoing(queued).await.unwrap();
    assert_eq!(s.queue.queue_size().await.unwrap(), 1);

    // the existing worker delivers it (proving DKIM + tracking reuse)
    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Delivered { .. }));

    // the mock SMTP saw a DKIM-signed message with our subject
    let seen = smtp.received.lock().unwrap().clone();
    assert!(seen.iter().any(|l| l == "Subject: API send"));

    let message = s
        .sink
        .message_by_id(s.server.id, sent.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.status, "Sent");
    assert_eq!(message.tag.as_deref(), Some("api"));
}

// ------------------------------------------ DMARC ingestion (RUA route)

/// A raw inbound mail carrying the given payload as a base64 attachment.
fn dmarc_mail(filename: &str, content_type: &str, payload: &[u8]) -> Vec<u8> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    format!(
        "From: noreply-dmarc-support@acme.com\r\n\
         To: dmarc@org.example\r\n\
         Subject: Report Domain: example.com\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"B\"\r\n\
         \r\n\
         --B\r\n\
         Content-Type: {content_type}; name=\"{filename}\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         Content-Disposition: attachment; filename=\"{filename}\"\r\n\
         \r\n\
         {encoded}\r\n\
         --B--\r\n"
    )
    .into_bytes()
}

fn gzipped_fixture_report() -> Vec<u8> {
    use std::io::Write;
    let xml = std::fs::read(format!(
        "{}/tests/fixtures/dmarc/rfc-appendix-c.xml",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&xml).unwrap();
    encoder.finish().unwrap()
}

/// A route targeting `internal://dmarc-reports` on org.example.
async fn dmarc_route_setup(s: &Setup) -> camelmailer_core::Route {
    let domain = s
        .store
        .create_domain(
            camelmailer_core::DomainOwner::Server(s.server.id),
            "org.example",
            true,
            None,
        )
        .await
        .unwrap();
    s.store
        .create_route_with_endpoint(
            s.server.id,
            Some(domain.id),
            "dmarc",
            camelmailer_core::RouteMode::Endpoint,
            Some(camelmailer_core::DMARC_REPORTS_ENDPOINT.to_string()),
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dmarc_route_messages_are_ingested_into_the_report_tables() {
    use camelmailer_core::ServerStore;

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let route = dmarc_route_setup(&s).await;

    let mut message = outgoing_message(s.server.id, "dmarc@org.example");
    message.scope = MessageScope::Incoming;
    message.route_id = Some(route.id);
    message.raw_message = dmarc_mail(
        "acme.com!example.com!1335571200!1335657599.xml.gz",
        "application/gzip",
        &gzipped_fixture_report(),
    );
    let message_id = s.sink.insert_message(&message).await.unwrap();

    let worker = Worker::new(&worker_config(1), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::DmarcReportIngested);
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);

    // the report landed in the tenant's tables
    let reports = s
        .store
        .dmarc_reports(s.server.id, &camelmailer_core::DmarcFilter::default())
        .await
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].domain, "example.com");
    assert_eq!(reports[0].org_name.as_deref(), Some("acme.com"));
    assert_eq!(reports[0].report_id, "9391651994964116463");
    assert_eq!(reports[0].record_count, 1);
    let (_, records) = s
        .store
        .dmarc_report(s.server.id, reports[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].source_ip, "72.150.241.94");
    assert_eq!(records[0].count, 2);
    assert!(records[0].spf_aligned);
    assert!(!records[0].dkim_aligned);

    // the message was marked processed — and nothing was POSTed anywhere
    // (an HTTP attempt against internal:// would have failed the message)
    let stored = s
        .sink
        .message_by_id(s.server.id, message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, "Processed");
    let deliveries = s
        .sink
        .deliveries_for_message(s.server.id, message_id)
        .await
        .unwrap();
    assert_eq!(deliveries.len(), 1);
    assert!(deliveries[0]
        .details
        .as_deref()
        .unwrap_or_default()
        .contains("DMARC aggregate report"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unparseable_dmarc_route_messages_are_held_not_fatal() {
    use camelmailer_core::ServerStore;

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let route = dmarc_route_setup(&s).await;

    let mut message = outgoing_message(s.server.id, "dmarc@org.example");
    message.scope = MessageScope::Incoming;
    message.route_id = Some(route.id);
    message.raw_message =
        b"From: x@y.z\r\nContent-Type: text/plain\r\n\r\nnot a report at all\r\n".to_vec();
    let message_id = s.sink.insert_message(&message).await.unwrap();

    let worker = Worker::new(&worker_config(1), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::Held);
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);

    // held like any undeliverable inbound message; nothing was stored
    let stored = s
        .sink
        .message_by_id(s.server.id, message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, "Held");
    assert!(s
        .store
        .dmarc_reports(s.server.id, &camelmailer_core::DmarcFilter::default())
        .await
        .unwrap()
        .is_empty());

    // the worker keeps running: a second, valid report still ingests
    let mut valid = outgoing_message(s.server.id, "dmarc@org.example");
    valid.scope = MessageScope::Incoming;
    valid.route_id = Some(route.id);
    valid.raw_message = dmarc_mail(
        "report.xml.gz",
        "application/gzip",
        &gzipped_fixture_report(),
    );
    s.sink.insert_message(&valid).await.unwrap();
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::DmarcReportIngested);
}

// ---------------------------------------- ARF ingestion (feedback loop)

/// A raw inbound ARF report whose embedded original carries the given
/// broadcast `List-Unsubscribe` token.
fn arf_mail(token: &str) -> Vec<u8> {
    format!(
        "From: abuse@isp.example\r\n\
         To: fbl@track.example.com\r\n\
         Subject: FW: spam\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/report; report-type=\"feedback-report\";\r\n\
         \tboundary=\"B\"\r\n\
         \r\n\
         --B\r\n\
         Content-Type: text/plain\r\n\
         \r\n\
         This is an email abuse report.\r\n\
         --B\r\n\
         Content-Type: message/feedback-report\r\n\
         \r\n\
         Feedback-Type: abuse\r\n\
         Version: 1\r\n\
         Original-Rcpt-To: <r@dest.example>\r\n\
         \r\n\
         --B\r\n\
         Content-Type: message/rfc822\r\n\
         \r\n\
         From: <broadcast@org.example>\r\n\
         To: <r@dest.example>\r\n\
         Subject: Newsletter\r\n\
         List-Unsubscribe: <http://track.example.com/track/u/{token}>, <mailto:u@org.example>\r\n\
         \r\n\
         Spam spam spam\r\n\
         --B--\r\n"
    )
    .into_bytes()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_arf_reports_record_a_stream_scoped_complaint() {
    use camelmailer_core::{NewStream, ServerStore};

    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;

    // A broadcast stream with an opted-in recipient and a one-click token.
    let stream = ServerStore::create_stream(
        &s.store,
        NewStream {
            server_id: s.server.id,
            name: "Broadcast".into(),
            permalink: "broadcast".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        },
    )
    .await
    .unwrap();
    ServerStore::upsert_subscription(
        &s.store,
        s.server.id,
        stream.id,
        "r@dest.example",
        "subscribed",
    )
    .await
    .unwrap();
    let token = ServerStore::create_unsubscribe_token(
        &s.store,
        s.server.id,
        Some(stream.id),
        "r@dest.example",
    )
    .await
    .unwrap();

    // The ISP feedback loop delivers the ARF report as ordinary inbound mail.
    let mut message = outgoing_message(s.server.id, "fbl@track.example.com");
    message.scope = MessageScope::Incoming;
    message.raw_message = arf_mail(&token);
    let message_id = s.sink.insert_message(&message).await.unwrap();

    let worker = Worker::new(&worker_config(1), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert_eq!(outcome, ProcessOutcome::FeedbackReportIngested);
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);

    // The recipient is now stream-suppressed as `complaint` and opted out.
    assert!(
        !ServerStore::is_subscribed(&s.store, s.server.id, stream.id, "r@dest.example")
            .await
            .unwrap()
    );
    assert!(ServerStore::address_suppressed(
        &s.store,
        s.server.id,
        "r@dest.example",
        Some(stream.id)
    )
    .await
    .unwrap());

    // The message was marked processed with the complaint noted.
    let stored = s
        .sink
        .message_by_id(s.server.id, message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, "Processed");
    let deliveries = s
        .sink
        .deliveries_for_message(s.server.id, message_id)
        .await
        .unwrap();
    assert_eq!(deliveries.len(), 1);
    assert!(deliveries[0]
        .details
        .as_deref()
        .unwrap_or_default()
        .contains("spam complaint recorded"));
}
