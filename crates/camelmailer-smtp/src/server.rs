//! The TCP acceptor loop — the port of `app/lib/smtp_server/server.rb` and
//! `script/smtp_server.rb`, on tokio instead of a thread-per-connection
//! model.
//!
//! TLS termination (STARTTLS upgrade) is not yet implemented in the Rust
//! port; the server refuses to start with `smtp_server.tls_enabled: true`
//! rather than advertising a capability it cannot honour.

use crate::session::{Reply, Session, SessionConfig};
use camelmailer_core::{MessageSink, Store};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

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
        if self.config.smtp_server.tls_enabled {
            return Err(std::io::Error::other(
                "smtp_server.tls_enabled is not yet supported by the Rust SMTP server; \
                 terminate TLS in front of it or disable the option",
            ));
        }

        let port = std::env::var("PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(self.config.smtp_server.default_port);
        let bind_address = std::env::var("BIND_ADDRESS")
            .unwrap_or_else(|_| self.config.smtp_server.default_bind_address.clone());

        let listener = TcpListener::bind((bind_address.as_str(), port)).await?;
        tracing::info!(%bind_address, port, "camelmailer SMTP server listening");

        let this = Arc::new(self);
        loop {
            let (stream, peer) = listener.accept().await?;
            let this = this.clone();
            tokio::spawn(async move {
                if let Err(error) = this.handle_connection(stream, peer).await {
                    tracing::debug!(%peer, %error, "connection ended with error");
                }
            });
        }
    }

    async fn handle_connection(
        &self,
        stream: TcpStream,
        peer: SocketAddr,
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

        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        if send_banner {
            write_half
                .write_all(format!("{}\r\n", session.banner()).as_bytes())
                .await?;
        }

        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            let read = reader.read_until(b'\n', &mut buffer).await?;
            if read == 0 {
                break;
            }
            if buffer.last() == Some(&b'\n') {
                buffer.pop();
            }
            let line = String::from_utf8_lossy(&buffer).into_owned();
            let reply = session.handle(&line);
            match reply {
                Reply::None => {}
                Reply::Line(text) => {
                    write_half
                        .write_all(format!("{text}\r\n").as_bytes())
                        .await?;
                }
                Reply::Lines(lines) => {
                    let mut out = String::new();
                    for text in lines {
                        out.push_str(&text);
                        out.push_str("\r\n");
                    }
                    write_half.write_all(out.as_bytes()).await?;
                }
            }
            if session.take_start_tls() {
                // Unreachable while tls_enabled is rejected in run(), kept as
                // a guard for when TLS support lands.
                break;
            }
            if session.finished() {
                break;
            }
        }
        write_half.shutdown().await.ok();
        Ok(())
    }
}
