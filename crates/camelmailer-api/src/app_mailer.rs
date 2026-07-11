//! Platform email delivery ("app mail") — the installation sends its own
//! account emails (password resets, invitations, welcome mail) through its
//! own sending pipeline (dogfooding).
//!
//! Configured by the `app_mail` group: the operator points
//! `app_mail.server_api_key` at an API credential of a mail server of THIS
//! installation and sets `app_mail.from_address` (a verified sending domain
//! of that server). There is no HTTP loopback: the key is resolved exactly
//! like the messaging-API authentication (credential → server) and the mail
//! is enqueued through the same internal path as
//! `POST /api/v2/server/messages` ([`crate::server_api::enqueue_send`]).
//!
//! With `app_mail.enabled: false` (the default) everything here is a no-op.
//! Delivery failures are logged via `tracing` and never fail the request
//! that triggered the mail.

use crate::app::ApiState;
use crate::server_api::{enqueue_send, AddressInput, AddressOrString, SendMessage};

/// One platform mail: plain hardcoded subject/bodies, no dependency on the
/// template library.
pub(crate) struct AppMail {
    pub(crate) to: String,
    pub(crate) subject: String,
    pub(crate) text_body: String,
    pub(crate) html_body: String,
}

/// Minimal HTML escaping for values interpolated into the hardcoded bodies.
fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The password-reset mail: carries the frontend reset link
/// (`{frontend_url}/reset-password?token=…`).
pub(crate) fn password_reset_mail(to: &str, link: &str, expiry_hours: u32) -> AppMail {
    AppMail {
        to: to.to_string(),
        subject: "Reset your CamelMailer password".into(),
        text_body: format!(
            "Hello,\n\n\
             A password reset was requested for your CamelMailer account.\n\
             Choose a new password using this link:\n\n\
             {link}\n\n\
             The link expires in {expiry_hours} hours. If you did not request \
             a reset, you can ignore this email.\n"
        ),
        html_body: format!(
            "<p>Hello,</p>\
             <p>A password reset was requested for your CamelMailer account. \
             Choose a new password using this link:</p>\
             <p><a href=\"{link}\">{link}</a></p>\
             <p>The link expires in {expiry_hours} hours. If you did not \
             request a reset, you can ignore this email.</p>",
            link = escape_html(link),
        ),
    }
}

/// The invitation mail: carries the frontend accept link
/// (`{frontend_url}/invitations/accept?token=…`).
pub(crate) fn invitation_mail(
    to: &str,
    organization_name: &str,
    link: &str,
    expiry_days: u32,
) -> AppMail {
    AppMail {
        to: to.to_string(),
        subject: format!("You have been invited to {organization_name} on CamelMailer"),
        text_body: format!(
            "Hello,\n\n\
             You have been invited to join {organization_name} on CamelMailer.\n\
             Accept the invitation using this link:\n\n\
             {link}\n\n\
             The invitation expires in {expiry_days} days.\n"
        ),
        html_body: format!(
            "<p>Hello,</p>\
             <p>You have been invited to join <strong>{organization_name}</strong> \
             on CamelMailer. Accept the invitation using this link:</p>\
             <p><a href=\"{link}\">{link}</a></p>\
             <p>The invitation expires in {expiry_days} days.</p>",
            organization_name = escape_html(organization_name),
            link = escape_html(link),
        ),
    }
}

/// The welcome mail after self-registration. Carries no token.
pub(crate) fn welcome_mail(to: &str, first_name: &str, frontend_url: Option<&str>) -> AppMail {
    let greeting = if first_name.is_empty() {
        "Hello".to_string()
    } else {
        format!("Hello {first_name}")
    };
    let sign_in_text = frontend_url
        .map(|url| format!("\nSign in at {url}\n"))
        .unwrap_or_default();
    let sign_in_html = frontend_url
        .map(|url| {
            let url = escape_html(url);
            format!("<p>Sign in at <a href=\"{url}\">{url}</a></p>")
        })
        .unwrap_or_default();
    AppMail {
        to: to.to_string(),
        subject: "Welcome to CamelMailer".into(),
        text_body: format!(
            "{greeting},\n\n\
             Your CamelMailer account has been created.\n\
             {sign_in_text}\n\
             Happy sending!\n"
        ),
        html_body: format!(
            "<p>{greeting},</p>\
             <p>Your CamelMailer account has been created.</p>\
             {sign_in_html}\
             <p>Happy sending!</p>",
            greeting = escape_html(&greeting),
        ),
    }
}

/// Enqueue one platform mail. No-op (returns `false`) when `app_mail` is
/// disabled; on any failure a warning is logged and `false` is returned —
/// the triggering request must never fail because of platform mail.
pub(crate) async fn deliver(state: &ApiState, mail: AppMail) -> bool {
    let settings = &state.config.app_mail;
    if !settings.enabled {
        return false;
    }
    let (Some(key), Some(from_address)) = (
        settings.server_api_key.as_deref().filter(|k| !k.is_empty()),
        settings.from_address.as_deref().filter(|a| !a.is_empty()),
    ) else {
        tracing::warn!(
            "app_mail is enabled but server_api_key/from_address are missing; platform mail skipped"
        );
        return false;
    };

    // Resolve the credential to a server exactly like the messaging-API
    // auth middleware does.
    let server = match state.store.server_for_api_token(key).await {
        Ok(Some(server)) if !server.suspended => server,
        Ok(Some(_)) => {
            tracing::warn!("app_mail server is suspended; platform mail skipped");
            return false;
        }
        Ok(None) => {
            tracing::warn!(
                "app_mail.server_api_key does not resolve to a server; platform mail skipped"
            );
            return false;
        }
        Err(error) => {
            tracing::warn!(%error, "app_mail credential lookup failed; platform mail skipped");
            return false;
        }
    };

    let to = mail.to;
    let message = SendMessage {
        from: Some(AddressOrString::Object(AddressInput {
            email: from_address.to_string(),
            name: Some(settings.from_name.clone()).filter(|n| !n.is_empty()),
        })),
        to: vec![AddressOrString::String(to.clone())],
        subject: Some(mail.subject),
        html_body: Some(mail.html_body),
        text_body: Some(mail.text_body),
        ..Default::default()
    };
    match enqueue_send(state, &server, message).await {
        Ok(_) => {
            tracing::info!(to = %to, "platform mail enqueued");
            true
        }
        Err((_, code, message)) => {
            tracing::warn!(to = %to, %code, %message, "platform mail delivery failed");
            false
        }
    }
}
