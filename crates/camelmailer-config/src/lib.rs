//! Configuration for CamelMailer.
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
pub struct CamelMailer {
    /// The hostname that the CamelMailer web interface runs on
    pub web_hostname: String,
    /// The HTTP protocol to use for the CamelMailer web interface
    pub web_protocol: String,
    /// The hostname that the CamelMailer SMTP server runs on
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
    /// SMTP relays in the format smtp://host:port
    pub smtp_relays: Vec<String>,
    /// IP addresses to trust for proxying requests (in addition to localhost)
    pub trusted_proxies: Vec<String>,
    pub queued_message_lock_stale_days: u32,
    #[serde(default = "default_true")]
    pub batch_queued_messages: bool,
    pub batch_queued_messages_limit: u32,
    /// The global API key for the Admin API. If not set, only database-backed
    /// admin API keys are accepted.
    pub admin_api_key: Option<String>,
}

impl Default for CamelMailer {
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
            batch_queued_messages: true,
            batch_queued_messages_limit: 100,
            admin_api_key: None,
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
}

impl Default for WebServer {
    fn default() -> Self {
        Self {
            default_port: 5000,
            default_bind_address: "127.0.0.1".into(),
            max_threads: 5,
            cors_origins: vec![],
        }
    }
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
    pub invitation_expiry_days: u32,
    /// Password-reset link lifetime.
    pub password_reset_expiry_hours: u32,
    /// Base URL of the web frontend — used to build invitation/reset links
    /// and as the OIDC post-login redirect target.
    pub frontend_url: Option<String>,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            session_timeout_days: 14,
            max_login_attempts: 5,
            lockout_minutes: 15,
            minimum_password_length: 8,
            allow_organization_creation: true,
            invitation_expiry_days: 7,
            password_reset_expiry_hours: 2,
            frontend_url: None,
        }
    }
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Worker {
    pub default_health_server_port: u16,
    pub default_health_server_bind_address: String,
    pub threads: u32,
}

impl Default for Worker {
    fn default() -> Self {
        Self {
            default_health_server_port: 9090,
            default_health_server_bind_address: "127.0.0.1".into(),
            threads: 2,
        }
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmtpServer {
    pub default_port: u16,
    pub default_bind_address: String,
    pub default_health_server_port: u16,
    pub default_health_server_bind_address: String,
    pub tls_enabled: bool,
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
}

impl Default for SmtpServer {
    fn default() -> Self {
        Self {
            default_port: 25,
            default_bind_address: "::".into(),
            default_health_server_port: 9091,
            default_health_server_bind_address: "127.0.0.1".into(),
            tls_enabled: false,
            tls_certificate_path: "$config-file-root/smtp.cert".into(),
            tls_private_key_path: "$config-file-root/smtp.key".into(),
            tls_ciphers: None,
            ssl_version: "SSLv23".into(),
            proxy_protocol: false,
            log_connections: false,
            max_message_size: 14,
            log_ip_address_exclusion_matcher: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dns {
    pub mx_records: Vec<String>,
    pub spf_include: String,
    pub return_path_domain: String,
    pub route_domain: String,
    pub track_domain: String,
    pub helo_hostname: Option<String>,
    pub dkim_identifier: String,
    pub domain_verify_prefix: String,
    pub custom_return_path_prefix: String,
    pub timeout: u32,
    pub resolv_conf_path: String,
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
            route_domain: "routes.postal.example.com".into(),
            track_domain: "track.postal.example.com".into(),
            helo_hostname: None,
            dkim_identifier: "postal".into(),
            domain_verify_prefix: "postal-verification".into(),
            custom_return_path_prefix: "psrp".into(),
            timeout: 5,
            resolv_conf_path: "/etc/resolv.conf".into(),
        }
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SmtpClient {
    pub open_timeout: u32,
    pub read_timeout: u32,
}

impl Default for SmtpClient {
    fn default() -> Self {
        Self {
            open_timeout: 30,
            read_timeout: 30,
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

/// The complete CamelMailer configuration.
///
/// Unknown top-level groups (e.g. `rails`, `rspamd`, `oidc` from a legacy
/// `postal.yml`) are ignored so a legacy file loads without modification;
/// unknown keys *within* a known group are an error, mirroring the strictness
/// of the Konfig schema.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(alias = "postal")]
    pub camelmailer: CamelMailer,
    pub web_server: WebServer,
    pub auth: Auth,
    pub oidc: Oidc,
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

    pub fn validate(&self) -> Result<(), ConfigError> {
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
        if self.auth.minimum_password_length < 8 {
            return Err(ConfigError::Invalid(
                "auth.minimum_password_length must be at least 8".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert_eq!(config.main_db.host, "localhost");
        assert_eq!(config.main_db.port, 3306);
        assert_eq!(config.main_db.database, "postal");
        assert_eq!(config.message_db.database_name_prefix, "postal");

        assert!(config.logging.enabled);
        assert!(!config.logging.rails_log_enabled);

        assert_eq!(config.smtp_server.default_port, 25);
        assert_eq!(config.smtp_server.default_bind_address, "::");
        assert!(!config.smtp_server.tls_enabled);
        assert_eq!(config.smtp_server.max_message_size, 14);

        assert_eq!(
            config.dns.mx_records,
            vec!["mx1.postal.example.com", "mx2.postal.example.com"]
        );
        assert_eq!(config.dns.return_path_domain, "rp.postal.example.com");
        assert_eq!(config.dns.route_domain, "routes.postal.example.com");
        assert_eq!(config.dns.custom_return_path_prefix, "psrp");
        assert_eq!(config.dns.dkim_identifier, "postal");

        assert_eq!(config.smtp.port, 25);
        assert_eq!(config.smtp.authentication_type, "login");
        assert!(config.smtp.enable_starttls_auto);
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
        assert_eq!(config.auth.invitation_expiry_days, 7);
        assert_eq!(config.auth.password_reset_expiry_hours, 2);
        assert_eq!(config.auth.frontend_url, None);
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
        let config =
            Config::from_yaml("oidc:\n  enabled: true\n  issuer: https://x\n").unwrap();
        assert!(config.validate().is_err());
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
}
