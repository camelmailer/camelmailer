//! A minimal async SMTP client for outbound delivery — the port of
//! `app/lib/smtp_client.rb` / the `send_message` path of the SMTP sender.
//!
//! Supports opportunistic STARTTLS: when the remote server advertises it,
//! the connection is upgraded before MAIL FROM. For **direct-MX** delivery
//! the certificate is deliberately *not* verified (`TlsMode::AcceptAny`) —
//! this is opportunistic TLS in the MTA sense (`smtp_tls_security_level =
//! may`): encrypt when offered, but never require a trust chain a foreign MX
//! cannot provide. If the STARTTLS handshake fails, delivery falls back to a
//! fresh plaintext connection instead of stalling forever on
//! `invalid peer certificate: UnknownIssuer` (the Outlook/Microsoft bug).
//! Certificate verification (`smtp.openssl_verify_mode`) only governs
//! configured relays, whose identity is known.
//!
//! Relay endpoints can additionally demand more than opportunism (see
//! [`ConnectionSecurity`]): mandatory STARTTLS (submission, port 587 —
//! failing instead of falling back to plaintext) or implicit TLS from the
//! first byte (`smtps://`, port 465), and can authenticate with AUTH PLAIN
//! once the connection is at its negotiated security level.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};
use tokio_rustls::TlsConnector;

pub use camelmailer_config::AddressFamily;

/// The outcome of a delivery attempt, classified like Postal's
/// Sent / SoftFail / HardFail statuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Sent {
        response: String,
        tls: bool,
    },
    /// Retryable: connection problems and 4xx replies.
    SoftFail {
        response: String,
    },
    /// Permanent: 5xx replies.
    HardFail {
        response: String,
    },
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

/// The transport security an endpoint requires. `tls_mode` controls *how*
/// certificates are verified; this controls *whether* TLS is optional,
/// mandatory-by-upgrade, or implicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionSecurity {
    /// Plaintext, upgraded via STARTTLS when the remote offers it (the
    /// default for MX delivery and `smtp://` relays on port 25).
    Opportunistic,
    /// STARTTLS is mandatory: a soft failure instead of a plaintext
    /// fallback when the remote does not offer or refuses it
    /// (`smtp://` relays on the submission port 587).
    RequireStartTls,
    /// TLS from the first byte, before any SMTP exchange
    /// (`smtps://` relays, classically port 465).
    ImplicitTls,
}

/// AUTH PLAIN credentials for a relay, sent once the connection has reached
/// its negotiated security level (i.e. after the TLS handshake when there
/// is one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpAuth {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct SendParams {
    pub host: String,
    pub port: u16,
    pub helo_hostname: String,
    pub mail_from: String,
    pub rcpt_to: String,
    /// How long to wait for the TCP connection, per remote address
    /// (`smtp_client.open_timeout`).
    pub timeout: Duration,
    /// How long to wait for the greeting, a TLS handshake and each reply
    /// (`smtp_client.read_timeout`). RFC 5321 4.5.3.2 asks for five minutes:
    /// receivers such as Hornetsecurity hold the greeting back on purpose
    /// for a sender they do not know yet.
    pub read_timeout: Duration,
    /// How long to wait while the message body is sent and for the reply to
    /// its final dot (`smtp_client.data_timeout`, RFC 5321: ten minutes).
    pub data_timeout: Duration,
    pub tls_mode: TlsMode,
    pub security: ConnectionSecurity,
    /// Relay credentials (AUTH PLAIN), if any.
    pub auth: Option<SmtpAuth>,
    /// Where the connection comes from: the address families to use and,
    /// for an IP pool, the local addresses to bind.
    pub source: OutboundSource,
}

/// Where an outgoing connection comes from.
///
/// Without an IP pool ([`OutboundSource::system`]) the operating system
/// picks the local address and `family` alone decides which remote
/// addresses are tried, in which order. With a pool
/// ([`OutboundSource::pool`]) the connection binds the pool address of the
/// remote address's family, and a family the pool address has no local
/// address for is never tried: an IPv4 source cannot reach an IPv6 remote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutboundSource {
    pub family: AddressFamily,
    /// The IPv4 address to bind, for an IP pool.
    pub ipv4: Option<Ipv4Addr>,
    /// The IPv6 address to bind, for an IP pool.
    pub ipv6: Option<Ipv6Addr>,
    /// Only families with a bound address are tried (an IP pool).
    pub pinned: bool,
}

