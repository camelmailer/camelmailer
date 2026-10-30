//! Emitting installation lifecycle events from the web server.
//!
//! Each helper queues one event in the outbox when
//! `lifecycle_webhooks.urls` is configured and does nothing otherwise, so a
//! self-hosted installation writes nothing. Queuing never fails the request
//! that caused it: an error is logged. The worker delivers the outbox (see
//! `camelmailer_worker::Worker::process_next_lifecycle_event`).

use camelmailer_core::lifecycle::{self, organization_data, user_data};
use camelmailer_core::{LifecycleEvent, Organization, User};
use serde_json::json;

use crate::app::ApiState;

/// Where a new account came from, reported in `user.registered`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RegistrationSource {
    /// `POST /api/v2/auth/register`.
    Registration,
    /// Accepting an invitation created the account.
    Invitation,
    /// First login through OIDC, SAML, a social or a tenant SSO provider.
    Sso,
    /// SCIM provisioning.
    Scim,
    /// Created by an administrator through the admin API.
    Admin,
}

impl RegistrationSource {
    fn as_str(self) -> &'static str {
        match self {
            RegistrationSource::Registration => "registration",
            RegistrationSource::Invitation => "invitation",
            RegistrationSource::Sso => "sso",
            RegistrationSource::Scim => "scim",
            RegistrationSource::Admin => "admin",
        }
    }
}

async fn emit(state: &ApiState, event: LifecycleEvent) {
    if !state.config.lifecycle_webhooks.enabled() {
        return;
    }
    if let Err(error) = state.store.enqueue_lifecycle_event(&event).await {
        tracing::error!(%error, event = %event.event, "could not queue a lifecycle event");
    }
}

fn actor(user: Option<&User>) -> serde_json::Value {
    user.map(user_data).unwrap_or(serde_json::Value::Null)
}

pub(crate) async fn organization_created(
    state: &ApiState,
    organization: &Organization,
    by: Option<&User>,
) {
    emit(
        state,
        LifecycleEvent::new(
            lifecycle::ORGANIZATION_CREATED,
            json!({ "organization": organization_data(organization), "user": actor(by) }),
        ),
    )
    .await;
}

pub(crate) async fn organization_deleted(
    state: &ApiState,
    organization: &Organization,
    by: Option<&User>,
) {
    emit(
        state,
        LifecycleEvent::new(
            lifecycle::ORGANIZATION_DELETED,
            json!({ "organization": organization_data(organization), "user": actor(by) }),
        ),
    )
    .await;
}

pub(crate) async fn user_registered(state: &ApiState, user: &User, source: RegistrationSource) {
    emit(
        state,
        LifecycleEvent::new(
            lifecycle::USER_REGISTERED,
            json!({ "user": user_data(user), "source": source.as_str() }),
        ),
    )
    .await;
}
