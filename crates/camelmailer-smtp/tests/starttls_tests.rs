//! End-to-end STARTTLS tests: a real TCP server, a real rustls handshake
//! with a self-signed certificate, and a full authenticated transaction
//! over the encrypted stream.

use camelmailer_config::SmtpListenerMode;
use camelmailer_core::testing::Fixtures;
use camelmailer_core::{CredentialType, MemorySink};
use camelmailer_smtp::SmtpServer;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

fn write_self_signed_cert(directory: &std::path::Path) -> (String, String) {
    let (cert_path, key_path, _) = write_cert_for(directory, "postal.example.com");
    (cert_path, key_path)
}

/// Writes `smtp.cert` / `smtp.key` for one DNS name and hands back the DER of
/// the certificate, so a test can tell which one a handshake presented.
fn write_cert_for(directory: &std::path::Path, dns_name: &str) -> (String, String, Vec<u8>) {
    let certified = rcgen::generate_simple_self_signed(vec![dns_name.into()]).unwrap();
    let cert_path = directory.join("smtp.cert");
    let key_path = directory.join("smtp.key");
    std::fs::write(&cert_path, certified.cert.pem()).unwrap();
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).unwrap();
    (
        cert_path.to_string_lossy().into_owned(),
        key_path.to_string_lossy().into_owned(),
        certified.cert.der().to_vec(),
    )
}

/// Pushes both files' modification times forward. Writing a file twice in
/// quick succession can leave the timestamp unchanged, and the reload keys on
/// exactly that, so the test states the renewal rather than hoping for it.
fn stamp_renewed(cert_path: &str, key_path: &str) {
    let times = std::fs::FileTimes::new()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2));
    for path in [cert_path, key_path] {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(times)
            .unwrap();
    }
}

