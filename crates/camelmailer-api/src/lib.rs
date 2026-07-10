//! CamelMailer Admin API v2 — the Rust port of `app/controllers/admin_api/`.
pub mod app;
pub mod auth_api;
mod memberships;
pub mod oidc;
mod resources;
pub mod server_api;
pub mod tracking;

pub use app::{build_router, ApiState};
pub use auth_api::build_auth_router;
pub use oidc::build_oidc_router;
pub use server_api::build_server_router;
pub use tracking::{tracking_router, TrackingState};
