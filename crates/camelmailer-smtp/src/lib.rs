//! CamelMailer SMTP server — the Rust port of `app/lib/smtp_server/`.

pub mod server;
pub mod session;
pub mod spf_resolver;

pub use server::SmtpServer;
pub use session::{Recipient, RecipientKind, Reply, Session, SessionConfig, State};
pub use spf_resolver::HickorySpfResolver;
