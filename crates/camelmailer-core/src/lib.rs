//! CamelMailer core domain model and storage abstractions.
//!
//! This crate is the Rust counterpart of the ActiveRecord models plus the
//! shared helpers (`app/models`, `app/lib/received_header.rb`,
//! `Postal::Helpers`).

pub mod admin_store;
pub mod message;
pub mod model;
pub mod received_header;
pub mod store;
pub mod testing;
pub mod token;

pub use admin_store::{
    AdminStore, NewCredential, NewIpAddress, NewOrganization, NewRoute, NewServer,
    NewSuppression, NewUser, NewWebhook, StoreError, TrackingStore, TrackingTarget,
};
pub use message::{MemorySink, MessageScope, MessageSink, QueuedMessage};
pub use model::*;
pub use store::{MemoryStore, Store};
