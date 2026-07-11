//! The TCP acceptor loop — the port of `app/lib/smtp_server/server.rb` and
//! `script/smtp_server.rb`, on tokio instead of a thread-per-connection
//! model, including STARTTLS termination via rustls.
//!
//! Besides `default_port` (always plaintext + optional STARTTLS), any number
//! of additional listeners can be configured via `smtp_server.listeners`,
//! each in mode `smtp` (like the default port) or `smtps` (implicit TLS from
//! the first byte, the classic port 465). The session state machine is
//! identical for all of them — only the I/O wrapper in front differs.

use crate::session::{Reply, Session, SessionConfig};
use camelmailer_config::SmtpListenerMode;
use camelmailer_core::{MessageSink, Store};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

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

    let certificates = rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(
        certificate_path,
    )?))
    .collect::<Result<Vec<_>, _>>()?;
    let private_key = rustls_pemfile::private_key(&mut std::io::BufReader::new(
        std::fs::File::open(private_key_path)?,
    ))?
    .ok_or_else(|| std::io::Error::other("no private key found in the TLS key file"))?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(std::io::Error::other)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

pub struct SmtpServer {
    config: camelmailer_config::Config,
    store: Arc<dyn Store>,
    sink: Arc<dyn MessageSink>,
}

impl SmtpServer {
    pub fn new(
        config: camelmailer_config::Config,
        store: Arc<dyn Store>,
        sink: Arc<dyn MessageSink>,
    ) -> Self {
        Self {
            config,
            store,
            sink,
        }
    }

    pub async fn run(self) -> std::io::Result<()> {
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
        self.serve_listeners(listeners).await
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
        let tls_acceptor = if self.config.smtp_server.tls_enabled {
            Some(load_tls_acceptor(
                &self.config.smtp_server.tls_certificate_path,
                &self.config.smtp_server.tls_private_key_path,
            )?)
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

        let this = Arc::new(self);
        let mut accept_loops = tokio::task::JoinSet::new();
        for (listener, mode) in listeners {
            let this = this.clone();
            let tls_acceptor = tls_acceptor.clone();
            accept_loops.spawn(async move {
                loop {
                    let (stream, peer) = listener.accept().await?;
                    let this = this.clone();
                    let tls_acceptor = tls_acceptor.clone();
                    tokio::spawn(async move {
                        if let Err(error) = this
                            .handle_connection(stream, peer, tls_acceptor, mode)
                            .await
                        {
                            tracing::debug!(%peer, %error, "connection ended with error");
                        }
                    });
                }
            });
        }
        // The accept loops never return Ok; surface the first error.
        match accept_loops.join_next().await {
            Some(Ok(result)) => result,
            Some(Err(join_error)) => Err(std::io::Error::other(join_error)),
            None => Ok(()),
        }
    }

    async fn handle_connection(
        &self,
        stream: TcpStream,
        peer: SocketAddr,
        tls_acceptor: Option<TlsAcceptor>,
        mode: SmtpListenerMode,
    ) -> std::io::Result<()> {
        let session_config = SessionConfig::from(&self.config);
        // With proxy_protocol enabled the client IP comes from the PROXY
        // line, not the socket peer.
        let ip_address = if self.config.smtp_server.proxy_protocol {
            None
        } else {
            Some(peer.ip().to_string())
        };
        let send_banner = ip_address.is_some();

        let mut session = Session::new(
            session_config,
            self.store.clone(),
            self.sink.clone(),
            ip_address,
        );

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
            let tls_stream = tls_acceptor.accept(stream).await?;
            session.set_tls(true);
            let mut lines = LineStream::new(tls_stream);
            if send_banner {
                lines.write_line(&session.banner()).await?;
            }
            drive_session(&mut session, &mut lines).await?;
            lines.stream.shutdown().await.ok();
            return Ok(());
        }

        let mut lines = LineStream::new(stream);
        if send_banner {
            lines.write_line(&session.banner()).await?;
        }

        // Plaintext phase: run until the connection ends or STARTTLS asks
        // for an upgrade.
        let upgrade = drive_session(&mut session, &mut lines).await?;
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
        let tls_stream = tls_acceptor.accept(lines.into_inner()).await?;
        session.set_tls(true);
        let mut tls_lines = LineStream::new(tls_stream);
        drive_session(&mut session, &mut tls_lines).await?;
        tls_lines.stream.shutdown().await.ok();
        Ok(())
    }
}

/// Pump lines through the session until the client disconnects, the session
/// finishes, or (in the plaintext phase) STARTTLS requests an upgrade.
/// Returns `true` when a TLS upgrade is requested.
async fn drive_session<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut Session,
    lines: &mut LineStream<S>,
) -> std::io::Result<bool> {
    loop {
        let Some(line) = lines.read_line().await? else {
            return Ok(false);
        };
        let reply = session.handle(&line);
        lines.write_reply(&reply).await?;
        if session.take_start_tls() {
            return Ok(true);
        }
        if session.finished() {
            return Ok(false);
        }
    }
}