impl OutboundSource {
    /// The operating system picks the local address.
    pub fn system(family: AddressFamily) -> Self {
        Self {
            family,
            ipv4: None,
            ipv6: None,
            pinned: false,
        }
    }

    /// Bind the given pool addresses; families without one are not tried.
    pub fn pool(family: AddressFamily, ipv4: Option<Ipv4Addr>, ipv6: Option<Ipv6Addr>) -> Self {
        Self {
            family,
            ipv4,
            ipv6,
            pinned: true,
        }
    }

    /// Bind one address, the way an IP pool without an address family of
    /// its own sends: from its IPv4 address, to IPv4 remotes only.
    pub fn bound(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::pool(AddressFamily::Ipv4Only, Some(v4), None),
            IpAddr::V6(v6) => Self::pool(AddressFamily::Ipv6Only, None, Some(v6)),
        }
    }

    fn uses_ipv4(&self) -> bool {
        self.family.allows_ipv4() && (!self.pinned || self.ipv4.is_some())
    }

    fn uses_ipv6(&self) -> bool {
        self.family.allows_ipv6() && (!self.pinned || self.ipv6.is_some())
    }

    /// The local address to bind for a connection to `remote`, if any.
    fn local_for(&self, remote: &SocketAddr) -> Option<IpAddr> {
        if remote.is_ipv4() {
            self.ipv4.map(IpAddr::V4)
        } else {
            self.ipv6.map(IpAddr::V6)
        }
    }
}

fn classify(code: u16, response: String, tls: bool) -> SendOutcome {
    match code {
        200..=399 => SendOutcome::Sent { response, tls },
        400..=499 => SendOutcome::SoftFail { response },
        _ => SendOutcome::HardFail { response },
    }
}

type Stream = Box<dyn AsyncStream>;

