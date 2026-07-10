//! CamelMailer delivery worker — the Rust port of `script/worker.rb`,
//! `app/lib/message_dequeuer` and `app/senders`.

pub mod dkim;
pub mod inspection;
pub mod sender;
pub mod signer;
pub mod tracking;
pub mod smtp_client;
pub mod worker;

pub use sender::SmtpSender;
pub use signer::Signer;
pub use smtp_client::SendOutcome;
pub use worker::{ProcessOutcome, WebhookOutcome, Worker};
