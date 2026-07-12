//! Relay transport tests for the outbound SMTP client: mandatory STARTTLS
//! on the submission port (no plaintext fallback), implicit TLS (smtps)
//! and AUTH PLAIN — against in-process mock relays. No database needed.

use base64::Engine;
use camelmailer_worker::smtp_client::{
    send_message, ConnectionSecurity, SendOutcome, SendParams, SmtpAuth, TlsMode,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

fn self_signed_acceptor() -> TlsAcceptor {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certified = rcgen::generate_simple_self_signed(vec!["relay.example".into()]).unwrap();
    let cert = rustls::pki_types::CertificateDer::from(certified.cert.der().to_vec());
    let key =
        rustls::pki_types::PrivateKeyDer::try_from(certified.key_pair.serialize_der()).unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

/// What the mock relay speaks on the wire.
#[derive(Clone, Copy, PartialEq)]
enum RelayFlavor {
    /// Plaintext only; STARTTLS is not advertised.
    PlainOnly,
    /// Plaintext greeting, STARTTLS advertised and honored.
    StartTls,
    /// STARTTLS advertised and accepted (220), but the "TLS" that follows is
    /// garbage — the client's handshake fails. Models an MX with a broken /
    /// misconfigured STARTTLS. Plaintext delivery on a reconnect still works.
    StartTlsBroken,
    /// TLS from the first byte (smtps).
    Smtps,
}

impl RelayFlavor {
    fn advertises_starttls(self) -> bool {
        matches!(self, RelayFlavor::StartTls | RelayFlavor::StartTlsBroken)
    }
}

struct MockRelay {
    port: u16,
    lines: Arc<Mutex<Vec<String>>>,
}

/// Read one CRLF line without over-reading (so a TLS handshake can follow).
async fn read_line(stream: &mut Box<dyn AsyncStream>, buffer: &mut Vec<u8>) -> Option<String> {
    loop {
        if let Some(position) = buffer.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = buffer.drain(..=position).collect();
            line.pop();
            return Some(
                String::from_utf8_lossy(&line)
                    .trim_end_matches('\r')
                    .to_string(),
            );
        }
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

async fn mock_relay(flavor: RelayFlavor) -> MockRelay {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let captured = lines.clone();
    let acceptor = match flavor {
        RelayFlavor::PlainOnly => None,
        _ => Some(self_signed_acceptor()),
    };
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let captured = captured.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let mut stream: Box<dyn AsyncStream> = if flavor == RelayFlavor::Smtps {
                    let Ok(tls) = acceptor.as_ref().unwrap().accept(tcp).await else {
                        return;
                    };
                    Box::new(tls)
                } else {
                    Box::new(tcp)
                };
                let mut buffer = Vec::new();
                stream.write_all(b"220 mock relay\r\n").await.ok();
                let mut in_data = false;
                loop {
                    let Some(line) = read_line(&mut stream, &mut buffer).await else {
                        return;
                    };
                    captured.lock().unwrap().push(line.clone());
                    if in_data {
                        if line == "." {
                            in_data = false;
                            stream.write_all(b"250 Accepted\r\n").await.ok();
                        }
                        continue;
                    }
                    let upper = line.to_ascii_uppercase();
                    if upper.starts_with("EHLO") {
                        let reply: &[u8] = if flavor.advertises_starttls() {
                            b"250-mock\r\n250-STARTTLS\r\n250 OK\r\n"
                        } else {
                            b"250-mock\r\n250 OK\r\n"
                        };
                        stream.write_all(reply).await.ok();
                    } else if upper.starts_with("STARTTLS") && flavor == RelayFlavor::StartTls {
                        stream.write_all(b"220 Ready to start TLS\r\n").await.ok();
                        // The client sends nothing between our 220 and its
                        // handshake, so the read buffer is empty here.
                        assert!(buffer.is_empty());
                        let Ok(tls) = acceptor.as_ref().unwrap().accept(stream).await else {
                            return;
                        };
                        stream = Box::new(tls);
                    } else if upper.starts_with("STARTTLS") && flavor == RelayFlavor::StartTlsBroken
                    {
                        // Accept STARTTLS, then drop the connection instead of
                        // performing the handshake, so the client's TLS layer
                        // fails on EOF. It should then reconnect and deliver in
                        // plaintext.
                        stream.write_all(b"220 Ready to start TLS\r\n").await.ok();
                        return;
                    } else if upper.starts_with("AUTH PLAIN") {
                        stream.write_all(b"235 Authenticated\r\n").await.ok();
                    } else if upper.starts_with("DATA") {
                        in_data = true;
                        stream.write_all(b"354 Go ahead\r\n").await.ok();
                    } else if upper.starts_with("QUIT") {
                        stream.write_all(b"221 Bye\r\n").await.ok();
                        return;
                    } else {
                        stream.write_all(b"250 OK\r\n").await.ok();
                    }
                }
            });
        }
    });
    MockRelay { port, lines }
}

