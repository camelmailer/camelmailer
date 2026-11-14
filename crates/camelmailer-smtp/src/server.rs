//! The TCP acceptor loop — the port of `app/lib/smtp_server/server.rb` and
//! `script/smtp_server.rb`, on tokio instead of a thread-per-connection
//! model, including STARTTLS termination via rustls.
//!
//! Besides `default_port` (always plaintext + optional STARTTLS), any number
//! of additional listeners can be configured via `smtp_server.listeners`,
//! each in mode `smtp` (like the default port) or `smtps` (implicit TLS from
//! the first byte, the classic port 465). The session state machine is
//! identical for all of them — only the I/O wrapper in front differs.

use crate::session::{Reply, Session, SessionConfig, State};
use camelmailer_config::SmtpListenerMode;
use camelmailer_core::{MessageSink, Store};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

/// How long a `proxy_protocol` connection may take to send its PROXY header,
/// on every listener. A load balancer sends it right after connecting.
const PROXY_HEADER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// After the grace period, how long sessions get to write their `421` and
/// close before the server stops waiting for them.
const SHUTDOWN_CLOSE_MARGIN: std::time::Duration = std::time::Duration::from_secs(2);

/// Where a graceful shutdown stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drain {
    /// Accepting connections.
    Running,
    /// No new connections; open sessions finish their transaction.
    Draining,
    /// The grace period is over: every session still open is told `421`
    /// and closed.
    Expired,
}

/// The reply a session gets when the server closes it during a shutdown.
/// RFC 5321 4.2.3 lists `421 <domain> Service not available, closing
/// transmission channel`; RFC 3463 `4.3.2` is "system not accepting network
/// messages", a temporary condition, so the client retries elsewhere or
/// later. Nothing it was told `250` for is lost: a message is queued before
/// its `250` is sent.
fn shutdown_reply(hostname: &str) -> String {
    format!("421 4.3.2 {hostname} Service shutting down, closing transmission channel")
}

/// The sessions open right now, so a shutdown can wait for them.
#[derive(Default)]
struct OpenSessions {
    count: std::sync::atomic::AtomicUsize,
    changed: tokio::sync::Notify,
}

impl OpenSessions {
    fn open(self: &Arc<Self>) -> OpenSession {
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        OpenSession(self.clone())
    }

    fn len(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Resolves once no session is open.
    async fn closed(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.len() == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// One open session; closing it (dropping the guard) wakes a waiting
/// shutdown.
struct OpenSession(Arc<OpenSessions>);

impl Drop for OpenSession {
    fn drop(&mut self) {
        self.0
            .count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }
}

/// What a session needs to know about a shutdown in progress.
#[derive(Clone)]
struct ShutdownWatch {
    drain: tokio::sync::watch::Receiver<Drain>,
    reply: Arc<str>,
}

impl ShutdownWatch {
    fn draining(&self) -> bool {
        *self.drain.borrow() != Drain::Running
    }

    /// Resolves when the grace period is over (or the server is gone).
    async fn expired(&mut self) {
        let _ = self.drain.wait_for(|d| *d == Drain::Expired).await;
    }
}

/// Why no PROXY header line came off the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyReadFailure {
    /// The peer closed the connection first.
    Closed,
    /// No `\n` within [`crate::proxy::V1_MAX_LENGTH`] bytes.
    TooLong,
    /// Nothing complete within [`PROXY_HEADER_TIMEOUT`].
    TimedOut,
}

impl std::fmt::Display for ProxyReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Closed => "closed before a PROXY header",
            Self::TooLong => "PROXY header longer than 107 bytes",
            Self::TimedOut => "no PROXY header within the timeout",
        })
    }
}

