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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    let pool = camelmailer_db::connect(&format!("{}/{}", &base[..position], db_name), 5)
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
        .create_webhook(NewWebhook {
            server_id: s.server.id,
            name: "hook".into(),
            url: hook.url.clone(),
            all_events: true,
            sign: false,
        })
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
        .create_webhook(NewWebhook {
            server_id: s.server.id,
            name: "hook".into(),
            url: hook.url.clone(),
            all_events: true,
            sign: false,
        })
        .await
        .unwrap();

    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Delayed { .. }));

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
        .create_webhook(NewWebhook {
            server_id: s.server.id,
            name: "hook".into(),
            url: hook.url.clone(),
            all_events: true,
            sign: false,
        })
        .await
        .unwrap();

    s.sink
        .insert_message(&outgoing_message(s.server.id, "user@dest.example"))
        .await
        .unwrap();

    let worker = Worker::new(&worker_config(smtp.port), s.store.clone());
    let outcome = worker.process_next().await.unwrap().unwrap();
    assert!(matches!(outcome, ProcessOutcome::Failed { .. }));
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);
    let hooks = hook.requests.lock().unwrap().clone();
    assert_eq!(hooks[0]["event"], "MessageDeliveryFailed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attempts_exhaust_into_terminal_failure() {
    let base = require_db!();
    let pool = test_pool(&base).await;
    let s = setup(pool).await;
    let smtp = mock_smtp("421 Still broken").await;

    s.sink
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
        } else {
            assert!(matches!(outcome, ProcessOutcome::Failed { .. }));
        }
    }
    assert_eq!(s.queue.queue_size().await.unwrap(), 0);
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
