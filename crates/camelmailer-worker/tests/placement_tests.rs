//! Seed inbox placement end to end against Postgres, a mock SMTP receiver
//! and a mock IMAP server. No network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use camelmailer_config::{Seed, SeedAuth, SeedProvider};
use camelmailer_core::admin_store::NewIpAddress;
use camelmailer_core::feedback::source;
use camelmailer_core::{AdminStore, FeedbackStore, NewOrganization, NewServer, ServerMode};
use camelmailer_db::{PgMessageSink, PgStore};
use camelmailer_worker::placement::PlacementRunner;
use camelmailer_worker::{ProcessOutcome, Worker};
use rand::RngExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

async fn test_pool() -> Option<sqlx::PgPool> {
    let base = std::env::var("CAMELMAILER_TEST_DATABASE_URL").ok()?;
    let name: String = {
        let mut rng = rand::rng();
        (0..12)
            .map(|_| char::from(b'a' + rng.random_range(0..26)))
            .collect()
    };
    let db_name = format!("cm_test_{name}");
    let admin_pool = camelmailer_db::connect(&base, 1).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {db_name}"))
        .execute(&admin_pool)
        .await
        .unwrap();
    admin_pool.close().await;
    let position = base.rfind('/').unwrap();
    let pool = camelmailer_db::connect(&format!("{}/{}", &base[..position], db_name), 4)
        .await
        .unwrap();
    camelmailer_db::migrate(&pool).await.unwrap();
    Some(pool)
}

/// SMTP receiver that captures every message.
async fn mock_smtp() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let messages: Arc<Mutex<Vec<String>>> = Arc::default();
    let captured = messages.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let captured = captured.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                write.write_all(b"220 mock\r\n").await.ok();
                let (mut data, mut body) = (false, String::new());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if data {
                        if line.trim_end() == "." {
                            data = false;
                            captured.lock().unwrap().push(std::mem::take(&mut body));
                            write.write_all(b"250 OK\r\n").await.ok();
                        } else {
                            body.push_str(&line);
                        }
                        continue;
                    }
                    let upper = line.to_ascii_uppercase();
                    let reply = if upper.starts_with("EHLO") {
                        "250-mock\r\n250 OK\r\n"
                    } else if upper.starts_with("DATA") {
                        data = true;
                        "354 go\r\n"
                    } else if upper.starts_with("QUIT") {
                        write.write_all(b"221 bye\r\n").await.ok();
                        return;
                    } else {
                        "250 OK\r\n"
                    };
                    write.write_all(reply.as_bytes()).await.ok();
                }
            });
        }
    });
    (port, messages)
}

/// mailbox -> [(uid, token, header)]
type Mailboxes = Arc<Mutex<HashMap<String, Vec<(u32, String, String)>>>>;

/// A minimal IMAP server: LOGIN, LIST, EXAMINE, UID SEARCH HEADER, UID FETCH,
/// LOGOUT. Records every command so the test can prove nothing writes.
async fn mock_imap(mailboxes: Mailboxes) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = commands.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let log = log.clone();
            let mailboxes = mailboxes.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                write.write_all(b"* OK IMAP4rev1 mock ready\r\n").await.ok();
                let mut selected = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let line = line.trim_end().to_string();
                    let (tag, command) = line.split_once(' ').unwrap();
                    log.lock().unwrap().push(command.to_string());
                    let upper = command.to_ascii_uppercase();
                    let mut out = String::new();
                    if upper.starts_with("LOGIN") {
                        if !command.contains("\"app-password\"") {
                            out.push_str(&format!("{tag} NO [AUTHENTICATIONFAILED] bad\r\n"));
                            write.write_all(out.as_bytes()).await.ok();
                            continue;
                        }
                    } else if upper.starts_with("LIST") {
                        out.push_str("* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n");
                        out.push_str("* LIST (\\HasNoChildren \\Junk) \"/\" \"Junk Mail\"\r\n");
                    } else if upper.starts_with("EXAMINE") {
                        selected = command[8..].trim_matches('"').to_string();
                        out.push_str("* 2 EXISTS\r\n");
                    } else if upper.starts_with("UID SEARCH") {
                        let token = command.rsplit(' ').next().unwrap().trim_matches('"');
                        let hits: Vec<String> = mailboxes
                            .lock()
                            .unwrap()
                            .get(&selected)
                            .into_iter()
                            .flatten()
                            .filter(|(_, t, _)| t == token)
                            .map(|(uid, _, _)| uid.to_string())
                            .collect();
                        out.push_str(&format!("* SEARCH {}\r\n", hits.join(" ")));
                    } else if upper.starts_with("UID FETCH") {
                        let uid: u32 = command.split_whitespace().nth(2).unwrap().parse().unwrap();
                        let header = mailboxes
                            .lock()
                            .unwrap()
                            .get(&selected)
                            .and_then(|m| {
                                m.iter()
                                    .find(|(u, _, _)| *u == uid)
                                    .map(|(_, _, h)| h.clone())
                            })
                            .unwrap();
                        out.push_str(&format!(
                            "* 1 FETCH (UID {uid} BODY[HEADER] {{{}}}\r\n{header})\r\n",
                            header.len()
                        ));
                    } else if upper.starts_with("LOGOUT") {
                        out.push_str("* BYE\r\n");
                        out.push_str(&format!("{tag} OK done\r\n"));
                        write.write_all(out.as_bytes()).await.ok();
                        return;
                    }
                    out.push_str(&format!("{tag} OK done\r\n"));
                    write.write_all(out.as_bytes()).await.ok();
                }
            });
        }
    });
    (port, commands)
}