/// Read a PROXY protocol v1 header from the raw socket, one byte at a time
/// so not a single byte that follows it is consumed: on an `smtps` listener
/// that is the client's TLS handshake. At most 107 bytes, so a byte-wise read
/// costs nothing worth buffering for (a `peek` loop would spin on a header
/// that arrives in pieces).
///
/// Returns the line without its `\n` (a trailing `\r` is kept, as the
/// session expects).
async fn read_proxy_header<S: AsyncRead + Unpin>(
    stream: &mut S,
    timeout: std::time::Duration,
) -> std::io::Result<Result<String, ProxyReadFailure>> {
    let read = async {
        let mut line = Vec::with_capacity(crate::proxy::V1_MAX_LENGTH);
        loop {
            let mut byte = [0u8; 1];
            if stream.read(&mut byte).await? == 0 {
                return Ok(Err(ProxyReadFailure::Closed));
            }
            if byte[0] == b'\n' {
                return Ok(Ok(String::from_utf8_lossy(&line).into_owned()));
            }
            line.push(byte[0]);
            if line.len() >= crate::proxy::V1_MAX_LENGTH {
                return Ok(Err(ProxyReadFailure::TooLong));
            }
        }
    };
    match tokio::time::timeout(timeout, read).await {
        Ok(result) => result,
        Err(_) => Ok(Err(ProxyReadFailure::TimedOut)),
    }
}

/// Log a refused PROXY header. Every refusal goes to debug; a warning goes
/// out at most once a minute per process, with the count since the last one,
/// because a refusal almost always means a misconfigured balancer (no
/// `send-proxy`, or PROXY v2) and then every connection fails the same way.
fn report_proxy_refusal(peer: SocketAddr, mode: SmtpListenerMode, reason: &dyn std::fmt::Display) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST_WARNING: AtomicU64 = AtomicU64::new(0);
    static SUPPRESSED: AtomicU64 = AtomicU64::new(0);
    const INTERVAL_SECONDS: u64 = 60;

    tracing::debug!(%peer, ?mode, %reason, "PROXY header refused, connection closed");
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = LAST_WARNING.load(Ordering::Relaxed);
    if (last == 0 || now.saturating_sub(last) >= INTERVAL_SECONDS)
        && LAST_WARNING
            .compare_exchange(last, now.max(1), Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        let suppressed = SUPPRESSED.swap(0, Ordering::Relaxed);
        tracing::warn!(
            %peer,
            ?mode,
            %reason,
            suppressed_since_last_warning = suppressed,
            "proxy_protocol is on but a connection sent no valid PROXY v1 header; \
             check that the load balancer sends PROXY protocol v1 (send-proxy, not v2)"
        );
    } else {
        SUPPRESSED.fetch_add(1, Ordering::Relaxed);
    }
}

/// A line-oriented reader/writer over any stream, kept upgradeable: unlike
/// a `BufReader` split, the inner stream can be taken back out for the TLS
/// handshake after STARTTLS.
struct LineStream<S> {
    stream: S,
    buffer: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> LineStream<S> {
    fn new(stream: S) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
        }
    }

    fn into_inner(self) -> S {
        self.stream
    }

    /// Read one line (without the trailing `\n`). `None` on EOF.
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            if let Some(position) = self.buffer.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=position).collect();
                line.pop(); // the \n
                return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
            }
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(None);
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    async fn write_reply(&mut self, reply: &Reply) -> std::io::Result<()> {
        match reply {
            Reply::None => Ok(()),
            Reply::Line(text) => {
                self.stream
                    .write_all(format!("{text}\r\n").as_bytes())
                    .await
            }
            Reply::Lines(lines) => {
                let mut out = String::new();
                for text in lines {
                    out.push_str(text);
                    out.push_str("\r\n");
                }
                self.stream.write_all(out.as_bytes()).await
            }
        }
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.stream
            .write_all(format!("{line}\r\n").as_bytes())
            .await
    }
}

