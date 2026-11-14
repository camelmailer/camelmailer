//! Configuration for Camelmailer.
//!
//! This is the Rust port of `lib/postal/config_schema.rb` + `lib/postal/config.rb`.
//! Configuration is loaded from a YAML file (path taken from
//! `CAMELMAILER_CONFIG_FILE_PATH`, falling back to `POSTAL_CONFIG_FILE_PATH`
//! for drop-in compatibility with existing deployments). Every key has the
//! same default as the Ruby schema, so an empty file is a valid configuration.
//!
//! The top-level group is named `camelmailer`, but the legacy `postal` group
//! name is accepted as an alias so existing `postal.yml` files keep working.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const ENV_CONFIG_FILE_PATH: &str = "CAMELMAILER_CONFIG_FILE_PATH";
pub const LEGACY_ENV_CONFIG_FILE_PATH: &str = "POSTAL_CONFIG_FILE_PATH";
pub const DEFAULT_CONFIG_FILE_PATH: &str = "config/camelmailer/camelmailer.yml";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not parse config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

fn default_true() -> bool {
    true
}

/// The `camelmailer` group (formerly `postal`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Camelmailer {
    /// The hostname that the Camelmailer web interface runs on
    pub web_hostname: String,
    /// The HTTP protocol to use for the Camelmailer web interface
    pub web_protocol: String,
    /// The hostname that the Camelmailer SMTP server runs on
    pub smtp_hostname: String,
    /// Should IP pools be enabled for this installation?
    pub use_ip_pools: bool,
    /// The maximum number of delivery attempts
    pub default_maximum_delivery_attempts: u32,
    /// The number of days to hold a message before they will be expired
    pub default_maximum_hold_expiry_days: u32,
    /// The number of days an address will remain in a suppression list before being removed
    pub default_suppression_list_automatic_removal_days: u32,
    /// The default threshold at which a message should be treated as spam
    pub default_spam_threshold: i32,
    /// The default threshold at which a message should be treated as spam failure
    pub default_spam_failure_threshold: i32,
    pub use_local_ns_for_domain_verification: bool,
    #[serde(default = "default_true")]
    pub use_resent_sender_header: bool,
    pub use_message_tags_for_bounces: bool,
    /// The default size for new DKIM keys
    pub default_dkim_key_size: u32,
    /// Path to the private key used for signing
    pub signing_key_path: String,
    /// Smarthosts to deliver every message through, tried in order, instead
    /// of looking up each recipient domain's MX records. Empty means
    /// direct-to-MX.
    ///
    /// `smtp://[user:password@]host[:port]` for plaintext with opportunistic
    /// STARTTLS, `smtps://…` for implicit TLS. The port defaults to 25
    /// (`smtp`) or 465 (`smtps`), and port 587 makes STARTTLS mandatory
    /// rather than opportunistic. The credentials are optional and go out as
    /// AUTH PLAIN once the connection is protected, never before;
    /// percent-encode any `@` or `:` inside them (`%40`, `%3A`). Certificate
    /// verification for relays follows `smtp.openssl_verify_mode`.
    ///
    /// An entry that does not parse is skipped, so a typo leaves that relay
    /// unused. See `docs/configuration.md` and `camelmailer-worker`'s
    /// `sender::parse_relay`.
    pub smtp_relays: Vec<String>,
    /// IP addresses to trust for proxying requests (in addition to localhost)
    pub trusted_proxies: Vec<String>,
    /// How long a queued message, webhook request or lifecycle event may
    /// stay locked before another worker takes it over, in days. Superseded
    /// by `queued_message_lock_stale_seconds` when that is set.
    pub queued_message_lock_stale_days: u32,
    /// The same lock lease in seconds. When set it replaces
    /// `queued_message_lock_stale_days`. A worker refreshes the locks it
    /// holds every 30 seconds while it works, so a live worker never loses a
    /// row however long its delivery takes; only the rows of a worker that
    /// died are taken over once the lease runs out. Values below 90 are
    /// raised to 90 (three heartbeats). Unset by default.
    pub queued_message_lock_stale_seconds: Option<u32>,
    #[serde(default = "default_true")]
    pub batch_queued_messages: bool,
    pub batch_queued_messages_limit: u32,
    /// The global API key for the Admin API. If not set, only database-backed
    /// admin API keys are accepted.
    pub admin_api_key: Option<String>,
    /// Guard outbound webhook / HTTP-route-endpoint requests against SSRF:
    /// resolve the destination host and refuse to send when it maps to a
    /// loopback, private, link-local, unique-local or otherwise non-global
    /// address. On by default. Turn it off only for a trusted, isolated
    /// self-hosted install that must target internal endpoints wholesale;
    /// prefer `outbound_allowed_hosts` for a narrow exception.
    #[serde(default = "default_true")]
    pub outbound_ssrf_protection: bool,
    /// Hosts (domains or IP literals) that bypass the SSRF guard even when
    /// they resolve to a non-global address — for self-hosters that
    /// deliberately deliver webhooks / routes to a known internal endpoint.
    #[serde(default)]
    pub outbound_allowed_hosts: Vec<String>,
    /// Deprecated alias of `retention.message_metadata_days`, used when that
    /// key is `0`. How many days to keep stored messages (and their
    /// deliveries, opens, clicks and tracking tokens). `0` keeps forever.
    #[serde(default)]
    pub message_retention_days: u32,
}

/// Default `worker.shutdown_grace_seconds`: below Docker's default stop
/// timeout of 10 s, so a worker without a configured `stop_grace_period`
/// still finishes its shutdown before it is killed.
pub const DEFAULT_SHUTDOWN_GRACE_SECONDS: u32 = 8;

/// The shortest lock lease a worker accepts: three heartbeats.
pub const MIN_QUEUE_LOCK_LEASE_SECONDS: u32 = 90;

impl Camelmailer {
    /// The effective lock lease in seconds: `queued_message_lock_stale_seconds`
    /// when set (at least [`MIN_QUEUE_LOCK_LEASE_SECONDS`]), otherwise
    /// `queued_message_lock_stale_days` (at least one day).
    pub fn queue_lock_lease_seconds(&self) -> u32 {
        match self.queued_message_lock_stale_seconds {
            Some(seconds) => seconds.max(MIN_QUEUE_LOCK_LEASE_SECONDS),
            None => self
                .queued_message_lock_stale_days
                .max(1)
                .saturating_mul(86_400),
        }
    }
}

impl Default for Camelmailer {
    fn default() -> Self {
        Self {
            web_hostname: "postal.example.com".into(),
            web_protocol: "https".into(),
            smtp_hostname: "postal.example.com".into(),
            use_ip_pools: false,
            default_maximum_delivery_attempts: 18,
            default_maximum_hold_expiry_days: 7,
            default_suppression_list_automatic_removal_days: 30,
            default_spam_threshold: 5,
            default_spam_failure_threshold: 20,
            use_local_ns_for_domain_verification: false,
            use_resent_sender_header: true,
            use_message_tags_for_bounces: false,
            default_dkim_key_size: 1024,
            signing_key_path: "$config-file-root/signing.key".into(),
            smtp_relays: vec![],
            trusted_proxies: vec![],
            queued_message_lock_stale_days: 1,
            queued_message_lock_stale_seconds: None,
            batch_queued_messages: true,
            batch_queued_messages_limit: 100,
            admin_api_key: None,
            outbound_ssrf_protection: true,
            outbound_allowed_hosts: vec![],
            message_retention_days: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebServer {
    pub default_port: u16,
    pub default_bind_address: String,
    pub max_threads: u32,
    /// Origins allowed to call the HTTP APIs from a browser. Empty (the
    /// default) sends no CORS headers; `["*"]` allows any origin.
    pub cors_origins: Vec<String>,
    /// On SIGTERM or SIGINT the web server stops accepting connections and
    /// gives requests in flight this many seconds to finish before it stops
    /// (env `WEB_SERVER_SHUTDOWN_GRACE_SECONDS`). Keep the process manager's
    /// stop timeout above it.
    pub shutdown_grace_seconds: u32,
}

impl Default for WebServer {
    fn default() -> Self {
        Self {
            default_port: 5000,
            default_bind_address: "127.0.0.1".into(),
            max_threads: 5,
            cors_origins: vec![],
            shutdown_grace_seconds: DEFAULT_SERVER_SHUTDOWN_GRACE_SECONDS,
        }
    }
}

/// The default grace period of the web and SMTP servers' graceful shutdown.
pub const DEFAULT_SERVER_SHUTDOWN_GRACE_SECONDS: u32 = 30;

/// `name` as a number of seconds when the variable is set: the override of a
/// `shutdown_grace_seconds` setting.
fn grace_override(
    var: impl Fn(&str) -> Option<String>,
    name: &str,
    current: u32,
) -> Result<u32, ConfigError> {
    match var(name) {
        None => Ok(current),
        Some(value) => value.trim().parse().map_err(|_| {
            ConfigError::Invalid(format!(
                "{name} must be a number of seconds (got {value:?})"
            ))
        }),
    }
}

impl WebServer {
    /// `shutdown_grace_seconds` with `WEB_SERVER_SHUTDOWN_GRACE_SECONDS`
    /// applied.
    pub fn shutdown_grace(&self) -> Result<std::time::Duration, ConfigError> {
        self.shutdown_grace_with(|name| std::env::var(name).ok())
    }

    fn shutdown_grace_with(
        &self,
        var: impl Fn(&str) -> Option<String>,
    ) -> Result<std::time::Duration, ConfigError> {
        let seconds = grace_override(
            var,
            "WEB_SERVER_SHUTDOWN_GRACE_SECONDS",
            self.shutdown_grace_seconds,
        )?;
        Ok(std::time::Duration::from_secs(u64::from(seconds)))
    }
}

/// The protocol spoken by a social sign-in provider (`auth.sso_providers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SsoProviderType {
    /// Standard OpenID Connect (Google, Microsoft, any spec-compliant IdP).
    Oidc,
    /// GitHub's OAuth2 flow (GitHub does not implement OIDC).
    Github,
}

/// One social sign-in provider (`auth.sso_providers`). Unlike the single
/// enterprise `oidc` group these are meant for public "Continue with …"
/// buttons; several can be configured side by side.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SsoProvider {
    /// URL-safe identifier; becomes part of the endpoints
    /// (`/api/v2/auth/sso/{id}/start` and `…/callback`). Lowercase
    /// letters, digits and hyphens only; must be unique.
    pub id: String,
    #[serde(rename = "type")]
    pub provider_type: SsoProviderType,
    /// Display name for the login/registration buttons.
    pub name: String,
    /// Issuer URL (`type: oidc` only);
    /// `{issuer}/.well-known/openid-configuration` must resolve.
    #[serde(default)]
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    /// When non-empty, only these email domains may sign in / provision.
    #[serde(default)]
    pub allowed_email_domains: Vec<String>,
    /// Create accounts on first sign-in. When false, only users that
    /// already exist (by email) may sign in via this provider.
    #[serde(default = "default_true")]
    pub auto_provision: bool,
}

/// User-account authentication (sessions, lockout, invitations).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Auth {
    /// Sliding session lifetime in days.
    pub session_timeout_days: u32,
    /// Consecutive failed logins before the account is locked.
    pub max_login_attempts: u32,
    /// How long a lockout lasts.
    pub lockout_minutes: u32,
    pub minimum_password_length: u32,
    /// May any signed-in user create an organization (becoming its owner)?
    /// When false only global admins can.
    pub allow_organization_creation: bool,
    /// May anyone create an account via `POST /api/v2/auth/register`?
    /// Off by default; meant for public cloud offerings. Self-hosters
    /// typically keep this off and use invitations or `make-user`.
    pub allow_registration: bool,
    /// Automatically create a starter workspace for brand-new accounts
    /// (self-registration and SSO auto-provisioning): an organization
    /// "<FirstName>'s Team" owned by the user, a server "production" and —
    /// for registration only, where the response can show it once — an
    /// API credential "default". Off by default; meant for the cloud.
    pub bootstrap_workspace: bool,
    pub invitation_expiry_days: u32,
    /// Password-reset link lifetime.
    pub password_reset_expiry_hours: u32,
    /// Base URL of the web frontend — used to build invitation/reset links
    /// and as the OIDC post-login redirect target.
    pub frontend_url: Option<String>,
    /// Social sign-in providers (Google, Microsoft, GitHub, …), each
    /// served under `/api/v2/auth/sso/{id}/…`. Independent of the single
    /// enterprise `oidc` group, which keeps working unchanged.
    pub sso_providers: Vec<SsoProvider>,
    /// WebAuthn / passkeys (see [`WebAuthn`]).
    pub webauthn: WebAuthn,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            session_timeout_days: 14,
            max_login_attempts: 5,
            lockout_minutes: 15,
            minimum_password_length: 8,
            allow_organization_creation: true,
            allow_registration: false,
            bootstrap_workspace: false,
            invitation_expiry_days: 7,
            password_reset_expiry_hours: 2,
            frontend_url: None,
            sso_providers: vec![],
            webauthn: WebAuthn::default(),
        }
    }
}

/// WebAuthn / passkeys (`auth.webauthn`). When enabled, signed-in users
/// can register passkeys and anyone can sign in with one
/// (`/api/v2/auth/webauthn/*`); while disabled those endpoints answer
/// `403 WebAuthnDisabled`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebAuthn {
    pub enabled: bool,
    /// The relying-party id — the domain the passkeys are scoped to,
    /// e.g. `app.camelmailer.com`. Changing it invalidates every
    /// registered passkey.
    pub rp_id: String,
    /// The exact web origin the browser reports during ceremonies,
    /// e.g. `https://app.camelmailer.com`.
    pub rp_origin: String,
    /// Display name shown by browsers/authenticators during registration.
    pub rp_name: String,
}

impl Default for WebAuthn {
    fn default() -> Self {
        Self {
            enabled: false,
            rp_id: String::new(),
            rp_origin: String::new(),
            rp_name: "Camelmailer".into(),
        }
    }
}

/// Platform email delivery ("app mail") — the installation sends its own
/// account emails (password resets, invitations, welcome mail) through its
/// own sending pipeline, via a mail server of this installation.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppMail {
    pub enabled: bool,
    /// API credential (`X-Server-API-Key`) of a mail server of THIS
    /// installation through which platform mail is sent.
    pub server_api_key: Option<String>,
    /// From address for platform mail; its domain must be a verified
    /// sending domain of the configured server.
    pub from_address: Option<String>,
    /// Display name used with `from_address`.
    pub from_name: String,
}

