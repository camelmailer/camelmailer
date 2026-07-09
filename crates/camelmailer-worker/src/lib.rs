//! CamelMailer delivery worker — the Rust port of `script/worker.rb`,
//! `app/lib/message_dequeuer` and `app/senders`.

pub mod sender;
pub mod smtp_client;
pub mod worker;

pub use sender::SmtpSender;
pub use smtp_client::SendOutcome;
pub use worker::{ProcessOutcome, Worker};
