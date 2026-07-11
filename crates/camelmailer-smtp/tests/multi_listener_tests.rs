//! Multi-port tests: one SmtpServer serving several listeners in parallel
//! (the default-port behaviour plus additional `smtp_server.listeners`).

use base64::Engine;
use camelmailer_config::SmtpListenerMode;
use camelmailer_core::testing::Fixtures;
use camelmailer_core::{CredentialType, MemorySink};
use camelmailer_smtp::SmtpServer;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

struct LineClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
}

impl LineClient {
    async fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: write_half,
        }
    }

    async fn read_line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).await.unwrap();
        line.trim_end().to_string()
    }

    /// Read a full (possibly multiline) reply; returns all lines.
    async fn read_reply(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let line = self.read_line().await;
            let done = line.len() < 4 || line.as_bytes()[3] != b'-';
            lines.push(line);
            if done {
                return lines;
            }
        }
    }

    async fn send(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }

    async fn command(&mut self, line: &str) -> Vec<String> {
        self.send(line).await;
        self.read_reply().await
    }
}

/// Run one authenticated transaction against `port`.
async fn deliver_one(port: u16, credential_key: &str, subject: &str) {
    let mut client = LineClient::connect(port).await;
    let banner = client.read_line().await;
    assert!(banner.starts_with("220 postal.example.com ESMTP CamelMailer/"));

    let reply = client.command("EHLO client.example").await;
    assert!(reply.iter().any(|l| l == "250 AUTH PLAIN LOGIN"));

    let auth = base64::engine::general_purpose::STANDARD.encode(format!("\0XX\0{credential_key}"));
    let reply = client.command(&format!("AUTH PLAIN {auth}")).await;
    assert!(reply[0].starts_with("235 Granted for"));

    let reply = client.command("MAIL FROM:<sender@org.example>").await;
    assert_eq!(reply, vec!["250 OK"]);
    let reply = client.command("RCPT TO:<user@elsewhere.example>").await;
    assert_eq!(reply, vec!["250 OK"]);
    let reply = client.command("DATA").await;
    assert_eq!(reply, vec!["354 Go ahead"]);
    client.send("From: sender@org.example").await;
    client.send(&format!("Subject: {subject}")).await;
    client.send("").await;
    client.send("Hello.").await;
    let reply = client.command(".").await;
    assert_eq!(reply, vec!["250 OK"]);
    client.command("QUIT").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_listeners_accept_mail_in_parallel() {
    let fixtures = Fixtures::new();
    fixtures.verified_server_domain("org.example");
    let credential = fixtures.credential(CredentialType::Smtp, "multi-port-key");
    let sink = Arc::new(MemorySink::new());

    let mut config = camelmailer_config::Config::default();
    config.camelmailer.smtp_hostname = "postal.example.com".into();

    // A "default port"-style listener plus a second smtp-mode listener,
    // both served by the same SmtpServer (as `run()` does for
    // `smtp_server.listeners`).
    let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_port = first.local_addr().unwrap().port();
    let second_port = second.local_addr().unwrap().port();

    let server = SmtpServer::new(config, fixtures.store(), sink.clone());
    tokio::spawn(async move {
        server
            .serve_listeners(vec![
                (first, SmtpListenerMode::Smtp),
                (second, SmtpListenerMode::Smtp),
            ])
            .await
            .ok();
    });

    // Deliver over both ports concurrently.
    tokio::join!(
        deliver_one(first_port, &credential.key, "Via the default port"),
        deliver_one(second_port, &credential.key, "Via the second listener"),
    );

    let messages = sink.messages();
    assert_eq!(messages.len(), 2);
    for message in &messages {
        assert_eq!(message.rcpt_to, "user@elsewhere.example");
        assert!(!message.received_with_ssl);
    }
}
