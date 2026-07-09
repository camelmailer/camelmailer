//! CamelMailer SMTP server — the Rust port of `app/lib/smtp_server/`.

pub mod server;
pub mod session;

pub use server::SmtpServer;
pub use session::{Recipient, RecipientKind, Reply, Session, SessionConfig, State};
