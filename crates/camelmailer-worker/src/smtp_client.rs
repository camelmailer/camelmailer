//! A minimal async SMTP client for outbound delivery — the port of
//! `app/lib/smtp_client.rb` / the `send_message` path of the SMTP sender.
//!
//! Supports opportunistic STARTTLS: when the remote server advertises it,
//! the connection is upgraded before MAIL FROM (honoring
//! `smtp.openssl_verify_mode`; `"none"` accepts any certificate). An
//! optional source IP binds the local socket for IP-pool-aware delivery.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};
use tokio_rustls::TlsConnector;

/// The outcome of a delivery attempt, classified like Postal's
/// Sent / SoftFail / HardFail statuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Sent { response: String, tls: bool },
    /// Retryable: connection problems and 4xx replies.
    SoftFail { response: String },
    /// Permanent: 5xx replies.
    HardFail { response: String },
}

/// How to treat the remote certificate during STARTTLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// Attempt STARTTLS, verifying the certificate against webpki roots.
    Verify,
    /// Attempt STARTTLS but accept any certificate (openssl_verify_mode=none).
    AcceptAny,
    /// Never attempt STARTTLS.
    Disabled,
}

impl TlsMode {
    pub fn from_verify_mode(verify_mode: &str, enable: bool) -> Self {
        if !enable {
            Self::Disabled
        } else if verify_mode.eq_ignore_ascii_case("none") {
            Self::AcceptAny
        } else {
            Self::Verify
        }
    }
}

#[derive(Debug, Clone)]
pub struct SendParams {
    pub host: String,
    pub port: u16,
    pub helo_hostname: String,
    pub mail_from: String,
    pub rcpt_to: String,
    pub timeout: Duration,
    pub tls_mode: TlsMode,
    /// Local address to bind for the outgoing connection (IP pool source).
    pub source_ip: Option<IpAddr>,
}

fn classify(code: u16, response: String, tls: bool) -> SendOutcome {
    match code {
        200..=399 => SendOutcome::Sent { response, tls },
        400..=499 => SendOutcome::SoftFail { response },
        _ => SendOutcome::HardFail { response },
    }
}

type Stream = Box<dyn AsyncStream>;

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

struct SmtpConnection {
    stream: Stream,
    timeout: Duration,
    buffer: Vec<u8>,
}

impl SmtpConnection {
    async fn read_reply(&mut self) -> std::io::Result<(u16, String)> {
        let mut full = String::new();
        loop {
            let line = self.read_line().await?;
            full.push_str(&line);
            full.push('\n');
            if line.len() < 4 || line.as_bytes().get(3) != Some(&b'-') {
                let code = line
                    .get(0..3)
                    .and_then(|c| c.parse::<u16>().ok())
                    .unwrap_or(0);
                return Ok((code, full.trim().to_string()));
            }
        }
    }

    async fn read_line(&mut self) -> std::io::Result<String> {
        loop {
            if let Some(position) = self.buffer.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=position).collect();
                let text = String::from_utf8_lossy(&line);
                return Ok(text.trim_end_matches(['\r', '\n']).to_string());
            }
            let mut chunk = [0u8; 2048];
            let read = tokio::time::timeout(self.timeout, self.stream.read(&mut chunk))
                .await
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "read timeout"))??;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ));
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    async fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.stream.write_all(format!("{line}\r\n").as_bytes()).await
    }

    async fn command(&mut self, line: &str) -> std::io::Result<(u16, String)> {
        self.send_line(line).await?;
        self.read_reply().await
    }
}

/// A rustls verifier that accepts any server certificate.
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

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

