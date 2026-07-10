//! CamelMailer Admin API v2 — the Rust port of `app/controllers/admin_api/`.
pub mod app;
mod resources;
pub mod tracking;

pub use app::{build_router, ApiState};
pub use tracking::{tracking_router, TrackingState};