/// A rustls client verifier that accepts any certificate (tests only) and
/// records what the server presented, in handshake order.
#[derive(Debug)]
struct AcceptAnyCert {
    provider: rustls::crypto::CryptoProvider,
    presented: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        self.presented.lock().unwrap().push(end_entity.to_vec());
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

struct SmtpTestClient<S> {
    reader: BufReader<tokio::io::ReadHalf<S>>,
    writer: tokio::io::WriteHalf<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SmtpTestClient<S> {
    fn new(stream: S) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
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

    fn into_inner(self) -> S {
        self.reader.into_inner().unsplit(self.writer)
    }
}

async fn start_server(fixtures: &Fixtures, sink: Arc<MemorySink>) -> u16 {
    start_server_with_mode(fixtures, sink, SmtpListenerMode::Smtp).await
}

async fn start_server_with_mode(
    fixtures: &Fixtures,
    sink: Arc<MemorySink>,
    mode: SmtpListenerMode,
) -> u16 {
    start_server_with(fixtures, sink, mode, false).await
}

async fn start_server_with(
    fixtures: &Fixtures,
    sink: Arc<MemorySink>,
    mode: SmtpListenerMode,
    proxy_protocol: bool,
) -> u16 {
    start_server_full(fixtures, sink, mode, proxy_protocol, None).await
}

async fn start_server_full(
    fixtures: &Fixtures,
    sink: Arc<MemorySink>,
    mode: SmtpListenerMode,
    proxy_protocol: bool,
    proxy_header_timeout: Option<std::time::Duration>,
) -> u16 {
    // Every server gets its own certificate directory: the tests in this
    // file run concurrently in one process, and a shared path let one test
    // overwrite another's cert/key mid-write, producing a mismatched pair
    // and a flaky ConnectionReset during the handshake.
    static CERT_DIR_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "cm-starttls-{}-{}",
        std::process::id(),
        CERT_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let (cert_path, key_path) = write_self_signed_cert(&directory);

    let mut config = camelmailer_config::Config::default();
    config.camelmailer.smtp_hostname = "postal.example.com".into();
    config.smtp_server.tls_enabled = true;
    config.smtp_server.tls_certificate_path = cert_path;
    config.smtp_server.tls_private_key_path = key_path;
    config.smtp_server.proxy_protocol = proxy_protocol;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut server = SmtpServer::new(config, fixtures.store(), sink);
    if let Some(timeout) = proxy_header_timeout {
        server = server.with_proxy_header_timeout(timeout);
    }
    tokio::spawn(async move {
        server.serve_listeners(vec![(listener, mode)]).await.ok();
    });
    port
}

fn tls_connector() -> tokio_rustls::TlsConnector {
    recording_tls_connector().0
}

/// A connector plus the list it records the server's certificate into.
fn recording_tls_connector() -> (
    tokio_rustls::TlsConnector,
    Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
) {
    let provider = rustls::crypto::ring::default_provider();
    let _ = provider.clone().install_default();
    let presented = Arc::new(std::sync::Mutex::new(Vec::new()));
    let client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert {
            provider: rustls::crypto::ring::default_provider(),
            presented: presented.clone(),
        }))
        .with_no_client_auth();
    (
        tokio_rustls::TlsConnector::from(Arc::new(client_config)),
        presented,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starttls_upgrades_the_session_and_delivers_authenticated_mail() {
    let fixtures = Fixtures::new();
    fixtures.verified_server_domain("org.example");
    let credential = fixtures.credential(CredentialType::Smtp, "tls-test-key");
    let sink = Arc::new(MemorySink::new());
    let port = start_server(&fixtures, sink.clone()).await;

    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut client = SmtpTestClient::new(stream);

    let banner = client.read_line().await;
    assert!(banner.starts_with("220 postal.example.com ESMTP Camelmailer/"));

    // Before the upgrade: STARTTLS offered, AUTH withheld
    let reply = client.command("EHLO client.example").await;
    assert!(reply.iter().any(|l| l == "250 STARTTLS"));
    assert!(!reply.iter().any(|l| l.contains("AUTH")));

    // AUTH before TLS finds no mechanism (defense in depth: even if a client
    // tries, the credential lookup runs — but capability-wise it must not be
    // advertised). Now upgrade:
    let reply = client.command("STARTTLS").await;
    assert_eq!(reply, vec!["220 Ready to start TLS"]);

    let tcp = client.into_inner();
    let connector = tls_connector();
    let server_name = rustls::pki_types::ServerName::try_from("postal.example.com").unwrap();
    let tls_stream = connector.connect(server_name, tcp).await.unwrap();
    let mut client = SmtpTestClient::new(tls_stream);

    // After the upgrade: AUTH advertised, STARTTLS gone
    let reply = client.command("EHLO client.example").await;
    assert!(reply.iter().any(|l| l == "250 AUTH PLAIN LOGIN"));
    assert!(!reply.iter().any(|l| l.contains("STARTTLS")));

    // Full authenticated transaction over TLS
    use base64::Engine;
    let auth =
        base64::engine::general_purpose::STANDARD.encode(format!("\0XX\0{}", credential.key));
    let reply = client.command(&format!("AUTH PLAIN {auth}")).await;
    assert!(reply[0].starts_with("235 Granted for"));

    let reply = client.command("MAIL FROM:<sender@org.example>").await;
    assert_eq!(reply, vec!["250 OK"]);
    let reply = client.command("RCPT TO:<user@elsewhere.example>").await;
    assert_eq!(reply, vec!["250 OK"]);
    let reply = client.command("DATA").await;
    assert_eq!(reply, vec!["354 Go ahead"]);
    client.send("From: sender@org.example").await;
    client.send("Subject: Over TLS").await;
    client.send("").await;
    client.send("Encrypted hello.").await;
    let reply = client.command(".").await;
    assert_eq!(reply, vec!["250 OK"]);
    client.command("QUIT").await;

    let messages = sink.messages();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].received_with_ssl, "message must be marked TLS");
    assert_eq!(messages[0].rcpt_to, "user@elsewhere.example");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtps_listener_speaks_tls_from_the_first_byte() {
    let fixtures = Fixtures::new();
    fixtures.verified_server_domain("org.example");
    let credential = fixtures.credential(CredentialType::Smtp, "smtps-test-key");
    let sink = Arc::new(MemorySink::new());
    let port = start_server_with_mode(&fixtures, sink.clone(), SmtpListenerMode::Smtps).await;

    // The TLS handshake happens immediately on connect — before any SMTP
    // byte; the banner arrives over the encrypted stream.
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let connector = tls_connector();
    let server_name = rustls::pki_types::ServerName::try_from("postal.example.com").unwrap();
    let tls_stream = connector.connect(server_name, tcp).await.unwrap();
    let mut client = SmtpTestClient::new(tls_stream);

    let banner = client.read_line().await;
    assert!(banner.starts_with("220 postal.example.com ESMTP Camelmailer/"));

    // The session starts in the TLS state: AUTH is advertised right away
    // and STARTTLS is not offered (exactly as after a STARTTLS upgrade).
    let reply = client.command("EHLO client.example").await;
    assert!(reply.iter().any(|l| l == "250 AUTH PLAIN LOGIN"));
    assert!(!reply.iter().any(|l| l.contains("STARTTLS")));

    use base64::Engine;
    let auth =
        base64::engine::general_purpose::STANDARD.encode(format!("\0XX\0{}", credential.key));
    let reply = client.command(&format!("AUTH PLAIN {auth}")).await;
    assert!(reply[0].starts_with("235 Granted for"));

    let reply = client.command("MAIL FROM:<sender@org.example>").await;
    assert_eq!(reply, vec!["250 OK"]);
    let reply = client.command("RCPT TO:<user@elsewhere.example>").await;
    assert_eq!(reply, vec!["250 OK"]);
    let reply = client.command("DATA").await;
    assert_eq!(reply, vec!["354 Go ahead"]);
    client.send("From: sender@org.example").await;
    client.send("Subject: Implicit TLS").await;
    client.send("").await;
    client.send("Hello over smtps.").await;
    let reply = client.command(".").await;
    assert_eq!(reply, vec!["250 OK"]);
    client.command("QUIT").await;

    let messages = sink.messages();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].received_with_ssl, "message must be marked TLS");
    assert_eq!(messages[0].rcpt_to, "user@elsewhere.example");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plaintext_sessions_still_work_when_tls_is_enabled() {
    let fixtures = Fixtures::new();
    let sink = Arc::new(MemorySink::new());
    let port = start_server(&fixtures, sink).await;

    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut client = SmtpTestClient::new(stream);
    client.read_line().await;
    let reply = client.command("NOOP").await;
    assert_eq!(reply, vec!["250 OK"]);
    let reply = client.command("QUIT").await;
    assert_eq!(reply, vec!["221 Closing Connection"]);
}

