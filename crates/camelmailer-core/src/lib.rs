//! CamelMailer core domain model and storage abstractions.
//!
//! This crate is the Rust counterpart of the ActiveRecord models plus the
//! shared helpers (`app/models`, `app/lib/received_header.rb`,
//! `Postal::Helpers`).

pub mod admin_store;
pub mod auth;
pub mod auth_store;
pub mod bounce;
pub mod dmarc;
pub mod dns;
pub mod message;
pub mod mime;
pub mod model;
pub mod org_sso;
pub mod received_header;
pub mod server_store;
pub mod store;
pub mod template;
pub mod testing;
pub mod token;

pub use admin_store::{
    AdminStore, NewCredential, NewIpAddress, NewOrganization, NewRoute, NewSenderAddress,
    NewServer, NewSuppression, NewUser, NewWebhook, StoreError, TrackingStore, TrackingTarget,
};
pub use auth::{
    AuthEvent, AuthSession, Invitation, NewAuthEvent, NewAuthSession, NewInvitation,
    NewWebAuthnCredential, OrganizationMembership, Role, UserAuth, WebAuthnCredential,
};
pub use auth_store::AuthStore;
pub use bounce::BounceCategory;
pub use dmarc::{
    DmarcFilter, DmarcRecordRow, DmarcReport, DmarcSourceStat, DmarcSummary, NewDmarcRecord,
    NewDmarcReport, DMARC_REPORTS_ENDPOINT,
};
pub use dns::{DnsError, DnsResolver, StaticDnsResolver};
pub use message::{
    MemorySink, MessageRecord, MessageScope, MessageSink, QueuedMessage, SentMessage,
};
pub use model::*;
pub use org_sso::{
    NewOrgEmailDomain, NewOrgSsoConnection, OrgEmailDomain, OrgSsoConnection,
    OrgSsoConnectionUpdate, OrgSsoStore, SsoKind,
};
pub use server_store::{
    ActivityEvent, ApiRequestFilter, ApiRequestRecord, CampaignStats, CampaignUpdate,
    DeliveryRecord, DeliveryStats, MessageFilter, MessageShare, MessageStats, NewApiRequest,
    NewLayout, NewMessageShare, NewStream, NewTemplate, QueuedDomain, ServerContext, ServerStore,
    StatsFilter, TagCount,
};
pub use store::{MemoryStore, Store};
pub use template::{
    render as render_template, render_in_layout, wrapper_has_raw_content, RenderError,
};