fn params(port: u16, security: ConnectionSecurity, auth: Option<SmtpAuth>) -> SendParams {
    SendParams {
        host: "127.0.0.1".into(),
        port,
        helo_hostname: "sender.example".into(),
        mail_from: "sender@org.example".into(),
        rcpt_to: "user@elsewhere.example".into(),
        timeout: Duration::from_secs(5),
        // Self-signed mock certificates: verification must be off, but the
        // *security* requirement below still forces TLS where demanded.
        tls_mode: TlsMode::AcceptAny,
        security,
        auth,
        source_ip: None,
    }
}

const MESSAGE: &[u8] = b"Subject: Relay\r\n\r\nHello.\r\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mandatory_starttls_soft_fails_instead_of_plaintext_fallback() {
    let relay = mock_relay(RelayFlavor::PlainOnly).await;
    let outcome = send_message(
        &params(relay.port, ConnectionSecurity::RequireStartTls, None),
        MESSAGE,
    )
    .await;
    match outcome {
        SendOutcome::SoftFail { response } => {
            assert!(response.contains("STARTTLS"), "got: {response}")
        }
        other => panic!("expected SoftFail, got {other:?}"),
    }
    // The envelope must never have been sent in plaintext.
    let seen = relay.lines.lock().unwrap().clone();
    assert!(!seen.iter().any(|l| l.starts_with("MAIL FROM")), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mandatory_starttls_upgrades_and_authenticates() {
    let relay = mock_relay(RelayFlavor::StartTls).await;
    let auth = SmtpAuth {
        username: "mailer".into(),
        password: "s3cret".into(),
    };
    let outcome = send_message(
        &params(relay.port, ConnectionSecurity::RequireStartTls, Some(auth)),
        MESSAGE,
    )
    .await;
    assert!(
        matches!(outcome, SendOutcome::Sent { tls: true, .. }),
        "got {outcome:?}"
    );

    let seen = relay.lines.lock().unwrap().clone();
    let expected_token = base64::engine::general_purpose::STANDARD.encode("\0mailer\0s3cret");
    let auth_index = seen
        .iter()
        .position(|l| l == &format!("AUTH PLAIN {expected_token}"))
        .expect("AUTH PLAIN must be sent");
    let starttls_index = seen.iter().position(|l| l == "STARTTLS").unwrap();
    let mail_from_index = seen
        .iter()
        .position(|l| l.starts_with("MAIL FROM"))
        .unwrap();
    // AUTH happens after the TLS handshake, the envelope after AUTH.
    assert!(starttls_index < auth_index && auth_index < mail_from_index);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn implicit_tls_relay_delivers_with_auth() {
    let relay = mock_relay(RelayFlavor::Smtps).await;
    let auth = SmtpAuth {
        username: "mailer".into(),
        password: "s3cret".into(),
    };
    let outcome = send_message(
        &params(relay.port, ConnectionSecurity::ImplicitTls, Some(auth)),
        MESSAGE,
    )
    .await;
    assert!(
        matches!(outcome, SendOutcome::Sent { tls: true, .. }),
        "got {outcome:?}"
    );

    let seen = relay.lines.lock().unwrap().clone();
    let expected_token = base64::engine::general_purpose::STANDARD.encode("\0mailer\0s3cret");
    assert!(seen.contains(&format!("AUTH PLAIN {expected_token}")));
    assert!(seen.iter().any(|l| l.starts_with("MAIL FROM")));
    // No STARTTLS on an implicit-TLS connection.
    assert!(!seen.iter().any(|l| l == "STARTTLS"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opportunistic_relays_still_fall_back_to_plaintext() {
    let relay = mock_relay(RelayFlavor::PlainOnly).await;
    let outcome = send_message(
        &params(relay.port, ConnectionSecurity::Opportunistic, None),
        MESSAGE,
    )
    .await;
    assert!(
        matches!(outcome, SendOutcome::Sent { tls: false, .. }),
        "got {outcome:?}"
    );
}

/// Like [`params`] but with an explicit certificate-verification mode, so the
/// direct-MX (AcceptAny) and relay (Verify) semantics can be exercised apart.
fn params_verify(
    port: u16,
    security: ConnectionSecurity,
    tls_mode: TlsMode,
    auth: Option<SmtpAuth>,
) -> SendParams {
    SendParams {
        tls_mode,
        ..params(port, security, auth)
    }
}

// Regression ("Outlook UnknownIssuer"): a direct-MX endpoint delivers over
// STARTTLS to an MX presenting a self-signed / non-webpki certificate, with
// verification OFF (AcceptAny) as `Endpoint::mx` now sets. Before the fix the
// default `openssl_verify_mode = peer` forced Verify here and every such
// handshake soft-failed with `invalid peer certificate: UnknownIssuer`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_mx_delivers_over_tls_without_verifying_certificate() {
    let mx = mock_relay(RelayFlavor::StartTls).await;
    let outcome = send_message(
        &params_verify(
            mx.port,
            ConnectionSecurity::Opportunistic,
            TlsMode::AcceptAny,
            None,
        ),
        MESSAGE,
    )
    .await;
    // Encrypted delivery: marked sent-with-ssl, not a UnknownIssuer soft fail.
    assert!(
        matches!(outcome, SendOutcome::Sent { tls: true, .. }),
        "got {outcome:?}"
    );
    let seen = mx.lines.lock().unwrap().clone();
    assert!(seen.iter().any(|l| l == "STARTTLS"));
    assert!(seen.iter().any(|l| l.starts_with("MAIL FROM")));
}

// Opportunistic (direct-MX) delivery whose STARTTLS handshake fails must fall
// back to a fresh plaintext connection rather than soft-failing forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_mx_falls_back_to_plaintext_when_tls_handshake_fails() {
    let mx = mock_relay(RelayFlavor::StartTlsBroken).await;
    let outcome = send_message(
        &params_verify(
            mx.port,
            ConnectionSecurity::Opportunistic,
            TlsMode::AcceptAny,
            None,
        ),
        MESSAGE,
    )
    .await;
    // Delivered, but in plaintext (the handshake could not complete).
    assert!(
        matches!(outcome, SendOutcome::Sent { tls: false, .. }),
        "got {outcome:?}"
    );
    let seen = mx.lines.lock().unwrap().clone();
    // STARTTLS was attempted on the first connection, then the envelope was
    // delivered (on the plaintext reconnect).
    assert!(seen.iter().any(|l| l == "STARTTLS"));
    assert!(seen.iter().any(|l| l.starts_with("MAIL FROM")));
}

// Certificate verification still protects a *configured relay*: mandatory
// STARTTLS with `openssl_verify_mode = peer` (Verify) against an untrusted
// cert must fail — and never silently fall back to plaintext — whereas `none`
// (AcceptAny) succeeds over TLS. This is the smarthost identity guarantee
// that direct-MX delivery deliberately does not (and cannot) demand.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_verify_rejects_untrusted_cert_but_accept_any_succeeds() {
    let relay = mock_relay(RelayFlavor::StartTls).await;
    let rejected = send_message(
        &params_verify(
            relay.port,
            ConnectionSecurity::RequireStartTls,
            TlsMode::Verify,
            None,
        ),
        MESSAGE,
    )
    .await;
    assert!(
        matches!(rejected, SendOutcome::SoftFail { .. }),
        "Verify against an untrusted relay cert must fail, got {rejected:?}"
    );
    // No plaintext fallback for a mandatory-STARTTLS relay.
    let seen = relay.lines.lock().unwrap().clone();
    assert!(!seen.iter().any(|l| l.starts_with("MAIL FROM")), "{seen:?}");

    let relay = mock_relay(RelayFlavor::StartTls).await;
    let accepted = send_message(
        &params_verify(
            relay.port,
            ConnectionSecurity::RequireStartTls,
            TlsMode::AcceptAny,
            None,
        ),
        MESSAGE,
    )
    .await;
    assert!(
        matches!(accepted, SendOutcome::Sent { tls: true, .. }),
        "AcceptAny must deliver over TLS, got {accepted:?}"
    );
}