/// Run `future` for at most `limit`; running out is an `io::ErrorKind::TimedOut`
/// named after `what`, which the delivery log shows.
async fn within<T>(
    limit: Duration,
    what: &str,
    future: impl std::future::Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    tokio::time::timeout(limit, future)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, format!("{what} timeout")))?
}

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
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("read timeout after {}s", self.timeout.as_secs()),
                    )
                })??;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ));
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    /// Writes a command line and flushes it. A TLS stream holds encrypted
    /// records back until it is flushed, so without the flush a command can
    /// sit in the client while both sides wait for each other.
    async fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.stream
            .write_all(format!("{line}\r\n").as_bytes())
            .await?;
        self.stream.flush().await
    }

    /// Sends a message body after a 354: line endings made CRLF, lines
    /// starting with a dot doubled (RFC 5321 4.5.2), all of it flushed, then
    /// the final dot, whose reply is waited for with `data_timeout`. Without
    /// the flush, a TLS stream could keep the tail of a large body and the
    /// final dot buffered once the socket had pushed back; a receiver that
    /// takes its data slowly then waited for the end of the message until
    /// its own timeout (`421 4.4.2 timeout exceeded`).
    async fn send_data(
        &mut self,
        raw_message: &[u8],
        data_timeout: Duration,
        read_timeout: Duration,
    ) -> std::io::Result<(u16, String)> {
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
        // The body and the reply to its final dot get the data timeout: the
        // receiver may scan the whole message before it answers.
        within(data_timeout, "data", async {
            self.stream.write_all(&body).await?;
            self.stream.flush().await
        })
        .await?;
        self.timeout = data_timeout;
        let reply = self.command(".").await;
        self.timeout = read_timeout;
        reply
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

/// The remote addresses to try, in order (see [`OutboundSource`]).
///
/// The resolver's order is kept within each family. Across families:
///
/// - `dual_stack` starts with IPv6 and alternates with IPv4 (RFC 8305
///   section 4), so a host whose IPv6 path is broken loses one connection
///   timeout before it reaches IPv4, not one per AAAA record;
/// - `prefer_ipv4` tries every IPv4 address before the first IPv6 one;
/// - `ipv4_only` and `ipv6_only` drop the other family.
///
/// A pinned source (an IP pool) further drops each family it has no local
/// address for.
fn remote_addresses(
    addresses: impl Iterator<Item = SocketAddr>,
    source: &OutboundSource,
    host: &str,
) -> std::io::Result<Vec<SocketAddr>> {
    let addresses: Vec<SocketAddr> = addresses.collect();
    if addresses.is_empty() {
        return Err(std::io::Error::other(format!("no address for {host}")));
    }
    let (use_v4, use_v6) = (source.uses_ipv4(), source.uses_ipv6());
    if !use_v4 && !use_v6 {
        let family = if source.family.allows_ipv6() {
            "IPv6"
        } else {
            "IPv4"
        };
        return Err(std::io::Error::other(format!(
            "the IP pool address has no {family} address to send from \
             (address family {})",
            source.family
        )));
    }
    let (v4, v6): (Vec<_>, Vec<_>) = addresses.into_iter().partition(SocketAddr::is_ipv4);
    let v4 = if use_v4 { v4 } else { Vec::new() };
    let v6 = if use_v6 { v6 } else { Vec::new() };
    let candidates: Vec<SocketAddr> = match source.family {
        AddressFamily::PreferIpv4 => v4.into_iter().chain(v6).collect(),
        _ => interleave(v6, v4),
    };
    if candidates.is_empty() {
        let family = if use_v4 { "IPv4" } else { "IPv6" };
        let reason = if source.pinned && source.family.allows_ipv4() && source.family.allows_ipv6()
        {
            "required by the bound source address".to_string()
        } else {
            format!("address family {}", source.family)
        };
        return Err(std::io::Error::other(format!(
            "no {family} address for {host} ({reason})"
        )));
    }
    Ok(candidates)
}

/// `first[0], second[0], first[1], second[1], …`, then the rest of the
/// longer one.
fn interleave(first: Vec<SocketAddr>, second: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut out = Vec::with_capacity(first.len() + second.len());
    let (mut first, mut second) = (first.into_iter(), second.into_iter());
    loop {
        match (first.next(), second.next()) {
            (None, None) => return out,
            (a, b) => out.extend(a.into_iter().chain(b)),
        }
    }
}

/// Connect to one of the host's addresses: each candidate in turn (see
/// [`remote_addresses`]), so one unreachable address of a host with several
/// does not fail the attempt. The last connection error is returned when
/// none answers.
async fn connect(params: &SendParams) -> std::io::Result<TcpStream> {
    let addresses = tokio::net::lookup_host((params.host.as_str(), params.port)).await?;
    let candidates = remote_addresses(addresses, &params.source, &params.host)?;
    let mut last_error = None;
    for address in candidates {
        match connect_to(address, params).await {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                tracing::debug!(%address, host = %params.host, %error, "connect failed, trying the next address");
                last_error = Some(error);
            }
        }
    }
    Err(last_error
        .unwrap_or_else(|| std::io::Error::other(format!("no address for {}", params.host))))
}

async fn connect_to(address: SocketAddr, params: &SendParams) -> std::io::Result<TcpStream> {
    let connect = async {
        match params.source.local_for(&address) {
            Some(source_ip) => {
                let socket = if source_ip.is_ipv4() {
                    TcpSocket::new_v4()?
                } else {
                    TcpSocket::new_v6()?
                };
                socket.bind(SocketAddr::new(source_ip, 0))?;
                socket.connect(address).await
            }
            None => TcpStream::connect(address).await,
        }
    };
    within(params.timeout, "connect", connect).await
}

/// Outcome of a single connection attempt: either a fully classified
/// delivery result, or the signal that an *opportunistic* STARTTLS handshake
/// failed so the caller should retry the delivery in plaintext.
enum Attempt {
    Complete(SendOutcome),
    OpportunisticStartTlsHandshakeFailed,
}