fn tls_connector(mode: TlsMode) -> TlsConnector {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = match mode {
        TlsMode::AcceptAny => rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
            .with_no_client_auth(),
        _ => {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
    };
    TlsConnector::from(Arc::new(config))
}

async fn connect(params: &SendParams) -> std::io::Result<TcpStream> {
    let address: SocketAddr = tokio::net::lookup_host((params.host.as_str(), params.port))
        .await?
        .next()
        .ok_or_else(|| std::io::Error::other(format!("no address for {}", params.host)))?;
    let connect_future = match params.source_ip {
        Some(source_ip) => {
            let socket = if source_ip.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            socket.bind(SocketAddr::new(source_ip, 0))?;
            socket.connect(address)
        }
        None => return tokio::time::timeout(params.timeout, TcpStream::connect(address))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"))?,
    };
    tokio::time::timeout(params.timeout, connect_future)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"))?
}

/// Deliver a raw message over SMTP. Any I/O error is a soft failure
/// (retryable); SMTP rejections are classified by status code.
pub async fn send_message(params: &SendParams, raw_message: &[u8]) -> SendOutcome {
    match try_send(params, raw_message).await {
        Ok(outcome) => outcome,
        Err(error) => SendOutcome::SoftFail {
            response: format!("connection error to {}:{}: {error}", params.host, params.port),
        },
    }
}

async fn try_send(params: &SendParams, raw_message: &[u8]) -> std::io::Result<SendOutcome> {
    let tcp = connect(params).await?;
    let mut connection = SmtpConnection {
        stream: Box::new(tcp),
        timeout: params.timeout,
        buffer: Vec::new(),
    };

    let (code, response) = connection.read_reply().await?;
    if code != 220 {
        return Ok(classify(code, response, false));
    }

    let mut tls = false;
    let (code, ehlo_response) = connection
        .command(&format!("EHLO {}", params.helo_hostname))
        .await?;
    if !(200..=299).contains(&code) {
        let (code, response) = connection
            .command(&format!("HELO {}", params.helo_hostname))
            .await?;
        if !(200..=299).contains(&code) {
            return Ok(classify(code, response, false));
        }
    } else if params.tls_mode != TlsMode::Disabled
        && ehlo_response.to_uppercase().contains("STARTTLS")
    {
        let (code, response) = connection.command("STARTTLS").await?;
        if (200..=299).contains(&code) {
            // upgrade the stream and re-EHLO
            connection = upgrade(connection, params).await?;
            tls = true;
            let (code, response) = connection
                .command(&format!("EHLO {}", params.helo_hostname))
                .await?;
            if !(200..=299).contains(&code) {
                return Ok(classify(code, response, tls));
            }
        } else {
            // STARTTLS refused mid-session: fall through in plaintext
            let _ = response;
        }
    }

    let (code, response) = connection
        .command(&format!("MAIL FROM:<{}>", params.mail_from))
        .await?;
    if !(200..=299).contains(&code) {
        return Ok(classify(code, response, tls));
    }

    let (code, response) = connection
        .command(&format!("RCPT TO:<{}>", params.rcpt_to))
        .await?;
    if !(200..=299).contains(&code) {
        return Ok(classify(code, response, tls));
    }

    let (code, response) = connection.command("DATA").await?;
    if code != 354 {
        return Ok(classify(code, response, tls));
    }

    let mut body = Vec::with_capacity(raw_message.len() + 64);
    let mut lines: Vec<&[u8]> = raw_message.split(|&b| b == b'\n').collect();
    if lines.last() == Some(&&b""[..]) {
        lines.pop();
    }
    for line in lines {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.first() == Some(&b'.') {
            body.push(b'.');
        }
        body.extend_from_slice(line);
        body.extend_from_slice(b"\r\n");
    }
    connection.stream.write_all(&body).await?;
    let (code, response) = connection.command(".").await?;
    let outcome = classify(code, response, tls);

    let _ = connection.send_line("QUIT").await;
    Ok(outcome)
}

async fn upgrade(
    connection: SmtpConnection,
    params: &SendParams,
) -> std::io::Result<SmtpConnection> {
    // The rustls stream must start from the raw TCP stream with no buffered
    // bytes; the SMTP server does not send anything between "220 Ready to
    // start TLS" and the handshake, so the buffer is empty here.
    let SmtpConnection {
        stream,
        timeout,
        buffer,
    } = connection;
    debug_assert!(buffer.is_empty());
    let connector = tls_connector(params.tls_mode);
    let server_name = rustls::pki_types::ServerName::try_from(params.host.clone())
        .map_err(|_| std::io::Error::other("invalid server name for TLS"))?;
    let tls_stream = connector.connect(server_name, stream).await?;
    Ok(SmtpConnection {
        stream: Box::new(tls_stream),
        timeout,
        buffer: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_mode_from_verify_mode() {
        assert_eq!(TlsMode::from_verify_mode("peer", true), TlsMode::Verify);
        assert_eq!(TlsMode::from_verify_mode("none", true), TlsMode::AcceptAny);
        assert_eq!(TlsMode::from_verify_mode("peer", false), TlsMode::Disabled);
    }
}