impl Default for AppMail {
    fn default() -> Self {
        Self {
            enabled: false,
            server_api_key: None,
            from_address: None,
            from_name: "Camelmailer".into(),
        }
    }
}

/// Billing, provider neutral. The open-source build ships no billing
/// provider: a custom binary built on the `camelmailer` library crate plugs
/// one in (see `camelmailer::Extensions`). Self-hosted installations keep
/// the default (`enabled: false`), and so does an installation with
/// `enabled: true` but no provider compiled in: no billing endpoint is
/// active and the dashboard shows usage only.
#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Billing {
    pub enabled: bool,
    /// Free-form settings for the billing provider of a custom build (API
    /// base URL, credentials, ...). The open-source build never reads them.
    /// Values are redacted from `Debug` output.
    pub options: std::collections::BTreeMap<String, serde_yaml::Value>,
    /// Removed in 0.10 together with the built-in Stripe integration.
    /// Still accepted so an existing configuration file keeps loading; a
    /// value only produces a deprecation warning at startup.
    #[serde(rename = "stripe_secret_key")]
    pub legacy_stripe_secret_key: Option<String>,
    /// Removed in 0.10, see `stripe_secret_key`.
    #[serde(rename = "portal_return_url")]
    pub legacy_portal_return_url: Option<String>,
}

/// Provider options may hold secrets, so `Debug` lists their keys only.
impl std::fmt::Debug for Billing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Billing")
            .field("enabled", &self.enabled)
            .field("options", &self.options.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Installation-wide data retention defaults, in days; `0` keeps forever,
/// which is the default for every class except the API request log (30, as
/// before). Organizations can override the per-tenant classes through the
/// admin API. Expired authentication tokens (sessions, password resets,
/// invitations, share links, SSO and passkey ceremony state) are always
/// deleted at expiry, independent of this group.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Retention {
    /// Raw MIME of stored messages (outgoing and incoming). Capped at
    /// `message_metadata_days` when that is set.
    pub message_content_days: u32,
    /// Message rows, deliveries, opens, clicks, links, tracking tokens.
    /// Falls back to `camelmailer.message_retention_days` when `0`.
    pub message_metadata_days: u32,
    /// The webhook delivery log.
    pub webhook_log_days: u32,
    /// Received DMARC aggregate reports.
    pub dmarc_report_days: u32,
    /// The authentication audit log (`auth_events`). Installation-wide.
    pub auth_event_days: u32,
    /// The per-server API request log. Installation-wide; default 30.
    pub api_request_days: u32,
    /// Raw ARF feedback reports (complaints from JMRP, CFL and other
    /// feedback loops). They contain the complaining recipient's address,
    /// so their content is removed after this many days; the complaint, its
    /// suppression and the provider counts stay. Installation-wide; default
    /// 30, `0` keeps them as long as the message content.
    pub feedback_report_days: u32,
    /// Rows deleted per statement, so a large backlog never holds one long
    /// transaction or lock.
    pub batch_size: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            message_content_days: 0,
            message_metadata_days: 0,
            webhook_log_days: 0,
            dmarc_report_days: 0,
            auth_event_days: 0,
            api_request_days: 30,
            feedback_report_days: 30,
            batch_size: 1000,
        }
    }
}

/// Provider feedback (`deliverability_feedback`): what the large mailbox
/// providers report about this installation's mail. Every source is optional
/// and switched on only by its credentials; a source without them is
/// disabled, never an error. Secrets are never written into this file: they
/// come from environment variables or from files (Docker or systemd secrets)
/// named here. See `docs/provider-feedback.md`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeliverabilityFeedback {
    /// How often a worker fetches the Postmaster Tools and SNDS data, in
    /// seconds. Both publish once a day; the default is six hours. `0`
    /// turns the scheduled fetch off. Failures back off exponentially up to
    /// this interval.
    pub fetch_interval_seconds: u32,
    /// How long fetched snapshots and feedback-loop counts are kept.
    pub snapshot_retention_days: u32,
    pub postmaster_tools: PostmasterTools,
    pub snds: Snds,
    pub fbl: FeedbackLoops,
    pub placement: Placement,
}

impl Default for DeliverabilityFeedback {
    fn default() -> Self {
        Self {
            fetch_interval_seconds: 21_600,
            snapshot_retention_days: 400,
            postmaster_tools: PostmasterTools::default(),
            snds: Snds::default(),
            fbl: FeedbackLoops::default(),
            placement: Placement::default(),
        }
    }
}

/// Google Postmaster Tools API v2
/// (https://developers.google.com/workspace/gmail/postmaster/reference/rest).
/// Authorised as a Google account that has the domains verified in
/// Postmaster Tools, through an OAuth client and a refresh token with the
/// `https://www.googleapis.com/auth/postmaster.traffic.readonly` scope.
/// Enabled when `domains`, the client id, the client secret and the refresh
/// token are all present.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PostmasterTools {
    /// Domains registered and verified in Postmaster Tools.
    pub domains: Vec<String>,
    /// OAuth client id (not secret). Env `CAMELMAILER_POSTMASTER_CLIENT_ID`
    /// wins.
    pub client_id: Option<String>,
    /// File holding the OAuth client secret. Env
    /// `CAMELMAILER_POSTMASTER_CLIENT_SECRET` wins.
    pub client_secret_file: Option<String>,
    /// File holding the refresh token. Env
    /// `CAMELMAILER_POSTMASTER_REFRESH_TOKEN` wins.
    pub refresh_token_file: Option<String>,
    /// How many past days each fetch asks for.
    pub days: u32,
    /// API base URL (tests point it at a mock).
    pub api_base_url: String,
    /// OAuth token endpoint.
    pub token_url: String,
}

impl Default for PostmasterTools {
    fn default() -> Self {
        Self {
            domains: Vec::new(),
            client_id: None,
            client_secret_file: None,
            refresh_token_file: None,
            days: 7,
            api_base_url: "https://gmailpostmastertools.googleapis.com".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
        }
    }
}

/// Microsoft SNDS automated data access
/// (https://substrate.office.com/ip-domain-management-snds/SNDS/FAQ). The
/// SNDS portal shows the data URL, which carries the access key, on its
/// "Automated Access" page once the feature is switched on. The URL is a
/// secret. Enabled when it is present.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Snds {
    /// File holding the SNDS data URL. Env `CAMELMAILER_SNDS_DATA_URL` wins.
    pub data_url_file: Option<String>,
}

/// Inbox placement tests with seed mailboxes the operator owns. A test
/// message per sending IP goes to every seed; the worker then looks it up
/// over IMAP, read-only, and records inbox, spam or missing. It only
/// measures: it never opens, clicks, replies to, flags or moves a message.
/// Off while `seeds` is empty.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Placement {
    /// The mail server that sends the tests, as `organization/server`
    /// permalinks. `from_address` must be on one of its verified domains,
    /// so the tests carry its DKIM signature.
    pub server: String,
    /// Envelope and header sender of the tests.
    pub from_address: String,
    /// Run a round every this many seconds. `0`: only on demand from the
    /// dashboard or `POST /api/v2/admin/deliverability/placement_tests`.
    pub interval_seconds: u32,
    /// At most one round per this many seconds, on demand included.
    pub min_round_interval_seconds: u32,
    /// How long after sending to look for a test, and between lookups.
    pub check_after_seconds: u32,
    /// Lookups before a test that was not found counts as missing.
    pub max_checks: u32,
    pub seeds: Vec<Seed>,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            server: String::new(),
            from_address: String::new(),
            interval_seconds: 0,
            min_round_interval_seconds: 3600,
            check_after_seconds: 600,
            max_checks: 3,
            seeds: Vec::new(),
        }
    }
}

/// The mailbox provider of a seed, which sets the IMAP defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeedProvider {
    /// Gmail: imap.gmail.com:993, XOAUTH2 (scope https://mail.google.com/)
    /// or an app password.
    Gmail,
    /// Outlook.com / Hotmail: outlook.office365.com:993, XOAUTH2 only
    /// (basic authentication ended 2024-09-16).
    Outlook,
    /// Yahoo Mail: imap.mail.yahoo.com:993 with an app password.
    Yahoo,
    /// Any other IMAP server; `imap_host` is required.
    #[default]
    Imap,
}

/// How a seed logs in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedAuth {
    /// `LOGIN` with an app password (Gmail with 2-Step Verification, Yahoo).
    #[default]
    AppPassword,
    /// `AUTHENTICATE XOAUTH2` with an access token from a refresh token.
    Xoauth2,
}

/// One seed mailbox. Secrets come from the environment variable or the
/// file named here, never from this file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Seed {
    pub provider: SeedProvider,
    /// The seed's address (the test's recipient).
    pub address: String,
    /// IMAP login; defaults to `address`.
    pub username: Option<String>,
    pub imap_host: Option<String>,
    pub imap_port: Option<u16>,
    /// TLS from the first byte (993). Only tests turn it off.
    pub imap_tls: Option<bool>,
    pub auth: SeedAuth,
    /// App password: environment variable, else file.
    pub password_env: Option<String>,
    pub password_file: Option<String>,
    /// XOAUTH2: token endpoint (provider default when unset), client id,
    /// client secret and refresh token (each from env, else file).
    pub token_url: Option<String>,
    pub client_id: Option<String>,
    pub client_secret_env: Option<String>,
    pub client_secret_file: Option<String>,
    pub refresh_token_env: Option<String>,
    pub refresh_token_file: Option<String>,
    /// OAuth scope (provider default when unset).
    pub scope: Option<String>,
    /// The spam folder, when the server does not mark it `\Junk`.
    pub junk_folder: Option<String>,
    /// The provider group the seed measures (lowercase letters, digits and
    /// `_`, as in `provider_groups`). Defaults to `google` for gmail,
    /// `microsoft` for outlook, `yahoo` for yahoo and `other` for imap; set
    /// it for an `imap` seed at, say, GMX (`united_internet`), T-Online
    /// (`t_online`) or iCloud (`apple`).
    pub group: Option<String>,
}

/// Complaint feedback loops (Microsoft JMRP, Yahoo CFL and any other ARF
/// feedback loop). Reports reach the installation as inbound mail to a
/// route address; they are recognised by their ARF envelope (RFC 5965).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeedbackLoops {
    /// Also suppress the complainant when the complaint is about
    /// transactional mail (a message without a broadcast unsubscribe token),
    /// for the message's stream. On by default; each mail server can choose
    /// otherwise in its settings. Broadcast complaints are always suppressed
    /// for their stream.
    pub suppress_complainants: bool,
}

impl Default for FeedbackLoops {
    fn default() -> Self {
        Self {
            suppress_complainants: true,
        }
    }
}

/// Installation lifecycle webhooks: organization created/deleted, user
/// registered, organization usage thresholds. Off while `urls` is empty
/// (the default). Every request is signed with HMAC-SHA256 over
/// `"{timestamp}.{body}"` using `secret`.
#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleWebhooks {
    /// Endpoints that receive every event (POST, JSON).
    pub urls: Vec<String>,
    /// Shared HMAC secret; required when `urls` is not empty.
    pub secret: Option<String>,
}

impl LifecycleWebhooks {
    pub fn enabled(&self) -> bool {
        !self.urls.is_empty()
    }
}

impl std::fmt::Debug for LifecycleWebhooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleWebhooks")
            .field("urls", &self.urls)
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// Which captcha protects self-registration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptchaProvider {
    #[default]
    None,
    /// Cap (https://trycap.dev), or any service with the same siteverify
    /// contract.
    Cap,
}

impl CaptchaProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            CaptchaProvider::None => "none",
            CaptchaProvider::Cap => "cap",
        }
    }
}

/// Captcha at `POST /api/v2/auth/register`. Off by default.
#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Captcha {
    pub provider: CaptchaProvider,
    /// The public endpoint including the site key, e.g.
    /// `https://captcha.example.com/<site_key>/`. The widget talks to it,
    /// and the server verifies at `{endpoint}siteverify`.
    pub endpoint: Option<String>,
    /// The key secret used for server-side verification. Never sent to a
    /// browser, redacted from `Debug`.
    pub secret: Option<String>,
}

impl Captcha {
    pub fn enabled(&self) -> bool {
        self.provider != CaptchaProvider::None
    }

    /// The endpoint with exactly one trailing slash.
    pub fn normalized_endpoint(&self) -> Option<String> {
        self.endpoint
            .as_deref()
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
            .map(|endpoint| format!("{}/", endpoint.trim_end_matches('/')))
    }

    /// `{endpoint}siteverify`.
    pub fn siteverify_url(&self) -> Option<String> {
        self.normalized_endpoint()
            .map(|endpoint| format!("{endpoint}siteverify"))
    }
}

impl std::fmt::Debug for Captcha {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Captcha")
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// Links to the hosted service's legal pages, shown on the sign-in and
/// registration cards. Empty by default: self-hosted installations show no
/// legal links, while the managed cloud points these at its Terms and
/// Privacy pages.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Legal {
    pub terms_url: Option<String>,
    pub privacy_url: Option<String>,
}

/// OpenID Connect single sign-on. Field names match the upstream Postal
/// `oidc` group so a legacy `postal.yml` loads unchanged.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Oidc {
    pub enabled: bool,
    /// Display name for the identity provider (shown on the login page).
    pub name: String,
    /// Issuer URL; `{issuer}/.well-known/openid-configuration` must resolve.
    pub issuer: String,
    /// OAuth client id (Postal calls this `identifier`).
    pub identifier: Option<String>,
    pub secret: Option<String>,
    pub scopes: Vec<String>,
    /// Claim used as the stable account link (Postal: `uid_field`).
    pub uid_field: String,
    pub email_address_field: String,
    pub name_field: String,
    /// Use OIDC discovery (the only supported mode; present for
    /// config-file compatibility).
    pub discovery: bool,
    /// Create accounts on first SSO login. When false, only users that
    /// already exist (by email) may sign in via SSO.
    pub auto_provision: bool,
    /// When non-empty, only these email domains may sign in / provision.
    pub allowed_email_domains: Vec<String>,
}

