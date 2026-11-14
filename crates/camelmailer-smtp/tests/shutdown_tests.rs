//! Graceful shutdown of the SMTP server: no new connections once it begins,
//! an open transaction finishes and is queued, and sessions still open after
//! the grace period are told `421 4.3.2` and closed.

use base64::Engine;
use camelmailer_config::SmtpListenerMode;
use camelmailer_core::testing::Fixtures;
use camelmailer_core::{CredentialType, MemorySink};
use camelmailer_smtp::SmtpServer;
use std::sync::Arc;
use std::time::Duration;
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

    /// The next line, or `None` once the server closed the connection.
    async fn read_or_eof(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line).await {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end().to_string()),
        }
    }
}

const SHUTDOWN_REPLY: &str =
    "421 4.3.2 postal.example.com Service shutting down, closing transmission channel";

struct Running {
    port: u16,
    sink: Arc<MemorySink>,
    key: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

async fn start(grace: Duration) -> Running {
    let fixtures = Fixtures::new();
    fixtures.verified_server_domain("org.example");
    let credential = fixtures.credential(CredentialType::Smtp, "shutdown-key");
    let sink = Arc::new(MemorySink::new());
    let mut config = camelmailer_config::Config::default();
    config.camelmailer.smtp_hostname = "postal.example.com".into();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = SmtpServer::new(config, fixtures.store(), sink.clone()).with_shutdown_grace(grace);
    let server = tokio::spawn(async move {
        server
            .serve_listeners_until(vec![(listener, SmtpListenerMode::Smtp)], async move {
                let _ = stopped.await;
            })
            .await
    });
    Running {
        port,
        sink,
        key: credential.key,
        stop: Some(stop),
        server,
    }
}

impl Running {
    fn shut_down(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
    }
}

/// Connect, greet and authenticate.
async fn session(running: &Running) -> LineClient {
    let mut client = LineClient::connect(running.port).await;
    assert!(client.read_line().await.starts_with("220 "));
    client.command("EHLO client.example").await;
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("\0XX\0{}", running.key));
    assert!(client.command(&format!("AUTH PLAIN {auth}")).await[0].starts_with("235"));
    client
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transaction_open_at_shutdown_finishes_and_is_queued() {
    let mut running = start(Duration::from_secs(10)).await;
    let mut client = session(&running).await;
    assert_eq!(
        client.command("MAIL FROM:<sender@org.example>").await,
        vec!["250 OK"]
    );
    assert_eq!(
        client.command("RCPT TO:<user@elsewhere.example>").await,
        vec!["250 OK"]
    );
    assert_eq!(client.command("DATA").await, vec!["354 Go ahead"]);
    client.send("From: sender@org.example").await;
    client.send("Subject: In flight").await;
    client.send("").await;

    // The shutdown begins in the middle of DATA.
    running.shut_down();
    tokio::time::sleep(Duration::from_millis(100)).await;
    // New connections are refused from now on.
    assert!(TcpStream::connect(("127.0.0.1", running.port))
        .await
        .is_err());

    client.send("Hello.").await;
    assert_eq!(
        client.command(".").await,
        vec!["250 OK"],
        "accepted and queued"
    );
    // The transaction is over: the session is closed cleanly.
    assert_eq!(client.read_or_eof().await.as_deref(), Some(SHUTDOWN_REPLY));
    assert_eq!(client.read_or_eof().await, None);

    let messages = running.sink.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].rcpt_to, "user@elsewhere.example");
    // The server is done well before the grace period: no session is left.
    tokio::time::timeout(Duration::from_secs(2), running.server)
        .await
        .expect("the server stops once the last session closed")
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_still_open_after_the_grace_period_get_421() {
    let mut running = start(Duration::from_millis(300)).await;
    // One idle session, one in the middle of a transaction that never ends.
    let mut idle = session(&running).await;
    let mut stuck = session(&running).await;
    assert_eq!(
        stuck.command("MAIL FROM:<sender@org.example>").await,
        vec!["250 OK"]
    );
    assert_eq!(
        stuck.command("RCPT TO:<user@elsewhere.example>").await,
        vec!["250 OK"]
    );
    assert_eq!(stuck.command("DATA").await, vec!["354 Go ahead"]);
    stuck.send("Subject: never finished").await;

    running.shut_down();
    // Within the grace period an idle session may still talk.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(idle.command("NOOP").await, vec!["250 OK"]);

    assert_eq!(idle.read_or_eof().await.as_deref(), Some(SHUTDOWN_REPLY));
    assert_eq!(idle.read_or_eof().await, None);
    assert_eq!(stuck.read_or_eof().await.as_deref(), Some(SHUTDOWN_REPLY));
    assert_eq!(stuck.read_or_eof().await, None);
    // The unfinished message was never told 250, so it is not queued; the
    // client retries it.
    assert!(running.sink.messages().is_empty());
    tokio::time::timeout(Duration::from_secs(3), running.server)
        .await
        .expect("the server stops after the grace period")
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_open_sessions_the_server_stops_at_once() {
    let mut running = start(Duration::from_secs(30)).await;
    running.shut_down();
    tokio::time::timeout(Duration::from_secs(1), running.server)
        .await
        .expect("nothing to wait for")
        .unwrap()
        .unwrap();
    assert!(TcpStream::connect(("127.0.0.1", running.port))
        .await
        .is_err());
}
