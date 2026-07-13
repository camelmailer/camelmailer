//! The core domain model, ported from the ActiveRecord models in
//! `app/models`. These are plain data types; persistence is behind the
//! [`crate::store::Store`] trait so the SMTP server and admin API can be
//! tested without a database.

use serde::Serialize;

pub type Id = u64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Organization {
    pub id: Id,
    pub uuid: String,
    pub name: String,
    pub permalink: String,
    /// Org-wide two-factor enforcement (Postmark-style): while set, users
    /// without an active second factor (TOTP or a passkey) may not access
    /// this organization's resources via a user session. Admin API keys
    /// are unaffected.
    pub require_two_factor: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Server {
    pub id: Id,
    pub uuid: String,
    pub organization_id: Id,
    pub name: String,
    pub permalink: String,
    pub token: String,
    pub mode: ServerMode,
    pub suspended: bool,
    pub suspension_reason: Option<String>,
    pub privacy_mode: bool,
    pub log_smtp_data: bool,
    /// Whether the Sender header may authenticate a message in addition to From
    pub allow_sender: bool,
    /// IP pool to source outbound mail from (None = system default).
    pub ip_pool_id: Option<Id>,
    /// Default open-tracking for mail sent via the HTTP API.
    pub track_opens: bool,
    /// Default click-tracking for mail sent via the HTTP API.
    pub track_clicks: bool,
    /// Per-server spam threshold override (None = installation default).
    pub spam_threshold: Option<f64>,
    /// Per-server outbound spam threshold override.
    pub outbound_spam_threshold: Option<f64>,
    pub bounce_hook_url: Option<String>,
    pub delivery_hook_url: Option<String>,
    /// Domain that accepts inbound mail for this server.
    pub inbound_domain: Option<String>,
    /// UI accent color.
    pub color: Option<String>,
    /// Default message stream for HTTP sends (populated by migration 0012).
    pub default_stream_id: Option<Id>,
}

impl Server {
    pub fn full_permalink(&self, organization: &Organization) -> String {
        format!("{}/{}", organization.permalink, self.permalink)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum ServerMode {
    Live,
    Development,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Domain {
    pub id: Id,
    pub uuid: String,
    pub owner: DomainOwner,
    pub name: String,
    pub verified: bool,
    /// Stable token published as a TXT record at
    /// `_camelmailer-challenge.<domain>` to prove ownership. Generated once
    /// when the domain is created.
    pub verification_token: String,
    /// Per-domain DKIM RSA private key (PEM). `None` means the domain signs
    /// with the installation key (`camelmailer.signing_key_path`) — that
    /// fallback stays valid forever. Never serialized.
    #[serde(skip_serializing)]
    pub dkim_private_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "id")]
pub enum DomainOwner {
    Organization(Id),
    Server(Id),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Route {
    pub id: Id,
    pub uuid: String,
    pub server_id: Id,
    pub domain_id: Option<Id>,
    pub name: String,
    pub token: String,
    pub mode: RouteMode,
    /// HTTP delivery target for incoming mail (simplification of Postal's
    /// polymorphic endpoints).
    pub endpoint_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RouteMode {
    Endpoint,
    Accept,
    Hold,
    Bounce,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Credential {
    pub id: Id,
    pub uuid: String,
    pub server_id: Id,
    pub credential_type: CredentialType,
    pub name: String,
    /// For SMTP/API credentials this is the secret key; for SMTP-IP
    /// credentials it is a CIDR (e.g. `1.0.0.0/8`).
    pub key: String,
    pub hold: bool,
    /// When the credential last authenticated a request (API-key auth on
    /// the per-server API, SMTP AUTH). `None` = never used.
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CredentialType {
    #[serde(rename = "SMTP")]
    Smtp,
    #[serde(rename = "API")]
    Api,
    #[serde(rename = "SMTP-IP")]
    SmtpIp,
}

/// A route joined with its server and domain name — what the SMTP session
/// needs in one lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRoute {
    pub route: Route,
    pub server: Server,
    /// The name of the route's domain (`route.domain.name` in Ruby)
    pub domain_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdminApiKey {
    pub id: Id,
    pub uuid: String,
    pub name: String,
    /// First few characters of the key, for display; never the full secret.
    pub key_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct User {
    pub id: Id,
    pub uuid: String,
    pub email_address: String,
    pub first_name: String,
    pub last_name: String,
    pub admin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IpPool {
    pub id: Id,
    pub uuid: String,
    pub name: String,
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IpAddress {
    pub id: Id,
    pub uuid: String,
    pub ip_pool_id: Id,
    pub ipv4: String,
    pub ipv6: Option<String>,
    pub hostname: String,
    pub priority: i32,
}

/// The webhook event names the worker can fire. The only valid values for
/// [`Webhook::events`]; API validation and the worker's filter both use
/// this list, so it is the single source of truth.
pub const WEBHOOK_EVENTS: [&str; 4] = [
    "MessageSent",
    "MessageDelayed",
    "MessageDeliveryFailed",
    "MessageHeld",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Webhook {
    pub id: Id,
    pub uuid: String,
    pub server_id: Id,
    pub name: String,
    pub url: String,
    pub all_events: bool,
    pub enabled: bool,
    pub sign: bool,
    /// Event names this webhook subscribes to (see [`WEBHOOK_EVENTS`]).
    /// Empty = all events (backwards compatible).
    pub events: Vec<String>,
    /// Extra HTTP headers set on every delivery request (e.g.
    /// `Authorization`). Values are secrets — never log them.
    pub headers: std::collections::BTreeMap<String, String>,
}

impl Webhook {
    /// Should this webhook fire for `event`? (Enabled + subscribed; an
    /// empty `events` list subscribes to everything.)
    pub fn subscribes_to(&self, event: &str) -> bool {
        self.enabled && (self.events.is_empty() || self.events.iter().any(|e| e == event))
    }
}

/// A verified single sender address of a server: authorizes the exact
/// From address even when its domain is not a verified sending domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SenderAddress {
    pub id: Id,
    pub uuid: String,
    pub server_id: Id,
    pub email_address: String,
    /// Confirmed via the emailed verification token.
    pub verified: bool,
    /// Hash of the outstanding verification token (cleared on confirm).
    #[serde(skip_serializing)]
    pub verification_token_hash: Option<String>,
}

/// A message stream — a flat label grouping mail for a server
/// (transactional / broadcast / inbound). A config record, filtered by
/// `server_id`; not RLS-protected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessageStream {
    pub id: Id,
    pub uuid: String,
    pub server_id: Id,
    pub name: String,
    pub permalink: String,
    pub stream_type: String,
    pub archived: bool,
}

/// A message template — named subject/html/text bodies rendered with a
/// per-send model. A config record, filtered by `server_id`; not
/// RLS-protected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Template {
    pub id: Id,
    pub uuid: String,
    pub server_id: Id,
    pub name: String,
    pub permalink: String,
    pub subject: Option<String>,
    pub html_body: Option<String>,
    pub text_body: Option<String>,
    pub archived: bool,
    /// Layout the rendered bodies are wrapped in (None = no layout).
    pub layout_id: Option<Id>,
}

/// A reusable layout: wrapper HTML (and optionally text) around a
/// template's rendered body — the place for logos, postal addresses and
/// social links that every mail shares. The HTML wrapper must embed the
/// body via raw interpolation (`{{{ content }}}`), so the rendered
/// template HTML is not escaped a second time. A config record, filtered
/// by `server_id`; not RLS-protected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layout {
    pub id: Id,
    pub uuid: String,
    pub server_id: Id,
    pub name: String,
    pub permalink: String,
    pub html_wrapper: String,
    pub text_wrapper: Option<String>,
}

/// A suppression-list entry. Tenant-scoped: lives under RLS in Postgres
/// (in the Ruby app this was a table in the per-server message database).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Suppression {
    pub id: Id,
    pub server_id: Id,
    #[serde(rename = "type")]
    pub suppression_type: String,
    pub address: String,
    pub reason: Option<String>,
}