fn load_tls_acceptor(
    certificate_path: &str,
    private_key_path: &str,
) -> std::io::Result<TlsAcceptor> {
    // rustls needs a process-wide crypto provider; installing twice is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let certificates = CertificateDer::pem_file_iter(certificate_path)
        .map_err(pem_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(pem_error)?;
    let private_key = PrivateKeyDer::from_pem_file(private_key_path).map_err(pem_error)?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(std::io::Error::other)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Map a PEM parsing failure onto `io::Error`, keeping a real I/O error (a
/// missing or unreadable file) as it is.
fn pem_error(error: rustls::pki_types::pem::Error) -> std::io::Error {
    match error {
        rustls::pki_types::pem::Error::Io(error) => error,
        rustls::pki_types::pem::Error::NoItemsFound => {
            std::io::Error::other("no private key found in the TLS key file")
        }
        other => std::io::Error::other(other.to_string()),
    }
}

/// Modification times of the certificate and key, the signal that a renewal
/// has landed. `None` for a file that cannot be stated right now, which
/// compares unequal to a real timestamp and so asks for a reload attempt.
type CertificateStamp = (Option<SystemTime>, Option<SystemTime>);

/// Hands out the current TLS acceptor, rebuilding it after the certificate on
/// disk changes.
///
/// The acceptor used to be built once, when the listeners were bound. That
/// quietly outlived the certificate: an ACME certificate is replaced every
/// ~60 days and the running process kept presenting the expired one until
/// somebody restarted the service. Nothing in the process noticed, and
/// neither did anything else until a client refused the handshake.
///
/// So the certificate is stated before each handshake and reloaded when its
/// modification time moves. That is one `stat` per connection that actually
/// negotiates TLS, against a page the kernel has cached.
struct TlsReloader {
    certificate_path: String,
    private_key_path: String,
    current: tokio::sync::RwLock<(TlsAcceptor, CertificateStamp)>,
}

impl TlsReloader {
    fn load(certificate_path: &str, private_key_path: &str) -> std::io::Result<Self> {
        let acceptor = load_tls_acceptor(certificate_path, private_key_path)?;
        let stamp = Self::stamp(certificate_path, private_key_path);
        Ok(Self {
            certificate_path: certificate_path.to_string(),
            private_key_path: private_key_path.to_string(),
            current: tokio::sync::RwLock::new((acceptor, stamp)),
        })
    }

    /// Stats both files synchronously. The workspace keeps tokio's feature
    /// list short and does not enable `fs`, and a `stat` of a path the kernel
    /// has cached costs about as much as the atomic loads around it. The
    /// reload below reads the files synchronously too, exactly as the startup
    /// path always has.
    fn stamp(certificate_path: &str, private_key_path: &str) -> CertificateStamp {
        fn modified(path: &str) -> Option<SystemTime> {
            std::fs::metadata(path).ok()?.modified().ok()
        }
        (modified(certificate_path), modified(private_key_path))
    }

    async fn acceptor(&self) -> TlsAcceptor {
        let on_disk = Self::stamp(&self.certificate_path, &self.private_key_path);
        {
            let current = self.current.read().await;
            if current.1 == on_disk {
                return current.0.clone();
            }
        }
        match load_tls_acceptor(&self.certificate_path, &self.private_key_path) {
            Ok(acceptor) => {
                let mut current = self.current.write().await;
                *current = (acceptor.clone(), on_disk);
                tracing::info!(
                    certificate = %self.certificate_path,
                    "reloaded the SMTP TLS certificate"
                );
                acceptor
            }
            Err(error) => {
                // A renewal caught mid-write, or a file that has gone bad.
                // Keep serving the certificate already loaded rather than
                // dropping TLS, and try again on the next handshake: the
                // stamp stays unrecorded, so this stays a retry.
                tracing::warn!(
                    %error,
                    certificate = %self.certificate_path,
                    "could not load the SMTP TLS certificate, keeping the previous one"
                );
                self.current.read().await.0.clone()
            }
        }
    }
}

pub struct SmtpServer {
    config: camelmailer_config::Config,
    store: Arc<dyn Store>,
    sink: Arc<dyn MessageSink>,
    /// Optional inbound-SPF resolver. When set, each transaction's
    /// envelope-From is SPF-checked against the client IP and the verdict is
    /// stamped as a `Received-SPF:` header on the stored message. Record-only
    /// — SPF never rejects a message. `None` disables the check (the default,
    /// and what tests using `serve`/`serve_listeners` get); `run` wires the
    /// production hickory resolver in.
    spf_resolver: Option<Arc<dyn camelmailer_core::SpfResolver>>,
    /// How long a `proxy_protocol` connection may take to send its PROXY
    /// header ([`PROXY_HEADER_TIMEOUT`]; tests shorten it).
    proxy_header_timeout: std::time::Duration,
    /// `smtp_server.shutdown_grace_seconds`.
    shutdown_grace: std::time::Duration,
}

impl SmtpServer {
    pub fn new(
        config: camelmailer_config::Config,
        store: Arc<dyn Store>,
        sink: Arc<dyn MessageSink>,
    ) -> Self {
        Self {
            store,
            sink,
            spf_resolver: None,
            proxy_header_timeout: PROXY_HEADER_TIMEOUT,
            shutdown_grace: std::time::Duration::from_secs(u64::from(
                config.smtp_server.shutdown_grace_seconds,
            )),
            config,
        }
    }

    /// Override the grace period of a graceful shutdown
    /// (`smtp_server.shutdown_grace_seconds`).
    pub fn with_shutdown_grace(mut self, grace: std::time::Duration) -> Self {
        self.shutdown_grace = grace;
        self
    }

    /// Override how long a `proxy_protocol` connection may take to send its
    /// PROXY header. For tests; production keeps [`PROXY_HEADER_TIMEOUT`].
    #[doc(hidden)]
    pub fn with_proxy_header_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.proxy_header_timeout = timeout;
        self
    }

    /// Enable inbound SPF evaluation with the given resolver. Purely
    /// informational (a `Received-SPF:` header on the stored message); it
    /// never blocks or rejects mail.
    pub fn with_spf_resolver(mut self, resolver: Arc<dyn camelmailer_core::SpfResolver>) -> Self {
        self.spf_resolver = Some(resolver);
        self
    }

    /// Serve until the process ends.
    pub async fn run(self) -> std::io::Result<()> {
        self.run_until(std::future::pending()).await
    }

    /// Serve until `shutdown` resolves, then shut down gracefully: stop
    /// accepting connections, let open sessions finish their transaction for
    /// up to `smtp_server.shutdown_grace_seconds`, then tell the rest `421`
    /// and close them.
    pub async fn run_until(
        mut self,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> std::io::Result<()> {
        // Enable inbound SPF in production: evaluate the envelope-From against
        // the client IP and record a Received-SPF header (never blocking).
        // Tests that call serve()/serve_listeners() directly leave it off.
        if self.spf_resolver.is_none() {
            match crate::spf_resolver::HickorySpfResolver::from_system() {
                Some(resolver) => self.spf_resolver = Some(Arc::new(resolver)),
                None => tracing::warn!("could not build SPF resolver; inbound SPF disabled"),
            }
        }
        let port = std::env::var("PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(self.config.smtp_server.default_port);
        let bind_address = std::env::var("BIND_ADDRESS")
            .unwrap_or_else(|_| self.config.smtp_server.default_bind_address.clone());

        // The default port always speaks plain SMTP (with optional STARTTLS);
        // additional listeners come from `smtp_server.listeners`.
        let mut listeners = vec![(
            TcpListener::bind((bind_address.as_str(), port)).await?,
            SmtpListenerMode::Smtp,
        )];
        tracing::info!(%bind_address, port, mode = "smtp", "camelmailer SMTP server listening");
        for listener in &self.config.smtp_server.listeners {
            listeners.push((
                TcpListener::bind((bind_address.as_str(), listener.port)).await?,
                listener.mode,
            ));
            tracing::info!(
                %bind_address,
                port = listener.port,
                mode = ?listener.mode,
                "camelmailer SMTP server listening"
            );
        }
        self.serve_listeners_until(listeners, shutdown).await
    }

    /// Accept connections on an existing listener in plain-SMTP mode (used
    /// by tests to bind an ephemeral port).
    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        self.serve_listeners(vec![(listener, SmtpListenerMode::Smtp)])
            .await
    }

    /// Accept connections on any number of existing listeners, each with its
    /// own mode (`smtp` = plaintext + optional STARTTLS, `smtps` = implicit
    /// TLS from the first byte).
    pub async fn serve_listeners(
        self,
        listeners: Vec<(TcpListener, SmtpListenerMode)>,
    ) -> std::io::Result<()> {
        self.serve_listeners_until(listeners, std::future::pending())
            .await
    }

    /// Like [`SmtpServer::serve_listeners`], shutting down gracefully once
    /// `shutdown` resolves (see [`SmtpServer::run_until`]). Returns when
    /// every session has closed, or the grace period and a short close
    /// margin are over.
    pub async fn serve_listeners_until(
        self,
        listeners: Vec<(TcpListener, SmtpListenerMode)>,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> std::io::Result<()> {
        let tls_acceptor = if self.config.smtp_server.tls_enabled {
            Some(Arc::new(TlsReloader::load(
                &self.config.smtp_server.tls_certificate_path,
                &self.config.smtp_server.tls_private_key_path,
            )?))
        } else {
            None
        };
        if tls_acceptor.is_none()
            && listeners
                .iter()
                .any(|(_, mode)| *mode == SmtpListenerMode::Smtps)
        {
            // Config validation rejects this; kept as a defensive check for
            // programmatic callers.
            return Err(std::io::Error::other(
                "smtps listeners require smtp_server.tls_enabled",
            ));
        }

        let grace = self.shutdown_grace;
        let (drain_tx, drain_rx) = tokio::sync::watch::channel(Drain::Running);
        let watch = ShutdownWatch {
            drain: drain_rx,
            reply: shutdown_reply(&self.config.camelmailer.smtp_hostname).into(),
        };
        let sessions = Arc::new(OpenSessions::default());
        let this = Arc::new(self);
        let mut accept_loops = tokio::task::JoinSet::new();
        for (listener, mode) in listeners {
            let this = this.clone();
            let tls_acceptor = tls_acceptor.clone();
            let mut watch = watch.clone();
            let sessions = sessions.clone();
            accept_loops.spawn(async move {
                loop {
                    let (stream, peer) = tokio::select! {
                        // Draining: stop accepting. Dropping the listener
                        // closes the socket, so new connections are refused.
                        _ = watch.drain.wait_for(|d| *d != Drain::Running) => return Ok(()),
                        accepted = listener.accept() => accepted?,
                    };
                    let this = this.clone();
                    let tls_acceptor = tls_acceptor.clone();
                    let watch = watch.clone();
                    let open = sessions.open();
                    tokio::spawn(async move {
                        let _open = open;
                        if let Err(error) = this
                            .handle_connection(stream, peer, tls_acceptor, mode, watch)
                            .await
                        {
                            tracing::debug!(%peer, %error, "connection ended with error");
                        }
                    });
                }
            });
        }

        // Until the shutdown, the accept loops only end on an error.
        tokio::pin!(shutdown);
        tokio::select! {
            _ = &mut shutdown => {}
            ended = accept_loops.join_next() => {
                return match ended {
                    Some(Ok(result)) => result,
                    Some(Err(join_error)) => Err(std::io::Error::other(join_error)),
                    None => Ok(()),
                };
            }
        }

        tracing::info!(
            open_sessions = sessions.len(),
            grace_seconds = grace.as_secs(),
            "SMTP server shutting down: no new connections, open sessions finish their transaction"
        );
        let _ = drain_tx.send(Drain::Draining);
        while accept_loops.join_next().await.is_some() {}
        if tokio::time::timeout(grace, sessions.closed())
            .await
            .is_err()
        {
            tracing::info!(
                open_sessions = sessions.len(),
                "SMTP shutdown grace period over; closing the remaining sessions with 421"
            );
            let _ = drain_tx.send(Drain::Expired);
            if tokio::time::timeout(SHUTDOWN_CLOSE_MARGIN, sessions.closed())
                .await
                .is_err()
            {
                tracing::warn!(
                    open_sessions = sessions.len(),
                    "SMTP sessions still open after the shutdown; they are dropped"
                );
            }
        }
        Ok(())
    }

    async fn handle_connection(
        &self,
        mut stream: TcpStream,
        peer: SocketAddr,
        tls_acceptor: Option<Arc<TlsReloader>>,
        mode: SmtpListenerMode,
        mut watch: ShutdownWatch,
    ) -> std::io::Result<()> {
        let session_config = SessionConfig::from(&self.config);
        // With proxy_protocol enabled the client IP comes from the PROXY
        // line, not the socket peer.
        let proxy_protocol = self.config.smtp_server.proxy_protocol;
        let ip_address = if proxy_protocol {
            None
        } else {
            Some(peer.ip().to_string())
        };

        let mut session = Session::new(
            session_config,
            self.store.clone(),
            self.sink.clone(),
            ip_address,
        );
        // `PROXY UNKNOWN` (a balancer health check) falls back to this.
        session.set_peer_ip(peer.ip().to_string());

        // Context for the (optional) inbound SPF check, evaluated just before
        // DATA once MAIL FROM and the client IP are known.
        let spf = self.spf_resolver.as_ref().map(|resolver| SpfContext {
            resolver: resolver.clone(),
            receiver: self.config.camelmailer.smtp_hostname.clone(),
        });

        // proxy_protocol: the PROXY line is the first thing on the socket on
        // every listener, read with the same length cap and timeout. On smtps
        // it comes in the clear in front of the client's TLS handshake, so
        // it is read byte by byte from the raw socket and the handshake bytes
        // that follow stay untouched.
        let mut proxy_reply = None;
        if proxy_protocol {
            let header = match read_proxy_header(&mut stream, self.proxy_header_timeout).await? {
                Ok(header) => header,
                Err(failure) => {
                    report_proxy_refusal(peer, mode, &failure);
                    if mode == SmtpListenerMode::Smtp && failure == ProxyReadFailure::TooLong {
                        // Same answer as for any other invalid line in plain text.
                        stream.write_all(b"502 Proxy Error\r\n").await.ok();
                    }
                    return Ok(());
                }
            };
            let reply = session.handle(&header);
            if session.finished() {
                if let Err(reason) = crate::proxy::parse_v1(header.trim_end_matches('\r')) {
                    report_proxy_refusal(peer, mode, &reason);
                } else {
                    // `UNKNOWN` without a peer address; unreachable here.
                    report_proxy_refusal(peer, mode, &"PROXY UNKNOWN without a peer address");
                }
                if mode == SmtpListenerMode::Smtp {
                    // Plain text can carry the session's 502; a TLS client
                    // cannot read it, so smtps just closes.
                    stream.write_all(b"502 Proxy Error\r\n").await.ok();
                }
                return Ok(());
            }
            // The banner. On smtps it is held back and sent over TLS below.
            proxy_reply = Some(reply);
        }

        if mode == SmtpListenerMode::Smtps {
            // Implicit TLS: handshake before the first SMTP byte, then run
            // the same session over the encrypted stream. The session starts
            // in the TLS state (messages marked `received_with_ssl`, AUTH
            // advertised immediately — exactly as after a STARTTLS upgrade).
            let Some(tls_acceptor) = tls_acceptor else {
                // serve_listeners() refuses smtps without an acceptor — this
                // is unreachable, kept as a defensive close.
                return Ok(());
            };
            let tls_stream = tls_acceptor.acceptor().await.accept(stream).await?;
            session.set_tls(true);
            let mut lines = LineStream::new(tls_stream);
            lines.write_line(&session.banner()).await?;
            drive_session(&mut session, &mut lines, spf.as_ref(), &mut watch).await?;
            lines.stream.shutdown().await.ok();
            return Ok(());
        }

        let mut lines = LineStream::new(stream);
        match proxy_reply {
            Some(reply) => lines.write_reply(&reply).await?,
            None => lines.write_line(&session.banner()).await?,
        }

        // Plaintext phase: run until the connection ends or STARTTLS asks
        // for an upgrade.
        let upgrade = drive_session(&mut session, &mut lines, spf.as_ref(), &mut watch).await?;
        if !upgrade {
            return Ok(());
        }

        let Some(tls_acceptor) = tls_acceptor else {
            // The session only offers STARTTLS when tls_enabled, and serve()
            // builds an acceptor whenever it is — this is unreachable, kept
            // as a defensive close.
            return Ok(());
        };

        // TLS phase: handshake on the raw socket, then continue the same
        // session over the encrypted stream.
        let tls_stream = tls_acceptor
            .acceptor()
            .await
            .accept(lines.into_inner())
            .await?;
        session.set_tls(true);
        let mut tls_lines = LineStream::new(tls_stream);
        drive_session(&mut session, &mut tls_lines, spf.as_ref(), &mut watch).await?;
        tls_lines.stream.shutdown().await.ok();
        Ok(())
    }
}

/// True when a line is the `DATA` command (case-insensitive, ignoring leading
/// whitespace), the point at which the SMTP envelope is complete.
fn is_data_command(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("DATA")
}

/// Everything the inbound SPF check needs, captured per connection.
struct SpfContext {
    resolver: Arc<dyn camelmailer_core::SpfResolver>,
    receiver: String,
}

/// Evaluate the envelope-From's SPF policy against the client IP and stamp
/// the result onto the session as a `Received-SPF:` header. Best-effort and
/// non-blocking: any missing piece (no client IP, a null/`<>` return path, an
/// address without a domain) simply skips the check. Never rejects mail.
async fn evaluate_inbound_spf(session: &mut Session, spf: &SpfContext) {
    let Some(ip) = session
        .ip_address()
        .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
    else {
        return;
    };
    let Some(envelope_from) = session.mail_from().map(str::to_string) else {
        return;
    };
    let Some(domain) = envelope_from
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_string())
        .filter(|domain| !domain.is_empty())
    else {
        return;
    };
    let helo = session.helo_name().unwrap_or_default().to_string();

    let result = camelmailer_core::evaluate_spf(spf.resolver.as_ref(), &domain, ip).await;
    let header =
        camelmailer_core::received_spf_header(result, &spf.receiver, &envelope_from, ip, &helo);
    session.set_received_spf(header);
}

/// Pump lines through the session until the client disconnects, the session
/// finishes, or (in the plaintext phase) STARTTLS requests an upgrade.
/// Returns `true` when a TLS upgrade is requested.
///
/// During a shutdown the session runs on until its open transaction ends
/// (the message accepted, refused or reset), then gets `421` and is closed.
/// Once the grace period is over, a session still open gets `421` at once,
/// whatever it was doing; a message it had not been told `250` for is
/// simply not accepted, and the client retries it.
async fn drive_session<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut Session,
    lines: &mut LineStream<S>,
    spf: Option<&SpfContext>,
    watch: &mut ShutdownWatch,
) -> std::io::Result<bool> {
    let mut spf_evaluated = false;
    loop {
        // Reading a line is cancel safe: bytes already read stay in the
        // buffer, so losing the race against the grace period drops nothing.
        let line = tokio::select! {
            line = lines.read_line() => line?,
            _ = watch.expired() => {
                lines.write_line(&watch.reply).await.ok();
                return Ok(false);
            }
        };
        let Some(line) = line else {
            return Ok(false);
        };
        let in_transaction = session.in_transaction();
        // Right before the client sends DATA (envelope complete), evaluate SPF
        // once so the verdict can be prepended to the stored message. Skipped
        // when SPF is disabled or the transaction never reached RCPT TO.
        if !spf_evaluated && session.state() == State::RcptToReceived && is_data_command(&line) {
            if let Some(spf) = spf {
                evaluate_inbound_spf(session, spf).await;
            }
            spf_evaluated = true;
        }
        let reply = session.handle(&line);
        lines.write_reply(&reply).await?;
        if session.take_start_tls() {
            return Ok(true);
        }
        if session.finished() {
            return Ok(false);
        }
        if watch.draining() && in_transaction && !session.in_transaction() {
            // Shutting down and the transaction is over: close cleanly.
            lines.write_line(&watch.reply).await.ok();
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod proxy_header_tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn reads_exactly_the_header_line_and_nothing_after_it() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        client
            .write_all(b"PROXY TCP4 203.0.113.7 10.0.70.5 1 465\r\n\x16\x03\x01rest")
            .await
            .unwrap();
        let header = read_proxy_header(&mut server, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            header,
            Ok("PROXY TCP4 203.0.113.7 10.0.70.5 1 465\r".to_string())
        );
        // The TLS ClientHello bytes are still on the stream.
        let mut rest = [0u8; 7];
        server.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b"\x16\x03\x01rest");
    }

    #[tokio::test]
    async fn stops_at_the_v1_length_cap() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        client.write_all(&[b'A'; 2000]).await.unwrap();
        let header = read_proxy_header(&mut server, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(header, Err(ProxyReadFailure::TooLong));
        // 106 bytes plus the line end is the longest header that fits.
        let (mut client, mut server) = tokio::io::duplex(4096);
        let mut longest = vec![b'A'; crate::proxy::V1_MAX_LENGTH - 1];
        longest.push(b'\n');
        client.write_all(&longest).await.unwrap();
        let header = read_proxy_header(&mut server, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(header.is_ok());
    }

    #[tokio::test]
    async fn gives_up_after_the_timeout_and_on_close() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        client.write_all(b"PROXY TCP4 ").await.unwrap();
        let header = read_proxy_header(&mut server, Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(header, Err(ProxyReadFailure::TimedOut));
        drop(client);
        let header = read_proxy_header(&mut server, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(header, Err(ProxyReadFailure::Closed));
    }
}