fn received_header(token: &str, ip: &str, spf: &str) -> String {
    format!(
        "Received: from out1.example.com (out1.example.com. [{ip}])\r\n by mx.seed.example with ESMTPS;\r\n \
         Fri, 13 Nov 2026 10:00:00 +0000\r\n\
         Authentication-Results: mx.seed.example; spf={spf} smtp.mailfrom=example.com;\r\n \
         dkim=pass header.d=example.com; dmarc=pass header.from=example.com\r\n\
         X-Camelmailer-Placement-Test: {token}\r\n\r\n"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_round_sends_from_each_address_and_finds_inbox_and_spam_read_only() {
    let Some(pool) = test_pool().await else {
        eprintln!("skipping: CAMELMAILER_TEST_DATABASE_URL is not set");
        return;
    };
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
            name: "Placement".into(),
            permalink: "placement".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let pool_row = store.create_ip_pool("out", false).await.unwrap();
    let address = store
        .create_ip_address(NewIpAddress {
            ip_pool_id: pool_row.id,
            ipv4: "127.0.0.1".into(),
            ipv6: None,
            hostname: "out1.example.com".into(),
            priority: 100,
        })
        .await
        .unwrap();

    let (smtp_port, smtp_messages) = mock_smtp().await;
    let mailboxes: Mailboxes = Arc::default();
    let (imap_port, imap_commands) = mock_imap(mailboxes.clone()).await;

    let mut config = camelmailer_config::Config::default();
    config.camelmailer.smtp_relays = vec![format!("smtp://127.0.0.1:{smtp_port}")];
    config.smtp_client.open_timeout = 5;
    let placement = &mut config.deliverability_feedback.placement;
    placement.server = "org/placement".into();
    placement.from_address = "placement@example.com".into();
    let seed = |address: &str| Seed {
        provider: SeedProvider::Imap,
        address: address.into(),
        imap_host: Some("127.0.0.1".into()),
        imap_port: Some(imap_port),
        imap_tls: Some(false),
        auth: SeedAuth::AppPassword,
        password_env: Some("SEED_PASSWORD".into()),
        ..Default::default()
    };
    placement.seeds = vec![seed("a@seed.example"), seed("b@seed.example")];
    placement.max_checks = 2;

    let runner = PlacementRunner::new(&config, store.clone())
        .with_lookup(|name| (name == "SEED_PASSWORD").then(|| "app-password".to_string()));
    // On demand: one round, then the rate limit.
    assert!(store
        .request_placement_round("manual", 3600)
        .await
        .unwrap()
        .is_some());
    assert!(store
        .request_placement_round("manual", 3600)
        .await
        .unwrap()
        .is_none());
    runner.run_once().await.unwrap();
    let tests = store.list_placement_tests(10).await.unwrap();
    assert_eq!(tests.len(), 2, "one address times two seeds");
    assert!(tests
        .iter()
        .all(|t| t.message_id.is_some() && t.status == "pending"));

    // The worker sends each test from the address it names.
    let worker = Worker::new(&config, store.clone());
    let sink = PgMessageSink::new(store.clone());
    for test in &tests {
        let message = sink
            .message_by_id(server.id, test.message_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            worker.placement_address(&message).await.map(|a| a.id),
            Some(address.id)
        );
    }
    for _ in 0..2 {
        assert!(matches!(
            worker.process_next().await.unwrap().unwrap(),
            ProcessOutcome::Delivered { .. }
        ));
    }
    let sent = smtp_messages.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert!(sent
        .iter()
        .all(|m| m.contains("X-Camelmailer-Placement-Test: ")
            && m.contains("[Placement test] 127.0.0.1")));

    // Seed a finds it in the inbox, seed b in the spam folder.
    let by_seed = |seed: &str| tests.iter().find(|t| t.seed_address == seed).unwrap();
    let (a, b) = (by_seed("a@seed.example"), by_seed("b@seed.example"));
    {
        let mut boxes = mailboxes.lock().unwrap();
        boxes.insert(
            "INBOX".into(),
            vec![(
                7,
                a.token.clone(),
                received_header(&a.token, "127.0.0.1", "pass"),
            )],
        );
        boxes.insert(
            "Junk Mail".into(),
            vec![(
                9,
                b.token.clone(),
                received_header(&b.token, "127.0.0.1", "softfail"),
            )],
        );
    }
    sqlx::query("UPDATE placement_tests SET check_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    runner.run_once().await.unwrap();

    let results = store.list_placement_tests(10).await.unwrap();
    let result = |id: i64| results.iter().find(|t| t.id == id).unwrap();
    assert_eq!(result(a.id).status, "inbox");
    assert_eq!(result(a.id).folder.as_deref(), Some("INBOX"));
    assert_eq!(result(a.id).spf.as_deref(), Some("pass"));
    assert_eq!(result(a.id).dkim.as_deref(), Some("pass"));
    assert_eq!(result(a.id).received_ip.as_deref(), Some("127.0.0.1"));
    assert_eq!(result(a.id).helo.as_deref(), Some("out1.example.com"));
    assert_eq!(result(b.id).status, "spam");
    assert_eq!(result(b.id).folder.as_deref(), Some("Junk Mail"));
    assert_eq!(result(b.id).spf.as_deref(), Some("softfail"));

    // Read-only: only commands that never change a mailbox.
    for command in imap_commands.lock().unwrap().iter() {
        let verb = command
            .split_whitespace()
            .next()
            .unwrap()
            .to_ascii_uppercase();
        assert!(
            ["LOGIN", "LIST", "EXAMINE", "UID", "LOGOUT"].contains(&verb.as_str()),
            "{command}"
        );
        assert!(!command.to_ascii_uppercase().contains("STORE"));
        assert!(
            !command.to_ascii_uppercase().starts_with("UID FETCH") || command.contains("BODY.PEEK"),
            "{command}"
        );
    }

    // The results became placement signals; the source is on.
    let snapshots = store
        .latest_feedback_snapshots(source::PLACEMENT)
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 1, "one IP, provider group other");
    let statuses = store.feedback_source_statuses().await.unwrap();
    assert!(statuses
        .iter()
        .any(|s| s.source == "placement" && s.enabled));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_token_of_the_runner_on_its_server_picks_the_address() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let store = PgStore::new(pool.clone());
    let organization = store
        .create_organization(NewOrganization {
            name: "Org".into(),
            permalink: "org".into(),
        })
        .await
        .unwrap();
    let mut servers = Vec::new();
    for permalink in ["placement", "tenant"] {
        servers.push(
            store
                .create_server(NewServer {
                    organization_id: organization.id,
                    name: permalink.into(),
                    permalink: permalink.into(),
                    mode: ServerMode::Live,
                })
                .await
                .unwrap(),
        );
    }
    let pool_row = store.create_ip_pool("out", false).await.unwrap();
    let address = store
        .create_ip_address(NewIpAddress {
            ip_pool_id: pool_row.id,
            ipv4: "127.0.0.1".into(),
            ipv6: None,
            hostname: "out1.example.com".into(),
            priority: 100,
        })
        .await
        .unwrap();
    let round = store
        .request_placement_round("manual", 0)
        .await
        .unwrap()
        .unwrap();
    store
        .create_placement_tests(&[camelmailer_core::NewPlacementTest {
            round_id: round.id,
            token: "knowntoken".into(),
            server_id: servers[0].id,
            ip_pool_id: pool_row.id,
            ip_address_id: address.id,
            ip: "127.0.0.1".into(),
            seed_address: "a@seed.example".into(),
            seed_provider: "imap".into(),
            provider: "other".into(),
            check_at: chrono::Utc::now(),
        }])
        .await
        .unwrap();
    let worker = Worker::new(&camelmailer_config::Config::default(), store.clone());
    let sink = PgMessageSink::new(store.clone());
    let message = |server_id, header: &str| camelmailer_core::QueuedMessage {
        server_id,
        rcpt_to: "a@seed.example".into(),
        mail_from: "x@example.com".into(),
        raw_message: format!("{header}Subject: hi\r\n\r\nbody\r\n").into_bytes(),
        received_with_ssl: false,
        scope: camelmailer_core::MessageScope::Outgoing,
        bounce: false,
        domain_id: None,
        credential_id: None,
        route_id: None,
        tag: None,
        metadata: None,
        stream_id: None,
    };
    for (server_id, header, expected) in [
        (
            servers[0].id,
            "X-Camelmailer-Placement-Test: knowntoken\r\n",
            Some(address.id),
        ),
        (
            servers[1].id,
            "X-Camelmailer-Placement-Test: knowntoken\r\n",
            None,
        ),
        (
            servers[0].id,
            "X-Camelmailer-Placement-Test: guessed\r\n",
            None,
        ),
        (servers[0].id, "", None),
    ] {
        let id = sink
            .insert_message(&message(server_id, header))
            .await
            .unwrap();
        let stored = sink.message_by_id(server_id, id).await.unwrap().unwrap();
        assert_eq!(
            worker.placement_address(&stored).await.map(|a| a.id),
            expected,
            "{server_id} {header:?}"
        );
    }
}