impl Default for Oidc {
    fn default() -> Self {
        Self {
            enabled: false,
            name: "OIDC".into(),
            issuer: String::new(),
            identifier: None,
            secret: None,
            scopes: vec!["openid".into(), "email".into(), "profile".into()],
            uid_field: "sub".into(),
            email_address_field: "email".into(),
            name_field: "name".into(),
            discovery: true,
            auto_provision: true,
            allowed_email_domains: vec![],
        }
    }
}

/// SAML 2.0 single sign-on (service-provider role). Camelmailer speaks
/// the HTTP-Redirect binding for the AuthnRequest and the HTTP-POST
/// binding for the response; assertions must be signed with the
/// configured IdP certificate.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Saml {
    pub enabled: bool,
    /// Display name for the identity provider (shown on the login page).
    pub name: String,
    /// The IdP single-sign-on URL the AuthnRequest is redirected to
    /// (`SingleSignOnService` with the HTTP-Redirect binding).
    pub idp_sso_url: String,
    /// The IdP signing certificate: either inline PEM (starts with
    /// `-----BEGIN CERTIFICATE-----`) or a path to a PEM file.
    pub idp_certificate: Option<String>,
    /// Our SP entity id. Defaults to
    /// `{camelmailer.web_protocol}://{camelmailer.web_hostname}`.
    pub sp_entity_id: Option<String>,
    /// Create accounts on first SSO login. When false, only users that
    /// already exist (by email) may sign in via SAML.
    pub auto_provision: bool,
    /// When non-empty, only these email domains may sign in / provision.
    pub allowed_email_domains: Vec<String>,
}

impl Default for Saml {
    fn default() -> Self {
        Self {
            enabled: false,
            name: "SAML".into(),
            idp_sso_url: String::new(),
            idp_certificate: None,
            sp_entity_id: None,
            auto_provision: true,
            allowed_email_domains: vec![],
        }
    }
}

/// SCIM 2.0 provisioning (RFC 7643/7644) under `/scim/v2`, authenticated
/// with a static bearer token.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scim {
    pub enabled: bool,
    /// The bearer token IdPs present as `Authorization: Bearer <token>`.
    /// Required when enabled; compared in constant time.
    pub bearer_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Worker {
    pub default_health_server_port: u16,
    pub default_health_server_bind_address: String,
    /// Messages one worker process delivers at the same time.
    pub threads: u32,
    /// Serve only these servers, as `organization/server` permalinks. Empty
    /// serves every server. For a worker dedicated to some tenants.
    pub servers: Vec<String>,
    /// Never serve these servers (`organization/server`), for example the
    /// ones a dedicated worker already serves.
    pub exclude_servers: Vec<String>,
    /// Which mail this worker delivers: `all`, `transactional` or `bulk`
    /// (broadcast streams and campaigns).
    pub deliver: WorkerDeliver,
    /// Choose the source address among the pool addresses this host holds,
    /// so workers on several hosts can share a pool, each sending from its
    /// own addresses. Off: every worker uses the pool's first address, as
    /// before.
    pub host_aware_pools: bool,
    /// With `host_aware_pools`: the addresses this host may send from. Empty
    /// takes the addresses configured on this host's network interfaces,
    /// re-read every 30 s.
    /// List them where an address is configured on the interface but routed
    /// to another host.
    pub source_addresses: Vec<String>,
    /// With `host_aware_pools`: how long a message whose pool has no address
    /// on this host is left for the other hosts before this worker looks at
    /// it again. No attempt is counted. At least 1 (env
    /// `WORKER_FOREIGN_POOL_DELAY_SECONDS`).
    pub foreign_pool_delay_seconds: u32,
    /// On SIGTERM or SIGINT the worker stops taking new work and waits this
    /// many seconds for the deliveries it has in flight. Whatever is still
    /// running then is stopped: rows that had not reached a receiver go back
    /// to the queue without counting an attempt, the rest keep their lock
    /// until the lease runs out. Keep it below the stop timeout of the
    /// process manager (Docker: `stop_grace_period`, default 10 s), which is
    /// why the default is 8: the worker then finishes its shutdown before
    /// Docker would kill it, even with no stop timeout configured.
    pub shutdown_grace_seconds: u32,
}

impl Default for Worker {
    fn default() -> Self {
        Self {
            default_health_server_port: 9090,
            default_health_server_bind_address: "127.0.0.1".into(),
            threads: 2,
            servers: Vec::new(),
            exclude_servers: Vec::new(),
            deliver: WorkerDeliver::All,
            host_aware_pools: false,
            source_addresses: Vec::new(),
            foreign_pool_delay_seconds: 30,
            shutdown_grace_seconds: DEFAULT_SHUTDOWN_GRACE_SECONDS,
        }
    }
}

/// How this process appears in the cluster topology (`cluster`): every
/// web, SMTP and worker process registers itself under `node_name` and
/// refreshes its entry every `heartbeat_interval_seconds`. The other values
/// describe the installation for the infrastructure dashboard and its DNS
/// checks; nothing here changes how mail is received or delivered.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cluster {
    /// The name this host appears under. Empty takes the host name (env
    /// `CAMELMAILER_NODE_NAME`).
    pub node_name: String,
    /// The public address of the load balancer in front of the SMTP and
    /// web nodes, if there is one (for example `2.31.154.200`).
    pub load_balancer_address: Option<String>,
    /// The host name MX records point at, whose A/AAAA records lead to the
    /// load balancer (or to the SMTP nodes without one), for example
    /// `mail.example.com`.
    pub inbound_hostname: Option<String>,
    /// This host's public addresses, the ones receivers see for servers
    /// without an IP pool. Listed, they replace the addresses the worker
    /// detects for the reverse-DNS check, per family (a family not listed is
    /// still detected). Needed where the process only sees a private address,
    /// such as `172.18.0.5` inside a Docker network (env
    /// `CAMELMAILER_PUBLIC_ADDRESSES`, comma separated).
    pub public_addresses: Vec<String>,
    /// How often a process refreshes its entry. At least 5.
    pub heartbeat_interval_seconds: u32,
    /// An entry not refreshed for this long shows as stale. At least twice
    /// the heartbeat interval.
    pub node_stale_seconds: u32,
}

impl Default for Cluster {
    fn default() -> Self {
        Self {
            node_name: String::new(),
            load_balancer_address: None,
            inbound_hostname: None,
            public_addresses: Vec::new(),
            heartbeat_interval_seconds: 30,
            node_stale_seconds: 90,
        }
    }
}

impl Cluster {
    /// The cluster settings with `CAMELMAILER_NODE_NAME` and
    /// `CAMELMAILER_PUBLIC_ADDRESSES` applied, so one config file can serve
    /// several hosts.
    pub fn with_env_overrides(&self) -> Result<Self, ConfigError> {
        self.with_overrides(|name| std::env::var(name).ok())
    }

    fn with_overrides(&self, var: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut cluster = self.clone();
        if let Some(value) = var("CAMELMAILER_NODE_NAME") {
            cluster.node_name = value.trim().to_string();
        }
        if let Some(value) = var("CAMELMAILER_PUBLIC_ADDRESSES") {
            cluster.public_addresses = value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(String::from)
                .collect();
        }
        cluster.validate()?;
        Ok(cluster)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for address in &self.public_addresses {
            if address.parse::<std::net::IpAddr>().is_err() {
                return Err(ConfigError::Invalid(format!(
                    "cluster.public_addresses takes IP addresses (got {address:?})"
                )));
            }
        }
        if let Some(address) = self.load_balancer_address.as_deref() {
            if !address.is_empty() && address.parse::<std::net::IpAddr>().is_err() {
                return Err(ConfigError::Invalid(format!(
                    "cluster.load_balancer_address takes an IP address (got {address:?})"
                )));
            }
        }
        if let Some(name) = self.inbound_hostname.as_deref() {
            if !name.is_empty() && !is_host_name(name) {
                return Err(ConfigError::Invalid(format!(
                    "cluster.inbound_hostname must be a fully qualified host name (got {name:?})"
                )));
            }
        }
        if self.heartbeat_interval_seconds < 5 {
            return Err(ConfigError::Invalid(
                "cluster.heartbeat_interval_seconds must be at least 5".into(),
            ));
        }
        if self.node_stale_seconds < self.heartbeat_interval_seconds.saturating_mul(2) {
            return Err(ConfigError::Invalid(
                "cluster.node_stale_seconds must be at least twice cluster.heartbeat_interval_seconds".into(),
            ));
        }
        Ok(())
    }

    /// The parsed `public_addresses`.
    pub fn public_ips(&self) -> Vec<std::net::IpAddr> {
        self.public_addresses
            .iter()
            .filter_map(|address| address.trim().parse().ok())
            .collect()
    }

    /// `node_name`, or the host name when it is empty.
    pub fn effective_node_name(&self) -> String {
        if !self.node_name.trim().is_empty() {
            return self.node_name.trim().to_string();
        }
        std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "camelmailer".into())
    }

    /// `load_balancer_address` when set and not empty.
    pub fn load_balancer(&self) -> Option<&str> {
        self.load_balancer_address
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// `inbound_hostname` when set and not empty, lower-cased.
    pub fn inbound_host(&self) -> Option<String> {
        self.inbound_hostname
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase)
    }
}

/// A fully qualified host name (the same rule as
/// `camelmailer_core::dns::is_valid_helo_hostname`, which this crate cannot
/// depend on): at least two labels of letters, digits and
/// hyphens, no trailing dot, no IP literal.
fn is_host_name(name: &str) -> bool {
    if name.len() > 253 || name.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    let labels: Vec<&str> = name.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && !labels
            .last()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
}

/// Which mail a worker delivers (`worker.deliver`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerDeliver {
    /// Everything, transactional before bulk within each server.
    #[default]
    All,
    /// Transactional streams and inbound mail, never bulk.
    Transactional,
    /// Only bulk mail: broadcast streams and campaigns.
    Bulk,
}

impl Worker {
    /// The worker settings with the `WORKER_THREADS`, `WORKER_SERVERS`,
    /// `WORKER_EXCLUDE_SERVERS` and `WORKER_DELIVER` environment variables
    /// applied, so one config file can start differently scoped worker
    /// containers. Lists are comma separated.
    pub fn with_env_overrides(&self) -> Result<Self, ConfigError> {
        self.with_overrides(|name| std::env::var(name).ok())
    }