/// Deliver a raw message over SMTP. Any I/O error is a soft failure
/// (retryable); SMTP rejections are classified by status code.
///
/// Opportunistic STARTTLS (direct-MX delivery and `smtp://…:25` relays) is
/// best-effort: if the STARTTLS handshake fails we retry once on a *fresh
/// plaintext connection* rather than losing the mail to a permanent soft
/// fail. This mirrors a normal MTA (`smtp_tls_security_level = may`) and is
/// what keeps mail to Microsoft/Outlook and other MX hosts flowing.
pub async fn send_message(params: &SendParams, raw_message: &[u8]) -> SendOutcome {
    match try_send(params, raw_message, true).await {
        Ok(Attempt::Complete(outcome)) => outcome,
        Ok(Attempt::OpportunisticStartTlsHandshakeFailed) => {
            // The STARTTLS handshake failed on an opportunistic connection.
            // That socket is mid-TLS and cannot be reused, so we open a new
            // connection and deliver in plaintext. Reconnecting without
            // STARTTLS is the most robust and least invasive fallback (the
            // path most MTAs take) versus trying to un-wind TLS in place.
            match try_send(params, raw_message, false).await {
                Ok(Attempt::Complete(outcome)) => outcome,
                // A plaintext attempt never enters the STARTTLS path.
                Ok(Attempt::OpportunisticStartTlsHandshakeFailed) => unreachable!(),
                Err(error) => SendOutcome::SoftFail {
                    response: format!(
                        "connection error to {}:{}: {error}",
                        params.host, params.port
                    ),
                },
            }
        }
        Err(error) => SendOutcome::SoftFail {
            response: format!(
                "connection error to {}:{}: {error}",
                params.host, params.port
            ),
        },
    }
}

/// Run one connection attempt. `allow_starttls == false` forces a plaintext
/// session (the reconnect after an opportunistic handshake failure).
async fn try_send(
    params: &SendParams,
    raw_message: &[u8],
    allow_starttls: bool,
) -> std::io::Result<Attempt> {
    let tcp = connect(params).await?;
    let mut tls = false;
    let stream: Stream = if params.security == ConnectionSecurity::ImplicitTls {
        // smtps: the TLS handshake happens before any SMTP byte; the banner
        // arrives over the encrypted stream.
        let connector = tls_connector(effective_tls_mode(params));
        let server_name = rustls::pki_types::ServerName::try_from(params.host.clone())
            .map_err(|_| std::io::Error::other("invalid server name for TLS"))?;
        tls = true;
        Box::new(
            within(
                params.read_timeout,
                "TLS handshake",
                connector.connect(server_name, tcp),
            )
            .await?,
        )
    } else {
        Box::new(tcp)
    };
    let mut connection = SmtpConnection {
        stream,
        timeout: params.read_timeout,
        buffer: Vec::new(),
    };

    let (code, response) = connection.read_reply().await?;
    if code != 220 {
        return Ok(Attempt::Complete(classify(code, response, tls)));
    }

    let (code, ehlo_response) = connection
        .command(&format!("EHLO {}", params.helo_hostname))
        .await?;
    if !(200..=299).contains(&code) {
        let (code, response) = connection
            .command(&format!("HELO {}", params.helo_hostname))
            .await?;
        if !(200..=299).contains(&code) {
            return Ok(Attempt::Complete(classify(code, response, tls)));
        }
    } else if !tls
        && allow_starttls
        && (params.tls_mode != TlsMode::Disabled
            || params.security == ConnectionSecurity::RequireStartTls)
        && ehlo_response.to_uppercase().contains("STARTTLS")
    {
        let (code, response) = connection.command("STARTTLS").await?;
        if (200..=299).contains(&code) {
            // upgrade the stream and re-EHLO
            connection = match upgrade(connection, params).await {
                Ok(upgraded) => upgraded,
                Err(error) => {
                    // Handshake failed after the server accepted STARTTLS.
                    // Opportunistic delivery retries in plaintext (signalled
                    // to the caller); a mandatory-TLS relay must not, so it
                    // soft-fails via the propagated error.
                    if params.security == ConnectionSecurity::Opportunistic {
                        return Ok(Attempt::OpportunisticStartTlsHandshakeFailed);
                    }
                    return Err(error);
                }
            };
            tls = true;
            let (code, response) = connection
                .command(&format!("EHLO {}", params.helo_hostname))
                .await?;
            if !(200..=299).contains(&code) {
                return Ok(Attempt::Complete(classify(code, response, tls)));
            }
        } else if params.security == ConnectionSecurity::RequireStartTls {
            return Ok(Attempt::Complete(SendOutcome::SoftFail {
                response: format!(
                    "relay {}:{} refused mandatory STARTTLS: {response}",
                    params.host, params.port
                ),
            }));
        } else {
            // STARTTLS refused mid-session: fall through in plaintext
            let _ = response;
        }
    }

    // Submission relays (:587) must never fall back to plaintext: fail
    // (retryably) instead when the remote did not offer STARTTLS.
    if params.security == ConnectionSecurity::RequireStartTls && !tls {
        return Ok(Attempt::Complete(SendOutcome::SoftFail {
            response: format!(
                "relay {}:{} does not offer STARTTLS, which is required for smtp:// relays on the submission port",
                params.host, params.port
            ),
        }));
    }

    if let Some(auth) = &params.auth {
        use base64::Engine;
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("\0{}\0{}", auth.username, auth.password));
        let (code, response) = connection.command(&format!("AUTH PLAIN {token}")).await?;
        if code != 235 {
            return Ok(Attempt::Complete(classify(code, response, tls)));
        }
    }

    let (code, response) = connection
        .command(&format!("MAIL FROM:<{}>", params.mail_from))
        .await?;
    if !(200..=299).contains(&code) {
        return Ok(Attempt::Complete(classify(code, response, tls)));
    }

    let (code, response) = connection
        .command(&format!("RCPT TO:<{}>", params.rcpt_to))
        .await?;
    if !(200..=299).contains(&code) {
        return Ok(Attempt::Complete(classify(code, response, tls)));
    }

    let (code, response) = connection.command("DATA").await?;
    if code != 354 {
        return Ok(Attempt::Complete(classify(code, response, tls)));
    }

    let (code, response) = connection
        .send_data(raw_message, params.data_timeout, params.read_timeout)
        .await?;
    let outcome = classify(code, response, tls);

    let _ = connection.send_line("QUIT").await;
    Ok(Attempt::Complete(outcome))
}

