//! CamelMailer Admin API v2 — the Rust port of `app/controllers/admin_api/`.
pub mod app;
mod app_mailer;
pub mod auth_api;
pub mod billing;
pub mod cors;
pub mod dns;
mod memberships;
pub mod oidc;
mod resources;
pub mod saml;
pub mod scim;
pub mod server_api;
pub mod share;
pub mod sso;
pub mod tracking;
mod webauthn;
pub mod webhook_send;
mod xmldsig;

mod insights;

pub use app::{build_router, ApiState};
pub use auth_api::build_auth_router;
pub use billing::{BillingError, BillingProvider, MockBilling, StripeBilling};
pub use cors::cors_layer;
pub use dns::HickoryDnsResolver;
pub use oidc::build_oidc_router;
pub use saml::build_saml_router;
pub use scim::build_scim_router;
pub use server_api::build_server_router;
pub use share::build_share_router;
pub use sso::{build_sso_router, GithubOauth, HttpGithub};
pub use tracking::{tracking_router, TrackingState};
pub use webhook_send::{ReqwestWebhookSender, WebhookSender};