    fn with_overrides(&self, var: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let list = |value: String| -> Vec<String> {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(String::from)
                .collect()
        };
        let mut worker = self.clone();
        if let Some(value) = var("WORKER_THREADS") {
            worker.threads = value.trim().parse().map_err(|_| {
                ConfigError::Invalid(format!("WORKER_THREADS must be a number (got {value:?})"))
            })?;
        }
        if let Some(value) = var("WORKER_SERVERS") {
            worker.servers = list(value);
        }
        if let Some(value) = var("WORKER_EXCLUDE_SERVERS") {
            worker.exclude_servers = list(value);
        }
        if let Some(value) = var("WORKER_HOST_AWARE_POOLS") {
            worker.host_aware_pools = match value.trim() {
                "true" | "1" => true,
                "false" | "0" | "" => false,
                other => {
                    return Err(ConfigError::Invalid(format!(
                        "WORKER_HOST_AWARE_POOLS must be true or false (got {other:?})"
                    )))
                }
            };
        }
        if let Some(value) = var("WORKER_SOURCE_ADDRESSES") {
            worker.source_addresses = list(value);
        }
        if let Some(value) = var("WORKER_FOREIGN_POOL_DELAY_SECONDS") {
            worker.foreign_pool_delay_seconds = value.trim().parse().map_err(|_| {
                ConfigError::Invalid(format!(
                    "WORKER_FOREIGN_POOL_DELAY_SECONDS must be a number (got {value:?})"
                ))
            })?;
        }
        if let Some(value) = var("WORKER_DELIVER") {
            worker.deliver = match value.trim() {
                "all" => WorkerDeliver::All,
                "transactional" => WorkerDeliver::Transactional,
                "bulk" => WorkerDeliver::Bulk,
                other => {
                    return Err(ConfigError::Invalid(format!(
                        "WORKER_DELIVER must be all, transactional or bulk (got {other:?})"
                    )))
                }
            };
        }
        worker.validate()?;
        Ok(worker)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for address in &self.source_addresses {
            if address.parse::<std::net::IpAddr>().is_err() {
                return Err(ConfigError::Invalid(format!(
                    "worker.source_addresses takes IP addresses (got {address:?})"
                )));
            }
        }
        if self.threads == 0 {
            return Err(ConfigError::Invalid(
                "worker.threads must be at least 1".into(),
            ));
        }
        if self.foreign_pool_delay_seconds == 0 {
            return Err(ConfigError::Invalid(
                "worker.foreign_pool_delay_seconds must be at least 1".into(),
            ));
        }
        for entry in self.servers.iter().chain(&self.exclude_servers) {
            if entry
                .split_once('/')
                .is_none_or(|(org, server)| org.is_empty() || server.is_empty())
            {
                return Err(ConfigError::Invalid(format!(
                    "worker.servers and worker.exclude_servers take organization/server \
                     permalinks (got {entry:?})"
                )));
            }
        }
        if let Some(both) = self
            .servers
            .iter()
            .find(|entry| self.exclude_servers.contains(entry))
        {
            return Err(ConfigError::Invalid(format!(
                "{both:?} is in worker.servers and worker.exclude_servers"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MainDb {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub database: String,
    pub pool_size: u32,
    pub encoding: String,
}

impl Default for MainDb {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 3306,
            username: "postal".into(),
            password: None,
            database: "postal".into(),
            pool_size: 5,
            encoding: "utf8mb4".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MessageDb {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub encoding: String,
    pub database_name_prefix: String,
}

impl Default for MessageDb {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 3306,
            username: "postal".into(),
            password: None,
            encoding: "utf8mb4".into(),
            database_name_prefix: "postal".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    pub rails_log_enabled: bool,
    pub sentry_dsn: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub highlighting_enabled: bool,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            rails_log_enabled: false,
            sentry_dsn: None,
            enabled: true,
            highlighting_enabled: false,
        }
    }
}

/// The TLS mode of an additional SMTP listener.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpListenerMode {
    /// Plaintext with optional STARTTLS — the behaviour of `default_port`.
    #[default]
    Smtp,
    /// Implicit TLS from the first byte (the classic port 465).
    Smtps,
}

/// An additional SMTP listener (`smtp_server.listeners`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpListener {
    pub port: u16,
    #[serde(default)]
    pub mode: SmtpListenerMode,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmtpServer {
    pub default_port: u16,
    pub default_bind_address: String,
    pub default_health_server_port: u16,
    pub default_health_server_bind_address: String,
    pub tls_enabled: bool,
    /// Whether AUTH requires a TLS-protected session. Leave this on.
    ///
    /// It exists for one situation: turning `tls_enabled` on for an
    /// installation whose clients already authenticate in the clear. The
    /// moment TLS becomes available, AUTH starts requiring it, and a client
    /// configured without STARTTLS fails at that instant. Setting this to
    /// `false` offers STARTTLS while still accepting an unprotected AUTH, so
    /// clients can upgrade on their own schedule. Every such AUTH is logged
    /// at warn level with the credential and the client address, so the
    /// window can be closed once the log falls quiet.
    ///
    /// Has no effect while `tls_enabled` is off, because there is no TLS to
    /// require.
    pub auth_requires_tls: bool,
    pub tls_certificate_path: String,
    pub tls_private_key_path: String,
    pub tls_ciphers: Option<String>,
    pub ssl_version: String,
    pub proxy_protocol: bool,
    pub log_connections: bool,
    /// The maximum message size to accept from the SMTP server (in MB)
    pub max_message_size: u64,
    /// A regular expression used to exclude connections from logging
    pub log_ip_address_exclusion_matcher: Option<String>,
    /// Additional listeners beyond `default_port` (which always runs in
    /// `smtp` mode). Empty by default: only `default_port` is bound.
    pub listeners: Vec<SmtpListener>,
    /// On SIGTERM or SIGINT the SMTP server stops accepting connections and
    /// gives open sessions this many seconds to finish their transaction;
    /// sessions still open then get `421 4.3.2` and are closed (env
    /// `SMTP_SERVER_SHUTDOWN_GRACE_SECONDS`). Keep the process manager's stop
    /// timeout above it.
    pub shutdown_grace_seconds: u32,
}

impl Default for SmtpServer {
    fn default() -> Self {
        Self {
            default_port: 25,
            default_bind_address: "::".into(),
            default_health_server_port: 9091,
            default_health_server_bind_address: "127.0.0.1".into(),
            tls_enabled: false,
            auth_requires_tls: true,
            tls_certificate_path: "$config-file-root/smtp.cert".into(),
            tls_private_key_path: "$config-file-root/smtp.key".into(),
            tls_ciphers: None,
            ssl_version: "SSLv23".into(),
            proxy_protocol: false,
            log_connections: false,
            max_message_size: 14,
            log_ip_address_exclusion_matcher: None,
            listeners: vec![],
            shutdown_grace_seconds: DEFAULT_SERVER_SHUTDOWN_GRACE_SECONDS,
        }
    }
}

impl SmtpServer {
    /// `shutdown_grace_seconds` with `SMTP_SERVER_SHUTDOWN_GRACE_SECONDS`
    /// applied.
    pub fn shutdown_grace(&self) -> Result<std::time::Duration, ConfigError> {
        self.shutdown_grace_with(|name| std::env::var(name).ok())
    }

    fn shutdown_grace_with(
        &self,
        var: impl Fn(&str) -> Option<String>,
    ) -> Result<std::time::Duration, ConfigError> {
        let seconds = grace_override(
            var,
            "SMTP_SERVER_SHUTDOWN_GRACE_SECONDS",
            self.shutdown_grace_seconds,
        )?;
        Ok(std::time::Duration::from_secs(u64::from(seconds)))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dns {
    pub mx_records: Vec<String>,
    pub spf_include: String,
    pub return_path_domain: String,
    /// Opt in to rewriting outbound envelope senders; defaults off in 0.7.x.
    pub return_path_envelope: bool,
    pub route_domain: String,
    pub track_domain: String,
    pub helo_hostname: Option<String>,
    pub dkim_identifier: String,
    pub domain_verify_prefix: String,
    /// DNS name label of the domain-ownership challenge record: the TXT is
    /// looked up at `<verification_record_label>.<domain>`. Brandable — an
    /// installation can set e.g. `_revenexxmailer-challenge`.
    pub verification_record_label: String,
    /// Prefix of the challenge TXT value: the record reads
    /// `<verification_value_prefix>=<token>` (e.g. `revenexx-verification`).
    pub verification_value_prefix: String,
    pub custom_return_path_prefix: String,
    pub timeout: u32,
    pub resolv_conf_path: String,
    /// The public DNS resolvers every DNS check asks (domain verification,
    /// the domain health check, the infrastructure DNS checks and the
    /// workers' reverse-DNS check): IP addresses, optionally with a port
    /// (`9.9.9.9`, `9.9.9.9:53`, `[2620:fe::fe]:53`). The checks never use
    /// the host's or container's resolver, which can answer from local
    /// configuration (the host name, `/etc/hosts`, a split-horizon zone)
    /// instead of what the rest of the internet sees. Delivery and inbound
    /// SPF keep using the system resolver.
    pub check_resolvers: Vec<String>,
}

/// The public resolvers `dns.check_resolvers` defaults to: Cloudflare and
/// Quad9.
pub const DEFAULT_CHECK_RESOLVERS: [&str; 2] = ["1.1.1.1", "9.9.9.9"];

impl Dns {
    /// `dns.check_resolvers` as socket addresses (port 53 unless given).
    /// Entries that do not parse are left out; `Config::validate` refuses
    /// them at start.
    pub fn check_resolver_addresses(&self) -> Vec<std::net::SocketAddr> {
        self.check_resolvers
            .iter()
            .filter_map(|entry| parse_resolver(entry))
            .collect()
    }
}

fn parse_resolver(entry: &str) -> Option<std::net::SocketAddr> {
    let entry = entry.trim();
    if let Ok(address) = entry.parse::<std::net::SocketAddr>() {
        return Some(address);
    }
    entry
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| std::net::SocketAddr::new(ip, 53))
}

impl Default for Dns {
    fn default() -> Self {
        Self {
            mx_records: vec![
                "mx1.postal.example.com".into(),
                "mx2.postal.example.com".into(),
            ],
            spf_include: "spf.postal.example.com".into(),
            return_path_domain: "rp.postal.example.com".into(),
            return_path_envelope: false,
            route_domain: "routes.postal.example.com".into(),
            track_domain: "track.postal.example.com".into(),
            helo_hostname: None,
            dkim_identifier: "postal".into(),
            domain_verify_prefix: "postal-verification".into(),
            verification_record_label: "_camelmailer-challenge".into(),
            verification_value_prefix: "camelmailer-verification".into(),
            custom_return_path_prefix: "psrp".into(),
            timeout: 5,
            resolv_conf_path: "/etc/resolv.conf".into(),
            check_resolvers: DEFAULT_CHECK_RESOLVERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

/// Convert a DNS name to the canonical ASCII form used for comparisons.
///
/// SMTP domain names are case-insensitive. Accept an operator-friendly
/// trailing root dot and Unicode input, but reject malformed labels.
pub fn normalize_dns_domain_name(value: &str) -> Option<String> {
    let domain = value.trim().trim_end_matches('.');
    let domain = idna::domain_to_ascii(domain).ok()?.to_ascii_lowercase();
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2
        || domain.len() > 253
        || labels.iter().any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                || !label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                || !label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
    {
        return None;
    }
    Some(domain)
}

impl Dns {
    /// The shared return-path domain in canonical form, when it can receive
    /// real mail. Reserved documentation domains deliberately remain unusable
    /// so an untouched sample configuration cannot change outbound MAIL FROM.
    pub fn normalized_return_path_domain(&self) -> Option<String> {
        let domain = normalize_dns_domain_name(&self.return_path_domain)?;
        for suffix in [
            "example",
            "example.com",
            "example.net",
            "example.org",
            "invalid",
            "localhost",
            "test",
        ] {
            if domain == suffix
                || domain
                    .strip_suffix(suffix)
                    .is_some_and(|prefix| prefix.ends_with('.'))
            {
                return None;
            }
        }
        Some(domain)
    }
}

/// Outbound SMTP used for application-level e-mail (password resets etc.).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Smtp {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub authentication_type: String,
    pub enable_starttls: bool,
    #[serde(default = "default_true")]
    pub enable_starttls_auto: bool,
    pub openssl_verify_mode: String,
    pub from_name: String,
    pub from_address: String,
}

impl Default for Smtp {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 25,
            username: None,
            password: None,
            authentication_type: "login".into(),
            enable_starttls: false,
            enable_starttls_auto: true,
            openssl_verify_mode: "peer".into(),
            from_name: "Postal".into(),
            from_address: "postal@example.com".into(),
        }
    }
}

/// Which address families outgoing SMTP connections use
/// (`smtp_client.address_family`, and per IP pool).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressFamily {
    /// Both families, IPv6 first, alternating with IPv4 (RFC 8305), raced
    /// with `connection_attempt_delay_ms` so a broken IPv6 path costs the
    /// attempt delay and not a connection timeout. The normal behaviour of
    /// a dual-stack host.
    #[default]
    DualStack,
    /// Both families, every IPv4 address before the first IPv6 one.
    PreferIpv4,
    /// IPv4 only; IPv6 addresses of the remote host are never tried.
    Ipv4Only,
    /// IPv6 only; IPv4 addresses of the remote host are never tried.
    Ipv6Only,
}

impl AddressFamily {
    /// Every value, as written in the config and the API.
    pub const ALL: [AddressFamily; 4] = [
        AddressFamily::DualStack,
        AddressFamily::PreferIpv4,
        AddressFamily::Ipv4Only,
        AddressFamily::Ipv6Only,
    ];

    /// The value as written in the config and the API.
    pub fn as_str(self) -> &'static str {
        match self {
            AddressFamily::DualStack => "dual_stack",
            AddressFamily::PreferIpv4 => "prefer_ipv4",
            AddressFamily::Ipv4Only => "ipv4_only",
            AddressFamily::Ipv6Only => "ipv6_only",
        }
    }

    /// Whether connections may use IPv4.
    pub fn allows_ipv4(self) -> bool {
        self != AddressFamily::Ipv6Only
    }

    /// Whether connections may use IPv6.
    pub fn allows_ipv6(self) -> bool {
        self != AddressFamily::Ipv4Only
    }
}

impl std::fmt::Display for AddressFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AddressFamily {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        AddressFamily::ALL
            .into_iter()
            .find(|family| family.as_str() == value.trim())
            .ok_or_else(|| {
                format!(
                    "address family must be dual_stack, prefer_ipv4, ipv4_only or ipv6_only \
                     (got {value:?})"
                )
            })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmtpClient {
    /// Which address families outgoing connections use when the server has
    /// no IP pool. A pool without an `address_family` of its own does not
    /// inherit this: it stays on its addresses' IPv4 values. Default
    /// `dual_stack`.
    pub address_family: AddressFamily,
    /// Milliseconds `dual_stack` waits for a connection attempt before it
    /// starts the next remote address in parallel (RFC 8305 section 5,
    /// "Connection Attempt Delay"). Each attempt keeps its own full
    /// `open_timeout`, a failed attempt starts the next one at once, and the
    /// first connection to succeed is used. `0` connects to one address at a
    /// time. The other families always connect one address at a time.
    /// Default `250`.
    pub connection_attempt_delay_ms: u32,
    /// Seconds between two reverse-DNS checks of the worker's source
    /// addresses (the first runs at start). `0` turns the check off.
    pub reverse_dns_check_interval: u32,
    /// Leave out an address family whose source address failed the
    /// reverse-DNS check, as long as the other family passed. Applies to
    /// `dual_stack` and `prefer_ipv4` only. Default `false`.
    pub skip_family_without_reverse_dns: bool,
    /// Seconds to wait for the TCP connection to a remote address.
    pub open_timeout: u32,
    /// Seconds to wait for the greeting, a TLS handshake and each reply.
    /// RFC 5321 4.5.3.2 asks for five minutes; some receivers hold the
    /// greeting back on purpose for senders they do not know yet.
    pub read_timeout: u32,
    /// Seconds to wait while the message body is sent and for the reply to
    /// its final dot (RFC 5321: ten minutes).
    pub data_timeout: u32,
    /// Greet with the `hostname` of the IP-pool address a delivery is bound
    /// to, so EHLO, the address's PTR and its A record name the same host.
    /// Falls back to `dns.helo_hostname` (else `camelmailer.smtp_hostname`)
    /// when the delivery has no pool address or the address has no hostname.
    /// Off by default, so an installation's EHLO never changes by itself.
    pub helo_from_ip_address: bool,
}

impl Default for SmtpClient {
    fn default() -> Self {
        Self {
            address_family: AddressFamily::DualStack,
            connection_attempt_delay_ms: 250,
            reverse_dns_check_interval: 3600,
            skip_family_without_reverse_dns: false,
            open_timeout: 30,
            read_timeout: 300,
            data_timeout: 600,
            helo_from_ip_address: false,
        }
    }
}

/// rspamd spam-scanning integration (`app/lib/postal/message_inspectors`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rspamd {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub ssl: bool,
    pub password: Option<String>,
    pub flags: Option<String>,
}

impl Default for Rspamd {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 11334,
            ssl: false,
            password: None,
            flags: None,
        }
    }
}

/// ClamAV virus scanning (INSTREAM over TCP).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clamav {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

impl Default for Clamav {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".into(),
            port: 3310,
        }
    }
}

/// PostgreSQL — the single multi-tenant database (replaces MariaDB's
/// `main_db` + database-per-server `message_db` layout; tenant isolation is
/// enforced with row-level security).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Postgres {
    /// Use PostgreSQL persistence (in-memory storage is used when disabled)
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub database: String,
    pub pool_size: u32,
}

impl Default for Postgres {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "localhost".into(),
            port: 5432,
            username: "camelmailer".into(),
            password: None,
            database: "camelmailer".into(),
            pool_size: 10,
        }
    }
}

impl Postgres {
    /// A `postgres://` connection URL. The `DATABASE_URL` environment
    /// variable, when set, takes precedence over the configured values.
    pub fn url(&self) -> String {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            return url;
        }
        let auth = match &self.password {
            Some(password) => format!("{}:{}", self.username, password),
            None => self.username.clone(),
        };
        format!(
            "postgres://{}@{}:{}/{}",
            auth, self.host, self.port, self.database
        )
    }
}

