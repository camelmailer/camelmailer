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
    let certified = rcgen::generate_simple_self_signed(vec!["postal.example.com".into()]).unwrap();
    let cert_path = directory.join("smtp.cert");
    let key_path = directory.join("smtp.key");
    std::fs::write(&cert_path, certified.cert.pem()).unwrap();
    std::fs::write(&key_path, certified.key_pair.serialize_pem()).unwrap();
    (
        cert_path.to_string_lossy().into_owned(),
        key_path.to_string_lossy().into_owned(),
    )
}

/// A rustls client verifier that accepts any certificate (tests only).
#[derive(Debug)]
struct AcceptAnyCert(rustls::crypto::CryptoProvider);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
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
            &self.0.signature_verification_algorithms,
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
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
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

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = SmtpServer::new(config, fixtures.store(), sink);
    tokio::spawn(async move {
        server.serve_listeners(vec![(listener, mode)]).await.ok();
    });
    port
}

fn tls_connector() -> tokio_rustls::TlsConnector {
    let provider = rustls::crypto::ring::default_provider();
    let _ = provider.clone().install_default();
    let client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(
            rustls::crypto::ring::default_provider(),
        )))
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(client_config))
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
    assert!(banner.starts_with("220 postal.example.com ESMTP CamelMailer/"));

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
    assert!(banner.starts_with("220 postal.example.com ESMTP CamelMailer/"));

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