/// The certificate-verification mode actually used for a connection: when
/// the endpoint *requires* TLS (mandatory STARTTLS or smtps) the configured
/// `Disabled` cannot apply — verification falls back to `Verify`.
fn effective_tls_mode(params: &SendParams) -> TlsMode {
    match (params.tls_mode, params.security) {
        (TlsMode::Disabled, ConnectionSecurity::RequireStartTls)
        | (TlsMode::Disabled, ConnectionSecurity::ImplicitTls) => TlsMode::Verify,
        (mode, _) => mode,
    }
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
    let connector = tls_connector(effective_tls_mode(params));
    let server_name = rustls::pki_types::ServerName::try_from(params.host.clone())
        .map_err(|_| std::io::Error::other("invalid server name for TLS"))?;
    let tls_stream = within(
        timeout,
        "TLS handshake",
        connector.connect(server_name, stream),
    )
    .await?;
    Ok(SmtpConnection {
        stream: Box::new(tls_stream),
        timeout,
        buffer: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that, like a TLS stream, sends what it was given only when
    /// it is flushed.
    struct SendsOnFlush<S> {
        inner: S,
        held: Vec<u8>,
    }

    impl<S: AsyncRead + Unpin> AsyncRead for SendsOnFlush<S> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl<S: AsyncWrite + Unpin> AsyncWrite for SendsOnFlush<S> {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.held.extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let this = &mut *self;
            while !this.held.is_empty() {
                let n = std::task::ready!(
                    std::pin::Pin::new(&mut this.inner).poll_write(cx, &this.held)
                )?;
                this.held.drain(..n);
            }
            std::pin::Pin::new(&mut this.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn a_large_body_and_its_final_dot_reach_a_receiver_behind_a_buffering_stream() {
        // A small pipe pushes back like a full socket under a slow receiver.
        let (client, mut server) = tokio::io::duplex(4096);
        let mut connection = SmtpConnection {
            stream: Box::new(SendsOnFlush {
                inner: client,
                held: Vec::new(),
            }),
            timeout: Duration::from_secs(2),
            buffer: Vec::new(),
        };

        let receiver = tokio::spawn(async move {
            let mut seen = Vec::new();
            let mut chunk = [0u8; 1024];
            while !seen.ends_with(b"\r\n.\r\n") {
                let n = server.read(&mut chunk).await.unwrap();
                assert!(n > 0, "client closed before the final dot");
                seen.extend_from_slice(&chunk[..n]);
            }
            server.write_all(b"250 2.0.0 Ok: queued\r\n").await.unwrap();
            seen
        });

        // About 300 KB, as an invoice with its PDF and XML, with bare LF
        // endings and a line that starts with a dot.
        let mut raw = b"Subject: Ihre Rechnung\n\n.dot line\n".to_vec();
        for _ in 0..4000 {
            raw.extend_from_slice(
                b"QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5\n",
            );
        }

        let (code, _) = tokio::time::timeout(
            Duration::from_secs(5),
            connection.send_data(&raw, Duration::from_secs(2), Duration::from_secs(2)),
        )
        .await
        .expect("the final dot never reached the receiver")
        .unwrap();
        assert_eq!(code, 250);

        let seen = receiver.await.unwrap();
        assert!(seen.starts_with(b"Subject: Ihre Rechnung\r\n\r\n..dot line\r\n"));
        assert!(
            !seen.windows(2).any(|w| w[1] == b'\n' && w[0] != b'\r'),
            "bare LF sent"
        );
    }

    #[tokio::test]
    async fn a_command_reaches_the_receiver_behind_a_buffering_stream() {
        let (client, mut server) = tokio::io::duplex(4096);
        let mut connection = SmtpConnection {
            stream: Box::new(SendsOnFlush {
                inner: client,
                held: Vec::new(),
            }),
            timeout: Duration::from_secs(2),
            buffer: Vec::new(),
        };
        let receiver = tokio::spawn(async move {
            let mut line = [0u8; 64];
            let n = server.read(&mut line).await.unwrap();
            server.write_all(b"250 Ok\r\n").await.unwrap();
            line[..n].to_vec()
        });

        let (code, _) = connection.command("NOOP").await.unwrap();
        assert_eq!(code, 250);
        assert_eq!(receiver.await.unwrap(), b"NOOP\r\n");
    }

    #[test]
    fn tls_mode_from_verify_mode() {
        assert_eq!(TlsMode::from_verify_mode("peer", true), TlsMode::Verify);
        assert_eq!(TlsMode::from_verify_mode("none", true), TlsMode::AcceptAny);
        assert_eq!(TlsMode::from_verify_mode("peer", false), TlsMode::Disabled);
    }

    fn v4(last: u8) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, last)), 25)
    }

    fn v6(last: u16) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, last)),
            25,
        )
    }

    fn system(family: AddressFamily) -> OutboundSource {
        OutboundSource::system(family)
    }

    #[test]
    fn a_bound_ipv4_source_keeps_only_ipv4_remotes() {
        let source = OutboundSource::bound("198.51.100.5".parse().unwrap());
        // an IPv4 source skips a leading AAAA candidate (MX hosts often
        // resolve IPv6-first) and keeps only the A record
        let picked = remote_addresses([v6(1), v4(10)].into_iter(), &source, "mx.example").unwrap();
        assert_eq!(picked, vec![v4(10)]);
        assert_eq!(
            source.local_for(&v4(10)),
            Some("198.51.100.5".parse().unwrap())
        );

        // no candidate in the source's family is a clear error, not EAFNOSUPPORT
        let error = remote_addresses([v6(1)].into_iter(), &source, "mx.example")
            .expect_err("family mismatch must error");
        assert!(error.to_string().contains("no IPv4 address"), "{error}");
    }

    #[test]
    fn a_bound_ipv6_source_keeps_only_ipv6_remotes() {
        let source = OutboundSource::bound("2001:db8::5".parse().unwrap());
        let picked = remote_addresses([v4(10), v6(1)].into_iter(), &source, "mx.example").unwrap();
        assert_eq!(picked, vec![v6(1)]);
        let error = remote_addresses([v4(10)].into_iter(), &source, "mx.example")
            .expect_err("family mismatch must error");
        assert!(error.to_string().contains("no IPv6 address"), "{error}");
    }

    #[test]
    fn an_empty_resolution_is_no_address() {
        let error = remote_addresses(
            std::iter::empty(),
            &system(AddressFamily::DualStack),
            "mx.example",
        )
        .expect_err("empty resolution must error");
        assert!(error.to_string().contains("no address"));
    }

    #[test]
    fn dual_stack_starts_with_ipv6_and_alternates_families() {
        let source = system(AddressFamily::DualStack);
        let picked = remote_addresses(
            [v4(10), v6(1), v4(11), v6(2), v6(3)].into_iter(),
            &source,
            "mx.example",
        )
        .unwrap();
        assert_eq!(picked, vec![v6(1), v4(10), v6(2), v4(11), v6(3)]);

        // a single-stack remote is reached either way
        let picked = remote_addresses([v4(10), v4(11)].into_iter(), &source, "mx.example").unwrap();
        assert_eq!(picked, vec![v4(10), v4(11)]);
        let picked = remote_addresses([v6(1)].into_iter(), &source, "mx.example").unwrap();
        assert_eq!(picked, vec![v6(1)]);
        assert_eq!(source.local_for(&v6(1)), None, "the OS picks the address");
    }

    #[test]
    fn prefer_ipv4_tries_every_ipv4_address_first() {
        let picked = remote_addresses(
            [v6(1), v4(10), v6(2), v4(11)].into_iter(),
            &system(AddressFamily::PreferIpv4),
            "mx.example",
        )
        .unwrap();
        assert_eq!(picked, vec![v4(10), v4(11), v6(1), v6(2)]);

        // an IPv6-only host is still reachable
        let picked = remote_addresses(
            [v6(1)].into_iter(),
            &system(AddressFamily::PreferIpv4),
            "mx.example",
        )
        .unwrap();
        assert_eq!(picked, vec![v6(1)]);
    }

    #[test]
    fn ipv4_only_and_ipv6_only_drop_the_other_family() {
        let resolved = [v6(1), v4(10), v6(2), v4(11)];
        let picked = remote_addresses(
            resolved.into_iter(),
            &system(AddressFamily::Ipv4Only),
            "mx.example",
        )
        .unwrap();
        assert_eq!(picked, vec![v4(10), v4(11)]);
        let picked = remote_addresses(
            resolved.into_iter(),
            &system(AddressFamily::Ipv6Only),
            "mx.example",
        )
        .unwrap();
        assert_eq!(picked, vec![v6(1), v6(2)]);

        let error = remote_addresses(
            [v6(1)].into_iter(),
            &system(AddressFamily::Ipv4Only),
            "mx.example",
        )
        .expect_err("no IPv4 remote");
        assert!(
            error
                .to_string()
                .contains("no IPv4 address for mx.example (address family ipv4_only)"),
            "{error}"
        );
        let error = remote_addresses(
            [v4(10)].into_iter(),
            &system(AddressFamily::Ipv6Only),
            "mx.example",
        )
        .expect_err("no IPv6 remote");
        assert!(error.to_string().contains("no IPv6 address"), "{error}");
    }

    #[test]
    fn a_dual_stack_pool_binds_the_address_of_each_family() {
        let source = OutboundSource::pool(
            AddressFamily::DualStack,
            Some("198.51.100.5".parse().unwrap()),
            Some("2001:db8:1::5".parse().unwrap()),
        );
        let picked = remote_addresses([v4(10), v6(1)].into_iter(), &source, "mx.example").unwrap();
        assert_eq!(picked, vec![v6(1), v4(10)]);
        assert_eq!(
            source.local_for(&v6(1)),
            Some("2001:db8:1::5".parse().unwrap())
        );
        assert_eq!(
            source.local_for(&v4(10)),
            Some("198.51.100.5".parse().unwrap())
        );

        let source = OutboundSource {
            family: AddressFamily::PreferIpv4,
            ..source
        };
        let picked = remote_addresses([v6(1), v4(10)].into_iter(), &source, "mx.example").unwrap();
        assert_eq!(picked, vec![v4(10), v6(1)]);
    }

    #[test]
    fn a_pool_address_without_ipv6_stays_on_ipv4_whatever_the_family() {
        let ipv4_only_address =
            |family| OutboundSource::pool(family, Some("198.51.100.5".parse().unwrap()), None);
        for family in [AddressFamily::DualStack, AddressFamily::PreferIpv4] {
            let picked = remote_addresses(
                [v6(1), v4(10)].into_iter(),
                &ipv4_only_address(family),
                "mx.example",
            )
            .unwrap();
            assert_eq!(picked, vec![v4(10)], "{family}");
            let error = remote_addresses(
                [v6(1)].into_iter(),
                &ipv4_only_address(family),
                "mx.example",
            )
            .expect_err("pinned to IPv4");
            assert!(
                error
                    .to_string()
                    .contains("required by the bound source address"),
                "{error}"
            );
        }

        // ipv6_only on a pool address without IPv6 can never send
        let error = remote_addresses(
            [v6(1), v4(10)].into_iter(),
            &ipv4_only_address(AddressFamily::Ipv6Only),
            "mx.example",
        )
        .expect_err("no IPv6 source");
        assert!(
            error
                .to_string()
                .contains("the IP pool address has no IPv6 address to send from"),
            "{error}"
        );
    }
}