/// The complete Camelmailer configuration.
///
/// Unknown top-level groups (e.g. `rails`, `rspamd`, `oidc` from a legacy
/// `postal.yml`) are ignored so a legacy file loads without modification;
/// unknown keys *within* a known group are an error, mirroring the strictness
/// of the Konfig schema.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(alias = "postal")]
    pub camelmailer: Camelmailer,
    pub web_server: WebServer,
    pub auth: Auth,
    pub app_mail: AppMail,
    pub billing: Billing,
    pub lifecycle_webhooks: LifecycleWebhooks,
    pub deliverability_feedback: DeliverabilityFeedback,
    pub captcha: Captcha,
    pub retention: Retention,
    pub legal: Legal,
    pub oidc: Oidc,
    pub saml: Saml,
    pub scim: Scim,
    pub worker: Worker,
    pub postgres: Postgres,
    /// Legacy MariaDB settings, accepted for config-file compatibility only.
    pub main_db: MainDb,
    /// Legacy MariaDB settings, accepted for config-file compatibility only.
    pub message_db: MessageDb,
    pub logging: Logging,
    pub smtp_server: SmtpServer,
    pub dns: Dns,
    pub smtp: Smtp,
    pub smtp_client: SmtpClient,
    pub rspamd: Rspamd,
    pub clamav: Clamav,
    pub warmup: Warmup,
    pub cluster: Cluster,
    /// Receiving provider groups for the IP metrics and the warm-up, merged
    /// into the built-in ones (microsoft, google, yahoo, apple,
    /// united_internet, t_online): an entry with a built-in name replaces
    /// that group, any other name adds one. Whatever matches no group is
    /// `other`.
    pub provider_groups: Vec<ProviderGroupConfig>,
}

/// One receiving provider group (`provider_groups`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderGroupConfig {
    /// `[a-z0-9_]`, at most 40 characters.
    pub name: String,
    /// Recipient domains of the provider's own mail service: `example.com`
    /// exactly, or `brand.*` for the brand under any top-level domain.
    pub domains: Vec<String>,
    /// MX hosts: `example.net` matches it and every host under it.
    pub mx_patterns: Vec<String>,
}

/// Adaptive IP warm-up (`warmup`). Warm-up itself is off until a pool or a
/// pool address is given a mode (dashboard or
/// `PATCH /api/v2/admin/ip_warmup/...`); these are the limits it judges by.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Warmup {
    /// Below this share of its allowance an address's day is too quiet to
    /// judge, and the ramp holds.
    pub min_volume_fraction: f64,
    /// The share of its allowance an address must have used, with healthy
    /// results, for the allowance to grow.
    pub advance_fraction: f64,
    /// Deferral rate (of attempts to one provider) from which the ramp
    /// stops growing.
    pub deferral_rate_slow: f64,
    /// Bounce rate (of attempts) from which the ramp brakes.
    pub bounce_rate_brake: f64,
    /// Policy refusals (blocks, reputation refusals) that brake: at least
    /// this many, and at least `policy_block_rate_brake` of the attempts.
    pub policy_blocks_brake: i64,
    pub policy_block_rate_brake: f64,
    /// Complaint rate (of accepted messages) that slows, and that brakes.
    /// Google's sender guidelines: stay below 0.1 %, never reach 0.3 %.
    pub complaint_rate_warn: f64,
    pub complaint_rate_brake: f64,
    /// How long a brake stops an address from sending to that provider.
    pub pause_hours: i64,
    /// The factor a brake steps the allowance back by (0.1 to 1).
    pub step_back: f64,
    /// How long a message waits when no address of its pool has room left
    /// today. No attempt is counted.
    pub defer_seconds: u32,
    /// Changes to the built-in profiles `slow`, `normal` and `fast`: the base
    /// ramp (used by `other` and by groups the profile does not name) and the
    /// first day's allowance and cap per provider group.
    pub profiles: std::collections::BTreeMap<String, WarmupProfileConfig>,
}

/// Changes to one built-in warm-up profile (`warmup.profiles.<name>`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WarmupProfileConfig {
    pub start: Option<i64>,
    pub growth: Option<f64>,
    pub cap: Option<i64>,
    /// Per provider group: `{ start, cap }`.
    pub groups: std::collections::BTreeMap<String, WarmupGroupLimits>,
}

/// The first day's allowance and the cap of one provider group.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WarmupGroupLimits {
    pub start: i64,
    pub cap: i64,
}