/// Starts an smtps listener whose certificate the caller can replace on disk,
/// handing back the directory the pair lives in.
async fn start_server_with_replaceable_cert(
    fixtures: &Fixtures,
    sink: Arc<MemorySink>,
) -> (u16, std::path::PathBuf, Vec<u8>) {
    let directory = std::env::temp_dir().join(format!("cm-cert-reload-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let (cert_path, key_path, der) = write_cert_for(&directory, "postal.example.com");

    let mut config = camelmailer_config::Config::default();
    config.camelmailer.smtp_hostname = "postal.example.com".into();
    config.smtp_server.tls_enabled = true;
    config.smtp_server.tls_certificate_path = cert_path;
    config.smtp_server.tls_private_key_path = key_path;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = SmtpServer::new(config, fixtures.store(), sink);
    tokio::spawn(async move {
        server
            .serve_listeners(vec![(listener, SmtpListenerMode::Smtps)])
            .await
            .ok();
    });
    (port, directory, der)
}

/// A certificate renewal has to reach a running server. The acceptor used to
/// be built once when the listeners were bound, so an ACME renewal every ~60
/// days left the process presenting the expired certificate until somebody
/// restarted it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_renewed_certificate_is_served_without_a_restart() {
    let fixtures = Fixtures::new();
    let sink = Arc::new(MemorySink::new());
    let (port, directory, first) = start_server_with_replaceable_cert(&fixtures, sink).await;
    let (connector, presented) = recording_tls_connector();

    let handshake = |connector: tokio_rustls::TlsConnector| async move {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let name = rustls::pki_types::ServerName::try_from("postal.example.com").unwrap();
        connector.connect(name, tcp).await.unwrap();
    };

    handshake(connector.clone()).await;
    assert_eq!(
        presented.lock().unwrap().clone(),
        vec![first.clone()],
        "the first handshake presents the certificate on disk"
    );

    let (cert_path, key_path, renewed) = write_cert_for(&directory, "renewed.example.com");
    stamp_renewed(&cert_path, &key_path);
    assert_ne!(first, renewed, "the renewal has to be a different cert");

    handshake(connector).await;
    assert_eq!(
        presented.lock().unwrap().clone(),
        vec![first, renewed],
        "the second handshake has to present the renewed certificate"
    );
}

/// Turning `tls_enabled` on makes AUTH require TLS, which breaks a client
/// that authenticates in the clear today. `auth_requires_tls: false` is the
/// window that lets such an installation offer STARTTLS first and move its
/// clients afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_stays_available_in_the_clear_while_auth_requires_tls_is_off() {
    let fixtures = Fixtures::new();
    fixtures.verified_server_domain("org.example");
    let credential = fixtures.credential(CredentialType::Smtp, "migration-window-key");
    let sink = Arc::new(MemorySink::new());

    let directory = std::env::temp_dir().join(format!("cm-authtls-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let (cert_path, key_path) = write_self_signed_cert(&directory);
    let mut config = camelmailer_config::Config::default();
    config.camelmailer.smtp_hostname = "postal.example.com".into();
    config.smtp_server.tls_enabled = true;
    config.smtp_server.auth_requires_tls = false;
    config.smtp_server.tls_certificate_path = cert_path;
    config.smtp_server.tls_private_key_path = key_path;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = SmtpServer::new(config, fixtures.store(), sink);
    tokio::spawn(async move {
        server
            .serve_listeners(vec![(listener, SmtpListenerMode::Smtp)])
            .await
            .ok();
    });

    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut client = SmtpTestClient::new(stream);
    client.read_line().await;

    // Both are offered: STARTTLS so a client can upgrade on its own, AUTH so
    // one that has not yet keeps working.
    let reply = client.command("EHLO client.example").await;
    assert!(reply
        .iter()
        .any(|l| l == "250-STARTTLS" || l == "250 STARTTLS"));
    assert!(reply.iter().any(|l| l.contains("AUTH PLAIN LOGIN")));

    use base64::Engine;
    let auth =
        base64::engine::general_purpose::STANDARD.encode(format!("\0XX\0{}", credential.key));
    let reply = client.command(&format!("AUTH PLAIN {auth}")).await;
    assert!(
        reply[0].starts_with("235 Granted for"),
        "unprotected AUTH must still be accepted, got {:?}",
        reply
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtps_behind_a_proxy_reads_the_proxy_line_before_the_handshake() {
    let fixtures = Fixtures::new();
    fixtures.verified_server_domain("org.example");
    let credential = fixtures.credential(CredentialType::Smtp, "smtps-proxy-key");
    let sink = Arc::new(MemorySink::new());
    let port = start_server_with(&fixtures, sink.clone(), SmtpListenerMode::Smtps, true).await;

    // A load balancer writes the PROXY line in the clear, then the client's
    // TLS handshake follows on the same connection.
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tcp.write_all(b"PROXY TCP4 203.0.113.7 10.0.70.5 40000 465\r\n")
        .await
        .unwrap();
    let connector = tls_connector();
    let server_name = rustls::pki_types::ServerName::try_from("postal.example.com").unwrap();
    let tls_stream = connector.connect(server_name, tcp).await.unwrap();
    let mut client = SmtpTestClient::new(tls_stream);

    // The banner comes over TLS, once.
    let banner = client.read_line().await;
    assert!(banner.starts_with("220 postal.example.com ESMTP Camelmailer/"));
    let reply = client.command("EHLO client.example").await;
    assert!(reply.iter().any(|l| l == "250 AUTH PLAIN LOGIN"));

    use base64::Engine;
    let auth =
        base64::engine::general_purpose::STANDARD.encode(format!("\0XX\0{}", credential.key));
    let reply = client.command(&format!("AUTH PLAIN {auth}")).await;
    assert!(reply[0].starts_with("235 Granted for"));
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
    client.send("Subject: Behind a proxy").await;
    client.send("").await;
    client.send("Hello.").await;
    assert_eq!(client.command(".").await, vec!["250 OK"]);
    client.command("QUIT").await;

    // The client IP is the one from the PROXY line, not the socket peer.
    let messages = sink.messages();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].received_with_ssl);
    let raw = String::from_utf8_lossy(&messages[0].raw_message);
    assert!(
        raw.contains("203.0.113.7"),
        "Received header carries the proxied IP: {raw}"
    );
    assert!(!raw.contains("127.0.0.1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtps_behind_a_proxy_closes_a_connection_without_a_valid_header() {
    let fixtures = Fixtures::new();
    let sink = Arc::new(MemorySink::new());
    let port = start_server_with(&fixtures, sink.clone(), SmtpListenerMode::Smtps, true).await;
    let server_name = rustls::pki_types::ServerName::try_from("postal.example.com").unwrap();

    // A client that starts TLS directly, bypassing the load balancer.
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let direct = tls_connector().connect(server_name.clone(), tcp).await;
    assert!(
        direct.is_err(),
        "a handshake without a PROXY line must not succeed"
    );

    // A malformed PROXY line.
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tcp.write_all(b"PROXY nonsense\r\n").await.unwrap();
    let malformed = tls_connector().connect(server_name, tcp).await;
    assert!(
        malformed.is_err(),
        "an invalid PROXY line must not reach TLS"
    );
    assert!(sink.messages().is_empty());
}

/// Reads until the server closes the connection; returns what it sent.
/// Panics when the server keeps the connection open past `within`.
async fn read_until_closed(stream: &mut TcpStream, within: std::time::Duration) -> String {
    use tokio::io::AsyncReadExt;
    let mut received = Vec::new();
    tokio::time::timeout(within, stream.read_to_end(&mut received))
        .await
        .expect("the server must close the connection")
        .ok();
    String::from_utf8_lossy(&received).into_owned()
}

/// Sends a whole authenticated transaction and asserts it was accepted.
async fn send_one_message<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut SmtpTestClient<S>,
    credential_key: &str,
) {
    use base64::Engine;
    let reply = client.command("EHLO client.example").await;
    assert!(reply[0].starts_with("250"), "{reply:?}");
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("\0XX\0{credential_key}"));
    let reply = client.command(&format!("AUTH PLAIN {auth}")).await;
    assert!(reply[0].starts_with("235 Granted for"), "{reply:?}");
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
    client.send("Subject: Behind a proxy").await;
    client.send("").await;
    client.send("Hello.").await;
    assert_eq!(client.command(".").await, vec!["250 OK"]);
    client.command("QUIT").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtps_behind_a_proxy_accepts_proxy_unknown_with_the_socket_address() {
    // A balancer health check sends `PROXY UNKNOWN`; the spec says accept it
    // and use the socket's own address.
    let fixtures = Fixtures::new();
    fixtures.verified_server_domain("org.example");
    let credential = fixtures.credential(CredentialType::Smtp, "smtps-unknown-key");
    let sink = Arc::new(MemorySink::new());
    let port = start_server_with(&fixtures, sink.clone(), SmtpListenerMode::Smtps, true).await;

    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tcp.write_all(b"PROXY UNKNOWN\r\n").await.unwrap();
    let server_name = rustls::pki_types::ServerName::try_from("postal.example.com").unwrap();
    let tls_stream = tls_connector().connect(server_name, tcp).await.unwrap();
    let mut client = SmtpTestClient::new(tls_stream);
    assert!(client
        .read_line()
        .await
        .starts_with("220 postal.example.com ESMTP Camelmailer/"));
    send_one_message(&mut client, &credential.key).await;

    let messages = sink.messages();
    assert_eq!(messages.len(), 1);
    let raw = String::from_utf8_lossy(&messages[0].raw_message);
    assert!(raw.contains("127.0.0.1"), "socket address used: {raw}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtps_behind_a_proxy_closes_a_header_whose_client_is_not_an_ip() {
    let fixtures = Fixtures::new();
    let sink = Arc::new(MemorySink::new());
    let port = start_server_with(&fixtures, sink.clone(), SmtpListenerMode::Smtps, true).await;
    let server_name = rustls::pki_types::ServerName::try_from("postal.example.com").unwrap();

    for header in [
        &b"PROXY TCP4 evil.example 10.0.70.5 40000 465\r\n"[..],
        &b"PROXY TCP4 203.0.113.7 10.0.70.5 notaport 465\r\n"[..],
        &b"PROXY TCP6 203.0.113.7 10.0.70.5 40000 465\r\n"[..],
    ] {
        let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        tcp.write_all(header).await.unwrap();
        let handshake = tls_connector().connect(server_name.clone(), tcp).await;
        assert!(
            handshake.is_err(),
            "{} must not reach TLS",
            String::from_utf8_lossy(header).trim_end()
        );
    }
    assert!(sink.messages().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtps_behind_a_proxy_closes_a_header_past_the_v1_length_cap() {
    let fixtures = Fixtures::new();
    let sink = Arc::new(MemorySink::new());
    let port = start_server_with(&fixtures, sink, SmtpListenerMode::Smtps, true).await;

    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    // 107 bytes with no line end: one more than a v1 header may take.
    tcp.write_all(&[b'A'; 107]).await.unwrap();
    let received = read_until_closed(&mut tcp, std::time::Duration::from_secs(5)).await;
    assert!(
        received.is_empty(),
        "nothing in the clear on smtps: {received}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtps_behind_a_proxy_closes_a_connection_that_sends_no_header_in_time() {
    let fixtures = Fixtures::new();
    let sink = Arc::new(MemorySink::new());
    let port = start_server_full(
        &fixtures,
        sink,
        SmtpListenerMode::Smtps,
        true,
        Some(std::time::Duration::from_millis(200)),
    )
    .await;

    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    // Half a header, then silence.
    tcp.write_all(b"PROXY TCP4 203.0").await.unwrap();
    let received = read_until_closed(&mut tcp, std::time::Duration::from_secs(5)).await;
    assert!(received.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtp_behind_a_proxy_delivers_a_message_with_the_proxied_ip() {
    // The plain listener behind a balancer, end to end: PROXY line, banner,
    // STARTTLS, AUTH, one message.
    let fixtures = Fixtures::new();
    fixtures.verified_server_domain("org.example");
    let credential = fixtures.credential(CredentialType::Smtp, "smtp-proxy-key");
    let sink = Arc::new(MemorySink::new());
    let port = start_server_with(&fixtures, sink.clone(), SmtpListenerMode::Smtp, true).await;

    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tcp.write_all(b"PROXY TCP4 203.0.113.9 10.0.70.5 40000 25\r\n")
        .await
        .unwrap();
    let mut client = SmtpTestClient::new(tcp);
    assert!(client
        .read_line()
        .await
        .starts_with("220 postal.example.com ESMTP Camelmailer/"));
    client.command("EHLO client.example").await;
    assert_eq!(
        client.command("STARTTLS").await[0],
        "220 Ready to start TLS"
    );
    let server_name = rustls::pki_types::ServerName::try_from("postal.example.com").unwrap();
    let tls_stream = tls_connector()
        .connect(server_name, client.into_inner())
        .await
        .unwrap();
    let mut client = SmtpTestClient::new(tls_stream);
    send_one_message(&mut client, &credential.key).await;

    let messages = sink.messages();
    assert_eq!(messages.len(), 1);
    let raw = String::from_utf8_lossy(&messages[0].raw_message);
    assert!(raw.contains("203.0.113.9"), "proxied IP recorded: {raw}");
    assert!(!raw.contains("127.0.0.1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtp_behind_a_proxy_refuses_bad_headers_with_502_and_closes() {
    let fixtures = Fixtures::new();
    let sink = Arc::new(MemorySink::new());
    let port = start_server_with(&fixtures, sink, SmtpListenerMode::Smtp, true).await;

    // A client field that is not an IP.
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tcp.write_all(b"PROXY TCP4 evil.example 10.0.70.5 40000 25\r\n")
        .await
        .unwrap();
    let received = read_until_closed(&mut tcp, std::time::Duration::from_secs(5)).await;
    assert_eq!(received, "502 Proxy Error\r\n");

    // A line past the v1 cap: refused at 107 bytes, not buffered forever.
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tcp.write_all(&[b'A'; 4096]).await.unwrap();
    let received = read_until_closed(&mut tcp, std::time::Duration::from_secs(5)).await;
    assert_eq!(received, "502 Proxy Error\r\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtp_behind_a_proxy_closes_a_connection_that_sends_no_header_in_time() {
    let fixtures = Fixtures::new();
    let sink = Arc::new(MemorySink::new());
    let port = start_server_full(
        &fixtures,
        sink,
        SmtpListenerMode::Smtp,
        true,
        Some(std::time::Duration::from_millis(200)),
    )
    .await;

    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let received = read_until_closed(&mut tcp, std::time::Duration::from_secs(5)).await;
    assert!(
        received.is_empty(),
        "no banner before the header: {received}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smtp_behind_a_proxy_answers_proxy_unknown_with_the_banner() {
    let fixtures = Fixtures::new();
    let sink = Arc::new(MemorySink::new());
    let port = start_server_with(&fixtures, sink, SmtpListenerMode::Smtp, true).await;

    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tcp.write_all(b"PROXY UNKNOWN\r\n").await.unwrap();
    let mut client = SmtpTestClient::new(tcp);
    assert!(client
        .read_line()
        .await
        .starts_with("220 postal.example.com ESMTP Camelmailer/"));
    assert_eq!(client.command("QUIT").await, vec!["221 Closing Connection"]);
}