impl Warmup {
    fn validate(&self) -> Result<(), ConfigError> {
        let fractions = [
            ("min_volume_fraction", self.min_volume_fraction),
            ("advance_fraction", self.advance_fraction),
            ("deferral_rate_slow", self.deferral_rate_slow),
            ("bounce_rate_brake", self.bounce_rate_brake),
            ("policy_block_rate_brake", self.policy_block_rate_brake),
            ("complaint_rate_warn", self.complaint_rate_warn),
            ("complaint_rate_brake", self.complaint_rate_brake),
        ];
        for (name, value) in fractions {
            if !(0.0..=1.0).contains(&value) {
                return Err(ConfigError::Invalid(format!(
                    "warmup.{name} must be between 0 and 1 (got {value})"
                )));
            }
        }
        if self.min_volume_fraction > self.advance_fraction {
            return Err(ConfigError::Invalid(
                "warmup.min_volume_fraction must not be above warmup.advance_fraction".into(),
            ));
        }
        if self.complaint_rate_warn > self.complaint_rate_brake {
            return Err(ConfigError::Invalid(
                "warmup.complaint_rate_warn must not be above warmup.complaint_rate_brake".into(),
            ));
        }
        if !(0.1..=1.0).contains(&self.step_back) {
            return Err(ConfigError::Invalid(format!(
                "warmup.step_back must be between 0.1 and 1 (got {})",
                self.step_back
            )));
        }
        for (name, profile) in &self.profiles {
            if !matches!(name.as_str(), "slow" | "normal" | "fast") {
                return Err(ConfigError::Invalid(format!(
                    "warmup.profiles: {name:?} is not a built-in profile (slow, normal, fast)"
                )));
            }
            if profile.start.is_some_and(|v| !(1..=1_000_000).contains(&v))
                || profile.growth.is_some_and(|v| !(1.05..=3.0).contains(&v))
                || profile.cap.is_some_and(|v| !(2..=10_000_000).contains(&v))
            {
                return Err(ConfigError::Invalid(format!(
                    "warmup.profiles.{name}: start 1 to 1000000, growth 1.05 to 3.0, cap up to 10000000"
                )));
            }
            for (group, limits) in &profile.groups {
                let valid_name = !group.is_empty()
                    && group.len() <= 40
                    && group
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
                if !valid_name
                    || limits.start < 1
                    || limits.cap <= limits.start
                    || limits.cap > 10_000_000
                {
                    return Err(ConfigError::Invalid(format!(
                        "warmup.profiles.{name}.groups.{group}: needs a valid group name, start >= 1 and cap above start"
                    )));
                }
            }
        }
        if self.pause_hours < 1 || self.policy_blocks_brake < 1 || self.defer_seconds < 60 {
            return Err(ConfigError::Invalid(
                "warmup.pause_hours and warmup.policy_blocks_brake must be at least 1, \
                 warmup.defer_seconds at least 60"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod warmup_tests {
    use super::*;

    #[test]
    fn warmup_limits_are_validated() {
        assert!(Config::default().validate().is_ok());
        let mut config = Config::default();
        config.warmup.complaint_rate_warn = 0.01;
        assert!(config.validate().is_err());
        let config =
            Config::from_yaml("warmup:\n  pause_hours: 48\n  defer_seconds: 600\n").unwrap();
        assert_eq!(config.warmup.pause_hours, 48);
        assert_eq!(config.warmup.advance_fraction, 0.9);
        assert!(config.validate().is_ok());
        assert!(Config::from_yaml("warmup:\n  unknown: 1\n").is_err());
        let config = Config::from_yaml(
            "warmup:\n  profiles:\n    normal:\n      growth: 1.4\n      groups:\n        seznam: { start: 80, cap: 25000 }\n",
        )
        .unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.warmup.profiles["normal"].groups["seznam"].cap, 25000);
        for bad in [
            "warmup:\n  profiles:\n    turbo: { start: 10 }\n",
            "warmup:\n  profiles:\n    slow: { growth: 5 }\n",
            "warmup:\n  profiles:\n    slow:\n      groups:\n        x: { start: 10, cap: 5 }\n",
        ] {
            assert!(Config::from_yaml(bad).unwrap().validate().is_err(), "{bad}");
        }
    }
}

impl Default for Warmup {
    fn default() -> Self {
        Self {
            min_volume_fraction: 0.5,
            advance_fraction: 0.9,
            deferral_rate_slow: 0.05,
            bounce_rate_brake: 0.05,
            policy_blocks_brake: 3,
            policy_block_rate_brake: 0.01,
            complaint_rate_warn: 0.001,
            complaint_rate_brake: 0.003,
            pause_hours: 24,
            step_back: 0.5,
            defer_seconds: 900,
            profiles: std::collections::BTreeMap::new(),
        }
    }
}

impl Config {
    /// Parse a configuration from a YAML string. An empty document yields the
    /// full default configuration.
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        if yaml.trim().is_empty() {
            return Ok(Self::default());
        }
        let config: Self = serde_yaml::from_str(yaml)?;
        Ok(config)
    }

    /// Load configuration from a file, substituting `$config-file-root` in
    /// path values with the directory containing the file (mirrors
    /// `Postal.substitute_config_file_root`).
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let yaml = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config = Self::from_yaml(&yaml).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        let root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_string_lossy()
            .into_owned();
        config.substitute_config_file_root(&root);
        config.validate()?;
        Ok(config)
    }

    /// The message metadata period after resolving the deprecated alias
    /// `camelmailer.message_retention_days`.
    pub fn message_metadata_retention_days(&self) -> u32 {
        if self.retention.message_metadata_days > 0 {
            self.retention.message_metadata_days
        } else {
            self.camelmailer.message_retention_days
        }
    }

    /// Settings that still load but no longer do anything. The binary logs
    /// each one at startup.
    pub fn deprecation_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.billing.legacy_stripe_secret_key.is_some() {
            warnings.push(
                "billing.stripe_secret_key is ignored: the built-in Stripe integration was removed; billing providers are plugged in by a custom build".into(),
            );
        }
        if self.camelmailer.message_retention_days > 0 {
            warnings.push(if self.retention.message_metadata_days > 0 {
                "camelmailer.message_retention_days is ignored because retention.message_metadata_days is set".into()
            } else {
                "camelmailer.message_retention_days is deprecated; use retention.message_metadata_days".into()
            });
        }
        if self.billing.legacy_portal_return_url.is_some() {
            warnings.push(
                "billing.portal_return_url is ignored (see billing.stripe_secret_key)".into(),
            );
        }
        warnings
    }

    /// Load configuration from the path named by `CAMELMAILER_CONFIG_FILE_PATH`
    /// (falling back to `POSTAL_CONFIG_FILE_PATH`, then the default path). A
    /// missing file at the *default* path yields the default configuration.
    pub fn load_from_env() -> Result<Self, ConfigError> {
        let explicit = std::env::var(ENV_CONFIG_FILE_PATH)
            .or_else(|_| std::env::var(LEGACY_ENV_CONFIG_FILE_PATH))
            .ok();
        match explicit {
            Some(path) => Self::load(Path::new(&path)),
            None => {
                let path = Path::new(DEFAULT_CONFIG_FILE_PATH);
                if path.exists() {
                    Self::load(path)
                } else {
                    Ok(Self::default())
                }
            }
        }
    }

    fn substitute_config_file_root(&mut self, root: &str) {
        for value in [
            &mut self.camelmailer.signing_key_path,
            &mut self.smtp_server.tls_certificate_path,
            &mut self.smtp_server.tls_private_key_path,
        ] {
            *value = value.replace("$config-file-root", root);
        }
    }

    /// The effective SAML SP entity id: `saml.sp_entity_id`, defaulting
    /// to `{camelmailer.web_protocol}://{camelmailer.web_hostname}`.
    pub fn saml_sp_entity_id(&self) -> String {
        match self.saml.sp_entity_id.as_deref().filter(|v| !v.is_empty()) {
            Some(value) => value.to_string(),
            None => format!(
                "{}://{}",
                self.camelmailer.web_protocol, self.camelmailer.web_hostname
            ),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.worker.validate()?;
        if self.dns.check_resolvers.is_empty() {
            return Err(ConfigError::Invalid(
                "dns.check_resolvers needs at least one resolver address".into(),
            ));
        }
        for entry in &self.dns.check_resolvers {
            if parse_resolver(entry).is_none() {
                return Err(ConfigError::Invalid(format!(
                    "dns.check_resolvers takes IP addresses, optionally with a port (got {entry:?})"
                )));
            }
        }
        self.warmup.validate()?;
        for group in &self.provider_groups {
            let valid = !group.name.is_empty()
                && group.name.len() <= 40
                && group
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !valid || group.name == "other" {
                return Err(ConfigError::Invalid(format!(
                    "provider_groups: {:?} is not a valid group name ([a-z0-9_], at most 40 characters, not \"other\")",
                    group.name
                )));
            }
            if group.domains.is_empty() && group.mx_patterns.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "provider_groups: {} needs domains or mx_patterns",
                    group.name
                )));
            }
        }
        self.cluster.validate()?;
        if !matches!(self.camelmailer.web_protocol.as_str(), "http" | "https") {
            return Err(ConfigError::Invalid(format!(
                "camelmailer.web_protocol must be http or https (got {:?})",
                self.camelmailer.web_protocol
            )));
        }
        if let Some(matcher) = &self.smtp_server.log_ip_address_exclusion_matcher {
            if matcher.is_empty() {
                return Err(ConfigError::Invalid(
                    "smtp_server.log_ip_address_exclusion_matcher must not be empty".into(),
                ));
            }
        }
        let mut seen_ports = vec![self.smtp_server.default_port];
        for listener in &self.smtp_server.listeners {
            if seen_ports.contains(&listener.port) {
                return Err(ConfigError::Invalid(format!(
                    "smtp_server.listeners: port {} is configured more than once \
                     (default_port and every listener must use distinct ports)",
                    listener.port
                )));
            }
            seen_ports.push(listener.port);
            if listener.mode == SmtpListenerMode::Smtps && !self.smtp_server.tls_enabled {
                return Err(ConfigError::Invalid(format!(
                    "smtp_server.listeners: port {} uses mode smtps, which requires \
                     smtp_server.tls_enabled with a certificate and private key",
                    listener.port
                )));
            }
        }
        if self.oidc.enabled {
            if self.oidc.issuer.is_empty() {
                return Err(ConfigError::Invalid(
                    "oidc.issuer is required when oidc.enabled is true".into(),
                ));
            }
            if self.oidc.identifier.as_deref().unwrap_or("").is_empty() {
                return Err(ConfigError::Invalid(
                    "oidc.identifier is required when oidc.enabled is true".into(),
                ));
            }
        }
        let mut seen_sso_ids: Vec<&str> = vec![];
        for provider in &self.auth.sso_providers {
            let id = provider.id.as_str();
            let valid_slug = !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !valid_slug {
                return Err(ConfigError::Invalid(format!(
                    "auth.sso_providers: id {id:?} is not a valid slug \
                     (lowercase letters, digits and hyphens only)"
                )));
            }
            if seen_sso_ids.contains(&id) {
                return Err(ConfigError::Invalid(format!(
                    "auth.sso_providers: id {id:?} is configured more than once"
                )));
            }
            seen_sso_ids.push(id);
            if provider.client_id.is_empty() || provider.client_secret.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "auth.sso_providers: provider {id:?} requires client_id and client_secret"
                )));
            }
            if provider.provider_type == SsoProviderType::Oidc && provider.issuer.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "auth.sso_providers: provider {id:?} has type oidc and requires an issuer"
                )));
            }
        }
        if self.saml.enabled {
            if self.saml.idp_sso_url.is_empty() {
                return Err(ConfigError::Invalid(
                    "saml.idp_sso_url is required when saml.enabled is true".into(),
                ));
            }
            if self
                .saml
                .idp_certificate
                .as_deref()
                .unwrap_or("")
                .is_empty()
            {
                return Err(ConfigError::Invalid(
                    "saml.idp_certificate is required when saml.enabled is true".into(),
                ));
            }
        }
        if self.scim.enabled && self.scim.bearer_token.as_deref().unwrap_or("").is_empty() {
            return Err(ConfigError::Invalid(
                "scim.bearer_token is required when scim.enabled is true".into(),
            ));
        }
        if self.auth.minimum_password_length < 8 {
            return Err(ConfigError::Invalid(
                "auth.minimum_password_length must be at least 8".into(),
            ));
        }
        if self.auth.webauthn.enabled {
            if self.auth.webauthn.rp_id.is_empty() {
                return Err(ConfigError::Invalid(
                    "auth.webauthn.rp_id is required when auth.webauthn.enabled is true".into(),
                ));
            }
            if self.auth.webauthn.rp_origin.is_empty() {
                return Err(ConfigError::Invalid(
                    "auth.webauthn.rp_origin is required when auth.webauthn.enabled is true".into(),
                ));
            }
        }
        if self.app_mail.enabled {
            if self
                .app_mail
                .server_api_key
                .as_deref()
                .unwrap_or("")
                .is_empty()
            {
                return Err(ConfigError::Invalid(
                    "app_mail.server_api_key is required when app_mail.enabled is true".into(),
                ));
            }
            if self
                .app_mail
                .from_address
                .as_deref()
                .unwrap_or("")
                .is_empty()
            {
                return Err(ConfigError::Invalid(
                    "app_mail.from_address is required when app_mail.enabled is true".into(),
                ));
            }
        }
        if self.lifecycle_webhooks.enabled() {
            if self
                .lifecycle_webhooks
                .secret
                .as_deref()
                .unwrap_or("")
                .is_empty()
            {
                return Err(ConfigError::Invalid(
                    "lifecycle_webhooks.secret is required when lifecycle_webhooks.urls is set"
                        .into(),
                ));
            }
            if let Some(url) = self
                .lifecycle_webhooks
                .urls
                .iter()
                .find(|url| !(url.starts_with("https://") || url.starts_with("http://")))
            {
                return Err(ConfigError::Invalid(format!(
                    "lifecycle_webhooks.urls: {url:?} is not an http(s) URL"
                )));
            }
        }
        if self.captcha.enabled() {
            match self.captcha.normalized_endpoint() {
                Some(endpoint)
                    if endpoint.starts_with("https://") || endpoint.starts_with("http://") => {}
                _ => {
                    return Err(ConfigError::Invalid(
                        "captcha.endpoint must be an http(s) URL when captcha.provider is set"
                            .into(),
                    ))
                }
            }
            if self.captcha.secret.as_deref().unwrap_or("").is_empty() {
                return Err(ConfigError::Invalid(
                    "captcha.secret is required when captcha.provider is set".into(),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_client_address_family_defaults_to_dual_stack_and_parses_every_value() {
        let config = Config::from_yaml("").unwrap();
        assert_eq!(config.smtp_client.address_family, AddressFamily::DualStack);
        assert_eq!(config.smtp_client.reverse_dns_check_interval, 3600);
        assert!(!config.smtp_client.skip_family_without_reverse_dns);
        assert_eq!(config.smtp_client.connection_attempt_delay_ms, 250);
        let config = Config::from_yaml("smtp_client:\n  connection_attempt_delay_ms: 0\n").unwrap();
        assert_eq!(config.smtp_client.connection_attempt_delay_ms, 0);

        for family in AddressFamily::ALL {
            let config = Config::from_yaml(&format!(
                "smtp_client:\n  address_family: {}\n  skip_family_without_reverse_dns: true\n",
                family.as_str()
            ))
            .unwrap();
            assert_eq!(config.smtp_client.address_family, family);
            assert!(config.smtp_client.skip_family_without_reverse_dns);
            assert_eq!(family.as_str().parse::<AddressFamily>(), Ok(family));
        }
        assert!(Config::from_yaml("smtp_client:\n  address_family: ipv4_first\n").is_err());
        assert!("ipv4_first".parse::<AddressFamily>().is_err());
        assert!(AddressFamily::Ipv4Only.allows_ipv4() && !AddressFamily::Ipv4Only.allows_ipv6());
        assert!(AddressFamily::Ipv6Only.allows_ipv6() && !AddressFamily::Ipv6Only.allows_ipv4());
    }

    #[test]
    fn defaults_match_the_ruby_schema() {
        let config = Config::default();
        assert_eq!(config.camelmailer.web_hostname, "postal.example.com");
        assert_eq!(config.camelmailer.web_protocol, "https");
        assert_eq!(config.camelmailer.smtp_hostname, "postal.example.com");
        assert!(!config.camelmailer.use_ip_pools);
        assert_eq!(config.camelmailer.default_maximum_delivery_attempts, 18);
        assert_eq!(config.camelmailer.default_dkim_key_size, 1024);
        assert!(config.camelmailer.use_resent_sender_header);
        assert!(config.camelmailer.batch_queued_messages);
        assert_eq!(config.camelmailer.batch_queued_messages_limit, 100);
        assert_eq!(config.camelmailer.admin_api_key, None);

        assert_eq!(config.web_server.default_port, 5000);
        assert_eq!(config.web_server.default_bind_address, "127.0.0.1");
        assert_eq!(config.worker.threads, 2);
        assert!(config.worker.servers.is_empty());
        assert_eq!(config.worker.deliver, WorkerDeliver::All);
        // Below Docker's default stop timeout of 10 s, so the worker is not
        // killed mid-shutdown when no stop_grace_period is configured.
        assert_eq!(config.worker.shutdown_grace_seconds, 8);
        assert!(config.worker.shutdown_grace_seconds < 10);

        assert_eq!(config.main_db.host, "localhost");
        assert_eq!(config.main_db.port, 3306);
        assert_eq!(config.main_db.database, "postal");
        assert_eq!(config.message_db.database_name_prefix, "postal");

        assert!(config.logging.enabled);
        assert!(!config.logging.rails_log_enabled);

        assert_eq!(config.smtp_server.default_port, 25);
        assert_eq!(config.smtp_server.default_bind_address, "::");
        assert!(!config.smtp_server.tls_enabled);
        // Secure by default: the migration window has to be asked for.
        assert!(config.smtp_server.auth_requires_tls);
        assert_eq!(config.smtp_server.max_message_size, 14);

        assert_eq!(
            config.dns.mx_records,
            vec!["mx1.postal.example.com", "mx2.postal.example.com"]
        );
        assert_eq!(config.dns.return_path_domain, "rp.postal.example.com");
        assert!(!config.dns.return_path_envelope);
        assert_eq!(config.dns.route_domain, "routes.postal.example.com");
        assert_eq!(config.dns.custom_return_path_prefix, "psrp");
        assert_eq!(config.dns.dkim_identifier, "postal");

        assert_eq!(config.smtp.port, 25);
        assert_eq!(config.smtp.authentication_type, "login");
        assert!(config.smtp.enable_starttls_auto);
    }

    #[test]
    fn return_path_envelope_requires_explicit_opt_in() {
        for (setting, enabled) in [
            ("", false),
            ("  return_path_envelope: false\n", false),
            ("  return_path_envelope: true\n", true),
        ] {
            let config: Config = serde_yaml::from_str(&format!(
                "dns:\n  return_path_domain: rp.camelmailer.com\n{setting}"
            ))
            .unwrap();
            assert_eq!(config.dns.return_path_envelope, enabled);
        }
    }

    #[test]
    fn dns_domain_names_are_normalized_for_comparison() {
        assert_eq!(
            normalize_dns_domain_name("  BÜCHER.Camelmailer.COM.  ").as_deref(),
            Some("xn--bcher-kva.camelmailer.com")
        );
        assert_eq!(normalize_dns_domain_name("bad_label.example"), None);
        assert_eq!(normalize_dns_domain_name("localhost"), None);
    }

    #[test]
    fn return_path_domain_rejects_reserved_values_after_normalizing() {
        let mut dns = Dns::default();
        for value in ["", "RP.Example.COM.", "foo.test", "localhost"] {
            dns.return_path_domain = value.into();
            assert_eq!(dns.normalized_return_path_domain(), None, "{value}");
        }

        dns.return_path_domain = "RP.Camelmailer.COM.".into();
        assert_eq!(
            dns.normalized_return_path_domain().as_deref(),
            Some("rp.camelmailer.com")
        );
    }

    #[test]
    fn postgres_defaults_and_url() {
        let config = Config::default();
        assert!(!config.postgres.enabled);
        assert_eq!(config.postgres.port, 5432);
        assert_eq!(config.postgres.database, "camelmailer");
        assert_eq!(
            config.postgres.url(),
            "postgres://camelmailer@localhost:5432/camelmailer"
        );

        let config = Config::from_yaml(
            r#"
postgres:
  enabled: true
  host: db.internal
  port: 5433
  username: app
  password: s3cret
  database: mail
"#,
        )
        .unwrap();
        assert!(config.postgres.enabled);
        assert_eq!(
            config.postgres.url(),
            "postgres://app:s3cret@db.internal:5433/mail"
        );
    }

    #[test]
    fn empty_yaml_yields_defaults() {
        let config = Config::from_yaml("").unwrap();
        assert_eq!(config.smtp_server.default_port, 25);
    }

    #[test]
    fn rspamd_and_clamav_defaults() {
        let config = Config::default();
        assert!(!config.rspamd.enabled);
        assert_eq!(config.rspamd.port, 11334);
        assert!(!config.clamav.enabled);
        assert_eq!(config.clamav.port, 3310);

        let config = Config::from_yaml(
            "rspamd:\n  enabled: true\n  host: scan.internal\nclamav:\n  enabled: true\n",
        )
        .unwrap();
        assert!(config.rspamd.enabled);
        assert_eq!(config.rspamd.host, "scan.internal");
        assert!(config.clamav.enabled);
    }

    #[test]
    fn yaml_overrides_defaults_and_keeps_the_rest() {
        let config = Config::from_yaml(
            r#"
camelmailer:
  smtp_hostname: mail.camel.example
  admin_api_key: secret123
smtp_server:
  default_port: 2525
  max_message_size: 25
dns:
  return_path_domain: rp.camel.example
"#,
        )
        .unwrap();
        assert_eq!(config.camelmailer.smtp_hostname, "mail.camel.example");
        assert_eq!(
            config.camelmailer.admin_api_key.as_deref(),
            Some("secret123")
        );
        assert_eq!(config.smtp_server.default_port, 2525);
        assert_eq!(config.smtp_server.max_message_size, 25);
        assert_eq!(config.dns.return_path_domain, "rp.camel.example");
        // untouched keys keep their defaults
        assert_eq!(config.camelmailer.web_hostname, "postal.example.com");
        assert_eq!(config.smtp_server.default_bind_address, "::");
    }

    #[test]
    fn legacy_postal_group_name_is_accepted() {
        let config = Config::from_yaml(
            r#"
postal:
  smtp_hostname: legacy.example.com
"#,
        )
        .unwrap();
        assert_eq!(config.camelmailer.smtp_hostname, "legacy.example.com");
    }

    #[test]
    fn unknown_keys_within_a_group_are_rejected() {
        let result = Config::from_yaml(
            r#"
smtp_server:
  no_such_key: true
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn unknown_top_level_groups_are_ignored_for_legacy_compat() {
        let config = Config::from_yaml(
            r#"
rails:
  environment: production
rspamd:
  enabled: true
"#,
        )
        .unwrap();
        assert_eq!(config.web_server.default_port, 5000);
    }

    #[test]
    fn invalid_web_protocol_fails_validation() {
        let mut config = Config::default();
        config.camelmailer.web_protocol = "gopher".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn load_substitutes_config_file_root() {
        let dir = std::env::temp_dir().join(format!("cm-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("camelmailer.yml");
        std::fs::write(&path, "logging:\n  enabled: false\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert!(!config.logging.enabled);
        assert_eq!(
            config.camelmailer.signing_key_path,
            format!("{}/signing.key", dir.to_string_lossy())
        );
        assert_eq!(
            config.smtp_server.tls_certificate_path,
            format!("{}/smtp.cert", dir.to_string_lossy())
        );
        std::fs::remove_dir_all(&dir).ok();
    }
    #[test]
    fn auth_and_oidc_defaults() {
        let config = Config::default();
        assert_eq!(config.web_server.cors_origins, Vec::<String>::new());
        assert_eq!(config.auth.session_timeout_days, 14);
        assert_eq!(config.auth.max_login_attempts, 5);
        assert_eq!(config.auth.lockout_minutes, 15);
        assert_eq!(config.auth.minimum_password_length, 8);
        assert!(config.auth.allow_organization_creation);
        assert!(!config.auth.allow_registration);
        assert!(!config.auth.bootstrap_workspace);
        assert_eq!(config.auth.invitation_expiry_days, 7);
        assert_eq!(config.auth.password_reset_expiry_hours, 2);
        assert_eq!(config.auth.frontend_url, None);
        assert!(!config.auth.webauthn.enabled);
        assert_eq!(config.auth.webauthn.rp_id, "");
        assert_eq!(config.auth.webauthn.rp_origin, "");
        assert_eq!(config.auth.webauthn.rp_name, "Camelmailer");
        assert!(!config.oidc.enabled);
        assert_eq!(config.oidc.scopes, vec!["openid", "email", "profile"]);
        assert_eq!(config.oidc.uid_field, "sub");
        assert_eq!(config.oidc.email_address_field, "email");
        assert!(config.oidc.auto_provision);
    }

    #[test]
    fn oidc_group_accepts_the_legacy_postal_keys() {
        let config = Config::from_yaml(
            "oidc:\n  enabled: true\n  name: Okta\n  issuer: https://idp.example.com\n  identifier: client-1\n  secret: s3cret\n  scopes: [openid, email]\n  uid_field: sub\n  email_address_field: email\n  name_field: name\n  discovery: true\n",
        )
        .unwrap();
        assert!(config.oidc.enabled);
        assert_eq!(config.oidc.issuer, "https://idp.example.com");
        assert_eq!(config.oidc.identifier.as_deref(), Some("client-1"));
        config.validate().unwrap();
    }

    #[test]
    fn enabled_oidc_requires_issuer_and_identifier() {
        let config = Config::from_yaml("oidc:\n  enabled: true\n").unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml("oidc:\n  enabled: true\n  issuer: https://x\n").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn sso_providers_default_to_empty() {
        let config = Config::default();
        assert!(config.auth.sso_providers.is_empty());
    }

    #[test]
    fn saml_defaults_and_sp_entity_id() {
        let config = Config::default();
        assert!(!config.saml.enabled);
        assert_eq!(config.saml.name, "SAML");
        assert!(config.saml.auto_provision);
        assert!(config.saml.allowed_email_domains.is_empty());
        // sp_entity_id defaults to the web origin
        assert_eq!(config.saml_sp_entity_id(), "https://postal.example.com");
        config.validate().unwrap();

        let config = Config::from_yaml(
            "saml:\n  enabled: true\n  name: Okta\n  idp_sso_url: https://idp.example.com/sso\n  idp_certificate: |\n    -----BEGIN CERTIFICATE-----\n    MIIB\n    -----END CERTIFICATE-----\n  sp_entity_id: https://mail.example.com/saml\n  auto_provision: false\n  allowed_email_domains: [corp.example]\n",
        )
        .unwrap();
        assert!(config.saml.enabled);
        assert_eq!(config.saml.name, "Okta");
        assert_eq!(config.saml.idp_sso_url, "https://idp.example.com/sso");
        assert_eq!(config.saml_sp_entity_id(), "https://mail.example.com/saml");
        assert!(!config.saml.auto_provision);
        assert_eq!(config.saml.allowed_email_domains, vec!["corp.example"]);
        config.validate().unwrap();
    }

    #[test]
    fn sso_providers_parse_from_yaml() {
        let config = Config::from_yaml(
            r#"
auth:
  sso_providers:
    - { id: google, type: oidc, name: Google, issuer: "https://accounts.google.com", client_id: cid-1, client_secret: cs-1 }
    - { id: microsoft, type: oidc, name: Microsoft, issuer: "https://login.microsoftonline.com/common/v2.0", client_id: cid-2, client_secret: cs-2, allowed_email_domains: [corp.example] }
    - { id: github, type: github, name: GitHub, client_id: cid-3, client_secret: cs-3, auto_provision: false }
"#,
        )
        .unwrap();
        config.validate().unwrap();
        let providers = &config.auth.sso_providers;
        assert_eq!(providers.len(), 3);
        assert_eq!(providers[0].id, "google");
        assert_eq!(providers[0].provider_type, SsoProviderType::Oidc);
        assert_eq!(providers[0].name, "Google");
        assert_eq!(providers[0].issuer, "https://accounts.google.com");
        assert!(providers[0].auto_provision, "auto_provision defaults on");
        assert!(providers[0].allowed_email_domains.is_empty());
        assert_eq!(providers[1].allowed_email_domains, vec!["corp.example"]);
        assert_eq!(providers[2].provider_type, SsoProviderType::Github);
        assert!(providers[2].issuer.is_empty());
        assert!(!providers[2].auto_provision);
    }

    #[test]
    fn sso_provider_ids_must_be_unique_slugs() {
        let config = Config::from_yaml(
            "auth:\n  sso_providers:\n    - { id: google, type: oidc, name: A, issuer: 'https://a', client_id: c, client_secret: s }\n    - { id: google, type: github, name: B, client_id: c, client_secret: s }\n",
        )
        .unwrap();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("more than once"));

        for bad_id in ["", "Google", "goo gle", "göögle", "a/b"] {
            let config = Config::from_yaml(&format!(
                "auth:\n  sso_providers:\n    - {{ id: {bad_id:?}, type: github, name: X, client_id: c, client_secret: s }}\n",
            ))
            .unwrap();
            assert!(
                config.validate().unwrap_err().to_string().contains("slug"),
                "id {bad_id:?} should be rejected"
            );
        }
    }

    #[test]
    fn sso_oidc_provider_requires_an_issuer() {
        let config = Config::from_yaml(
            "auth:\n  sso_providers:\n    - { id: google, type: oidc, name: Google, client_id: c, client_secret: s }\n",
        )
        .unwrap();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("issuer"));
        // github providers do not need one
        let config = Config::from_yaml(
            "auth:\n  sso_providers:\n    - { id: github, type: github, name: GitHub, client_id: c, client_secret: s }\n",
        )
        .unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn enabled_saml_requires_sso_url_and_certificate() {
        let config = Config::from_yaml("saml:\n  enabled: true\n").unwrap();
        assert!(config.validate().is_err());
        let config =
            Config::from_yaml("saml:\n  enabled: true\n  idp_sso_url: https://idp/sso\n").unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml(
            "saml:\n  enabled: true\n  idp_sso_url: https://idp/sso\n  idp_certificate: /etc/camelmailer/idp.pem\n",
        )
        .unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn sso_providers_require_client_id_and_secret() {
        for missing in [
            "{ id: x, type: github, name: X, client_id: '', client_secret: s }",
            "{ id: x, type: github, name: X, client_id: c, client_secret: '' }",
        ] {
            let config =
                Config::from_yaml(&format!("auth:\n  sso_providers:\n    - {missing}\n")).unwrap();
            assert!(config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("client_id and client_secret"));
        }
    }

    #[test]
    fn unknown_sso_provider_type_is_rejected() {
        let result = Config::from_yaml(
            "auth:\n  sso_providers:\n    - { id: x, type: saml, name: X, client_id: c, client_secret: s }\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn scim_defaults_and_token_requirement() {
        let config = Config::default();
        assert!(!config.scim.enabled);
        assert_eq!(config.scim.bearer_token, None);
        config.validate().unwrap();

        let config = Config::from_yaml("scim:\n  enabled: true\n").unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml("scim:\n  enabled: true\n  bearer_token: \"\"\n").unwrap();
        assert!(config.validate().is_err());
        let config =
            Config::from_yaml("scim:\n  enabled: true\n  bearer_token: secret-token\n").unwrap();
        assert_eq!(config.scim.bearer_token.as_deref(), Some("secret-token"));
        config.validate().unwrap();
    }

    #[test]
    fn app_mail_defaults_to_disabled() {
        let config = Config::default();
        assert!(!config.app_mail.enabled);
        assert_eq!(config.app_mail.server_api_key, None);
        assert_eq!(config.app_mail.from_address, None);
        assert_eq!(config.app_mail.from_name, "Camelmailer");
        config.validate().unwrap();
    }

    #[test]
    fn app_mail_parses_from_yaml() {
        let config = Config::from_yaml(
            "app_mail:\n  enabled: true\n  server_api_key: key-123\n  from_address: no-reply@example.com\n  from_name: Example Mail\n",
        )
        .unwrap();
        assert!(config.app_mail.enabled);
        assert_eq!(config.app_mail.server_api_key.as_deref(), Some("key-123"));
        assert_eq!(
            config.app_mail.from_address.as_deref(),
            Some("no-reply@example.com")
        );
        assert_eq!(config.app_mail.from_name, "Example Mail");
        config.validate().unwrap();
    }

    #[test]
    fn enabled_app_mail_requires_key_and_from_address() {
        let config = Config::from_yaml("app_mail:\n  enabled: true\n").unwrap();
        assert!(config.validate().is_err());
        let config =
            Config::from_yaml("app_mail:\n  enabled: true\n  server_api_key: k\n").unwrap();
        assert!(config.validate().is_err());
        let config =
            Config::from_yaml("app_mail:\n  enabled: true\n  from_address: a@b.c\n").unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml(
            "app_mail:\n  enabled: true\n  server_api_key: k\n  from_address: a@b.c\n",
        )
        .unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn billing_defaults_to_disabled() {
        let config = Config::default();
        assert!(!config.billing.enabled);
        assert!(config.billing.options.is_empty());
        config.validate().unwrap();
    }

    #[test]
    fn billing_parses_provider_options_from_yaml() {
        let config = Config::from_yaml(
            "billing:\n  enabled: true\n  options:\n    api_url: https://billing.example.com\n    api_key: k-secret\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert!(config.billing.enabled);
        assert_eq!(
            config.billing.options["api_url"].as_str(),
            Some("https://billing.example.com")
        );
        let debug = format!("{:?}", config.billing);
        assert!(!debug.contains("k-secret"), "{debug}");
    }

    #[test]
    fn legacy_stripe_keys_still_load_with_a_warning() {
        let config = Config::from_yaml(
            "billing:\n  enabled: true\n  stripe_secret_key: sk_test_1\n  portal_return_url: https://x.example\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.deprecation_warnings().len(), 2);
        assert!(Config::default().deprecation_warnings().is_empty());
    }

    #[test]
    fn retention_defaults_keep_everything_but_the_api_log() {
        let config = Config::default();
        assert_eq!(config.retention.message_content_days, 0);
        assert_eq!(config.retention.api_request_days, 30);
        assert_eq!(config.message_metadata_retention_days(), 0);
        let config = Config::from_yaml(
            "retention:\n  message_content_days: 30\n  message_metadata_days: 365\n  auth_event_days: 180\n",
        )
        .unwrap();
        assert_eq!(config.retention.message_content_days, 30);
        assert_eq!(config.message_metadata_retention_days(), 365);
        assert!(Config::from_yaml("retention:\n  foo: 1\n").is_err());
    }

    #[test]
    fn message_retention_days_is_a_deprecated_alias() {
        let config = Config::from_yaml("camelmailer:\n  message_retention_days: 90\n").unwrap();
        assert_eq!(config.message_metadata_retention_days(), 90);
        assert_eq!(config.deprecation_warnings().len(), 1);
        let config = Config::from_yaml(
            "camelmailer:\n  message_retention_days: 90\nretention:\n  message_metadata_days: 30\n",
        )
        .unwrap();
        assert_eq!(config.message_metadata_retention_days(), 30);
    }

    #[test]
    fn lifecycle_webhooks_require_a_secret_and_http_urls() {
        assert!(!Config::default().lifecycle_webhooks.enabled());
        let config =
            Config::from_yaml("lifecycle_webhooks:\n  urls: [https://hooks.example.com/cm]\n")
                .unwrap();
        assert!(config.validate().is_err());
        let config =
            Config::from_yaml("lifecycle_webhooks:\n  urls: [ftp://x]\n  secret: s\n").unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml(
            "lifecycle_webhooks:\n  urls: [https://hooks.example.com/cm]\n  secret: whsec\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert!(config.lifecycle_webhooks.enabled());
        assert!(!format!("{:?}", config.lifecycle_webhooks).contains("whsec"));
    }

    #[test]
    fn captcha_defaults_to_none_and_cap_needs_endpoint_and_secret() {
        let config = Config::default();
        assert!(!config.captcha.enabled());
        assert_eq!(config.captcha.provider.as_str(), "none");
        let config = Config::from_yaml("captcha:\n  provider: cap\n").unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml(
            "captcha:\n  provider: cap\n  endpoint: https://cap.example.com/abc\n",
        )
        .unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml(
            "captcha:\n  provider: cap\n  endpoint: https://cap.example.com/abc\n  secret: shh\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.captcha.siteverify_url().as_deref(),
            Some("https://cap.example.com/abc/siteverify")
        );
        assert!(!format!("{:?}", config.captcha).contains("shh"));
        assert!(Config::from_yaml("captcha:\n  provider: recaptcha\n").is_err());
    }

    #[test]
    fn webauthn_parses_from_yaml() {
        let config = Config::from_yaml(
            "auth:\n  webauthn:\n    enabled: true\n    rp_id: app.camelmailer.com\n    rp_origin: https://app.camelmailer.com\n    rp_name: Example\n",
        )
        .unwrap();
        assert!(config.auth.webauthn.enabled);
        assert_eq!(config.auth.webauthn.rp_id, "app.camelmailer.com");
        assert_eq!(
            config.auth.webauthn.rp_origin,
            "https://app.camelmailer.com"
        );
        assert_eq!(config.auth.webauthn.rp_name, "Example");
        config.validate().unwrap();
    }

    #[test]
    fn enabled_webauthn_requires_rp_id_and_rp_origin() {
        let config = Config::from_yaml("auth:\n  webauthn:\n    enabled: true\n").unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml(
            "auth:\n  webauthn:\n    enabled: true\n    rp_id: app.camelmailer.com\n",
        )
        .unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml(
            "auth:\n  webauthn:\n    enabled: true\n    rp_origin: https://app.camelmailer.com\n",
        )
        .unwrap();
        assert!(config.validate().is_err());
        let config = Config::from_yaml(
            "auth:\n  webauthn:\n    enabled: true\n    rp_id: app.camelmailer.com\n    rp_origin: https://app.camelmailer.com\n",
        )
        .unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn auth_requires_tls_parses_from_yaml() {
        let config = Config::from_yaml(
            "camelmailer:\n  web_hostname: mail.example.com\n\
             smtp_server:\n  tls_enabled: true\n  auth_requires_tls: false\n",
        )
        .unwrap();
        assert!(config.smtp_server.tls_enabled);
        assert!(!config.smtp_server.auth_requires_tls);
        // A migration window is a valid configuration, not a config error.
        config.validate().unwrap();
    }

    #[test]
    fn smtp_listeners_default_to_empty() {
        let config = Config::default();
        assert!(config.smtp_server.listeners.is_empty());
        config.validate().unwrap();
    }

    #[test]
    fn smtp_listeners_parse_from_yaml() {
        let config = Config::from_yaml(
            r#"
smtp_server:
  tls_enabled: true
  listeners:
    - { port: 587, mode: smtp }
    - { port: 465, mode: smtps }
    - { port: 2525 }
"#,
        )
        .unwrap();
        assert_eq!(
            config.smtp_server.listeners,
            vec![
                SmtpListener {
                    port: 587,
                    mode: SmtpListenerMode::Smtp
                },
                SmtpListener {
                    port: 465,
                    mode: SmtpListenerMode::Smtps
                },
                SmtpListener {
                    port: 2525,
                    mode: SmtpListenerMode::Smtp
                },
            ]
        );
        config.validate().unwrap();
    }

    #[test]
    fn smtps_listener_requires_tls_enabled() {
        let config =
            Config::from_yaml("smtp_server:\n  listeners:\n    - { port: 465, mode: smtps }\n")
                .unwrap();
        let error = config.validate().unwrap_err();
        assert!(error.to_string().contains("smtp_server.tls_enabled"));
    }

    #[test]
    fn duplicate_listener_ports_are_rejected() {
        // A listener duplicating another listener …
        let config = Config::from_yaml(
            "smtp_server:\n  listeners:\n    - { port: 587 }\n    - { port: 587 }\n",
        )
        .unwrap();
        assert!(config.validate().is_err());
        // … and a listener duplicating default_port.
        let config = Config::from_yaml(
            "smtp_server:\n  default_port: 2525\n  listeners:\n    - { port: 2525 }\n",
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn unknown_listener_mode_is_rejected() {
        let result =
            Config::from_yaml("smtp_server:\n  listeners:\n    - { port: 465, mode: tls }\n");
        assert!(result.is_err());
    }

    #[test]
    fn weak_minimum_password_length_is_rejected() {
        let config = Config::from_yaml("auth:\n  minimum_password_length: 4\n").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn cors_origins_parse_from_yaml() {
        let config = Config::from_yaml(
            "web_server:\n  cors_origins:\n    - https://app.example.com\n    - http://localhost:5173\n",
        )
        .unwrap();
        assert_eq!(
            config.web_server.cors_origins,
            vec!["https://app.example.com", "http://localhost:5173"]
        );
    }

    #[test]
    fn worker_scope_parses_and_validates() {
        let config =
            Config::from_yaml("worker:\n  threads: 4\n  servers: [acme/bulk]\n  deliver: bulk\n")
                .unwrap();
        assert_eq!(config.worker.threads, 4);
        assert_eq!(config.worker.servers, vec!["acme/bulk"]);
        assert_eq!(config.worker.deliver, WorkerDeliver::Bulk);
        config.validate().unwrap();

        let typo = Config::from_yaml("worker:\n  servers: [acme]\n").unwrap();
        assert!(typo.validate().is_err());
        let both = Config::from_yaml("worker:\n  servers: [acme/a]\n  exclude_servers: [acme/a]\n")
            .unwrap();
        assert!(both.validate().is_err());
        assert!(Config::from_yaml("worker:\n  deliver: newsletters\n").is_err());
    }

    #[test]
    fn server_shutdown_grace_defaults_and_env_overrides() {
        let config = Config::from_yaml("").unwrap();
        assert_eq!(config.web_server.shutdown_grace_seconds, 30);
        assert_eq!(config.smtp_server.shutdown_grace_seconds, 30);
        let config = Config::from_yaml(
            "web_server:\n  shutdown_grace_seconds: 12\nsmtp_server:\n  shutdown_grace_seconds: 45\n",
        )
        .unwrap();
        let none = |_: &str| None;
        assert_eq!(
            config.web_server.shutdown_grace_with(none).unwrap(),
            std::time::Duration::from_secs(12)
        );
        assert_eq!(
            config.smtp_server.shutdown_grace_with(none).unwrap(),
            std::time::Duration::from_secs(45)
        );
        let env = |name: &str| match name {
            "WEB_SERVER_SHUTDOWN_GRACE_SECONDS" => Some("5".to_string()),
            "SMTP_SERVER_SHUTDOWN_GRACE_SECONDS" => Some(" 0 ".to_string()),
            _ => None,
        };
        assert_eq!(
            config.web_server.shutdown_grace_with(env).unwrap(),
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            config.smtp_server.shutdown_grace_with(env).unwrap(),
            std::time::Duration::ZERO
        );
        let bad = |_: &str| Some("soon".to_string());
        assert!(config.smtp_server.shutdown_grace_with(bad).is_err());
    }

    #[test]
    fn cluster_defaults_and_validation() {
        let config = Config::from_yaml("").unwrap();
        assert_eq!(config.cluster.heartbeat_interval_seconds, 30);
        assert_eq!(config.cluster.node_stale_seconds, 90);
        assert!(config.cluster.public_addresses.is_empty());
        assert!(config.validate().is_ok());

        let config = Config::from_yaml(
            "cluster:\n  node_name: mail-1\n  load_balancer_address: 2.31.154.200\n  inbound_hostname: Mail.Example.com\n  public_addresses: [\"49.13.35.233\", \"2a01:4f8::1\"]\n",
        )
        .unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.cluster.effective_node_name(), "mail-1");
        assert_eq!(config.cluster.load_balancer(), Some("2.31.154.200"));
        assert_eq!(
            config.cluster.inbound_host().as_deref(),
            Some("mail.example.com")
        );
        assert_eq!(config.cluster.public_ips().len(), 2);

        for bad in [
            "cluster:\n  public_addresses: [\"172.18.0.x\"]\n",
            "cluster:\n  load_balancer_address: lb.example.com\n",
            "cluster:\n  inbound_hostname: \"mail\"\n",
            "cluster:\n  inbound_hostname: \"mail.example.com.\"\n",
            "cluster:\n  heartbeat_interval_seconds: 2\n",
            "cluster:\n  heartbeat_interval_seconds: 30\n  node_stale_seconds: 40\n",
        ] {
            let config = Config::from_yaml(bad).unwrap();
            assert!(config.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn cluster_env_overrides_replace_the_file_values() {
        let env = |name: &str| match name {
            "CAMELMAILER_NODE_NAME" => Some(" mail-2 ".to_string()),
            "CAMELMAILER_PUBLIC_ADDRESSES" => Some("167.233.167.89, 2a01:4f8:1c18:aa2d::1".into()),
            _ => None,
        };
        let cluster = Cluster::default().with_overrides(env).unwrap();
        assert_eq!(cluster.node_name, "mail-2");
        assert_eq!(
            cluster.public_addresses,
            vec!["167.233.167.89", "2a01:4f8:1c18:aa2d::1"]
        );
        let bad = |name: &str| (name == "CAMELMAILER_PUBLIC_ADDRESSES").then(|| "nope".to_string());
        assert!(Cluster::default().with_overrides(bad).is_err());
    }

    #[test]
    fn worker_env_overrides_replace_the_file_values() {
        let file = Worker {
            servers: vec!["acme/a".into()],
            ..Worker::default()
        };
        let env = |name: &str| match name {
            "WORKER_THREADS" => Some("8".to_string()),
            "WORKER_EXCLUDE_SERVERS" => Some(" acme/b , acme/c ,".to_string()),
            "WORKER_DELIVER" => Some("transactional".to_string()),
            _ => None,
        };
        let worker = file.with_overrides(env).unwrap();
        assert_eq!(worker.threads, 8);
        assert_eq!(worker.servers, vec!["acme/a"]);
        assert_eq!(worker.exclude_servers, vec!["acme/b", "acme/c"]);
        assert_eq!(worker.deliver, WorkerDeliver::Transactional);

        let bad = |name: &str| (name == "WORKER_DELIVER").then(|| "some".to_string());
        assert!(Worker::default().with_overrides(bad).is_err());
        let zero = |name: &str| (name == "WORKER_THREADS").then(|| "0".to_string());
        assert!(Worker::default().with_overrides(zero).is_err());
    }

    #[test]
    fn host_aware_pool_settings_come_from_the_environment() {
        let env = |name: &str| match name {
            "WORKER_HOST_AWARE_POOLS" => Some("true".to_string()),
            "WORKER_SOURCE_ADDRESSES" => Some("203.0.113.5, 2001:db8::5".to_string()),
            _ => None,
        };
        let worker = Worker::default().with_overrides(env).unwrap();
        assert!(worker.host_aware_pools);
        assert_eq!(worker.source_addresses, vec!["203.0.113.5", "2001:db8::5"]);
        assert_eq!(worker.foreign_pool_delay_seconds, 30);
        assert!(!Worker::default().host_aware_pools);

        let bad_flag = |name: &str| (name == "WORKER_HOST_AWARE_POOLS").then(|| "yes".to_string());
        assert!(Worker::default().with_overrides(bad_flag).is_err());
        let bad_address =
            |name: &str| (name == "WORKER_SOURCE_ADDRESSES").then(|| "out1.example".to_string());
        assert!(Worker::default().with_overrides(bad_address).is_err());
    }

    #[test]
    fn the_foreign_pool_delay_comes_from_the_environment_and_must_be_positive() {
        let delay =
            |name: &str| (name == "WORKER_FOREIGN_POOL_DELAY_SECONDS").then(|| "90".to_string());
        assert_eq!(
            Worker::default()
                .with_overrides(delay)
                .unwrap()
                .foreign_pool_delay_seconds,
            90
        );
        let zero =
            |name: &str| (name == "WORKER_FOREIGN_POOL_DELAY_SECONDS").then(|| "0".to_string());
        assert!(Worker::default().with_overrides(zero).is_err());
        let junk =
            |name: &str| (name == "WORKER_FOREIGN_POOL_DELAY_SECONDS").then(|| "soon".to_string());
        assert!(Worker::default().with_overrides(junk).is_err());
    }

    #[test]
    fn provider_feedback_is_off_without_credentials_and_reads_its_group() {
        let config = Config::from_yaml("").unwrap();
        let feedback = &config.deliverability_feedback;
        assert_eq!(feedback.fetch_interval_seconds, 21_600);
        assert!(feedback.postmaster_tools.domains.is_empty());
        assert!(feedback.snds.data_url_file.is_none());
        assert!(feedback.fbl.suppress_complainants, "on by default");
        assert_eq!(config.retention.feedback_report_days, 30);
        assert_eq!(
            feedback.postmaster_tools.api_base_url,
            "https://gmailpostmastertools.googleapis.com"
        );

        let config = Config::from_yaml(
            "deliverability_feedback:\n  postmaster_tools:\n    domains: [example.com]\n    \
             client_id: id\n    refresh_token_file: /run/secrets/t\n  snds:\n    \
             data_url_file: /run/secrets/snds\n  fbl:\n    suppress_complainants: false\n",
        )
        .unwrap();
        let feedback = &config.deliverability_feedback;
        assert_eq!(feedback.postmaster_tools.domains, vec!["example.com"]);
        assert_eq!(feedback.postmaster_tools.days, 7);
        assert_eq!(
            feedback.snds.data_url_file.as_deref(),
            Some("/run/secrets/snds")
        );
        assert!(!feedback.fbl.suppress_complainants);
        assert!(Config::from_yaml("deliverability_feedback:\n  snds:\n    key: x\n").is_err());

        let config = Config::from_yaml(
            "deliverability_feedback:\n  placement:\n    server: ops/placement\n    \
             from_address: p@example.com\n    seeds:\n      - provider: outlook\n        \
             address: s@outlook.com\n        auth: xoauth2\n        client_id: c\n        \
             refresh_token_file: /run/secrets/r\n      - provider: yahoo\n        \
             address: s@yahoo.com\n        password_env: SEED_YAHOO\n",
        )
        .unwrap();
        let placement = &config.deliverability_feedback.placement;
        assert_eq!(placement.seeds.len(), 2);
        assert_eq!(placement.seeds[0].provider, SeedProvider::Outlook);
        assert_eq!(placement.seeds[0].auth, SeedAuth::Xoauth2);
        assert_eq!(placement.seeds[1].auth, SeedAuth::AppPassword);
        assert_eq!(placement.min_round_interval_seconds, 3600);
        assert_eq!(placement.interval_seconds, 0);
    }
}

#[cfg(test)]
mod provider_group_tests {
    use super::*;

    #[test]
    fn provider_groups_are_read_and_checked() {
        let config = Config::from_yaml(
            "provider_groups:\n  - name: seznam\n    domains: [seznam.cz]\n    mx_patterns: [seznam.cz]\n",
        )
        .unwrap();
        assert_eq!(config.provider_groups[0].name, "seznam");
        assert!(config.validate().is_ok());
        for bad in [
            "provider_groups:\n  - name: Seznam\n    domains: [seznam.cz]\n",
            "provider_groups:\n  - name: other\n    domains: [x.cz]\n",
            "provider_groups:\n  - name: empty\n",
        ] {
            assert!(Config::from_yaml(bad).unwrap().validate().is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod check_resolver_tests {
    use super::*;

    #[test]
    fn check_resolvers_default_to_public_ones_and_are_validated() {
        let config = Config::default();
        assert_eq!(
            config.dns.check_resolver_addresses(),
            vec![
                "1.1.1.1:53".parse::<std::net::SocketAddr>().unwrap(),
                "9.9.9.9:53".parse().unwrap()
            ]
        );
        let config = Config::from_yaml(
            "dns:\n  check_resolvers: [\"192.0.2.53\", \"192.0.2.54:5353\", \"[2001:db8::53]:53\", \"2001:db8::54\"]\n",
        )
        .unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.dns.check_resolver_addresses().len(), 4);
        assert_eq!(config.dns.check_resolver_addresses()[1].port(), 5353);
        for bad in [
            "dns:\n  check_resolvers: []\n",
            "dns:\n  check_resolvers: [resolver.example]\n",
        ] {
            assert!(Config::from_yaml(bad).unwrap().validate().is_err(), "{bad}");
        }
    }
}
