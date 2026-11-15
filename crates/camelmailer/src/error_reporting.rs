//! Optional error reporting to Sentry or a Sentry-compatible tracker
//! (the `sentry` cargo feature, on by default).
//!
//! Nothing happens without a DSN: no client is created, no tracing layer is
//! installed and no connection is ever opened. With a DSN, the long-running
//! roles (`web-server`, `smtp-server`, `worker`) report
//!
//! - panics (with a backtrace),
//! - `ERROR`-level tracing events, with the preceding `WARN` events as
//!   breadcrumbs,
//! - the error a role exits with.
//!
//! A mail platform logs recipients, senders and credentials in the course of
//! normal work, so every event and breadcrumb passes through [`scrub_event`]
//! / [`scrub_breadcrumb`] before it leaves the process: fields named like
//! mail content, addresses or secrets are replaced, e-mail addresses and
//! URL credentials inside any remaining text are masked, and request and
//! user data is dropped. `send_default_pii` stays off, and span fields are
//! never attached. Performance tracing is not enabled.

use regex::Regex;
use sentry::integrations::tracing::{BreadcrumbLayer, ErrorLayer};
use sentry::protocol::{Breadcrumb, Context, Event, Map, Value};
use std::sync::LazyLock;
use tracing::Level;
use tracing_subscriber::filter::{filter_fn, LevelFilter};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// The roles that report errors. The one-shot commands (`initialize`,
/// `make-user`, `make-admin-api-key`, ...) never do: they print their
/// result to the operator, and two of them handle fresh credentials.
const REPORTING_ROLES: &[&str] = &["web-server", "smtp-server", "worker"];

/// What replaces a value that must not leave the process.
const REDACTED: &str = "[redacted]";

/// Resolved error-reporting settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub dsn: String,
    pub environment: String,
    pub sample_rate: f32,
}

impl Settings {
    /// Resolves the settings from environment variables (`SENTRY_DSN`,
    /// `SENTRY_ENVIRONMENT`, `SENTRY_SAMPLE_RATE`), falling back to the DSN
    /// of the config file (`logging.sentry_dsn`). An empty value counts as
    /// unset. `Ok(None)` means reporting is off; `Err` names a setting that
    /// is present but unusable.
    pub fn resolve(
        env: impl Fn(&str) -> Option<String>,
        config_dsn: Option<&str>,
    ) -> Result<Option<Self>, String> {
        let set = |name: &str| {
            env(name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let dsn = match set("SENTRY_DSN").or_else(|| {
            config_dsn
                .map(str::trim)
                .filter(|dsn| !dsn.is_empty())
                .map(str::to_string)
        }) {
            Some(dsn) => dsn,
            None => return Ok(None),
        };
        // Never echo the DSN: its key is what lets anyone submit events.
        dsn.parse::<sentry::types::Dsn>()
            .map_err(|error| format!("the Sentry DSN is not valid ({error})"))?;
        let sample_rate = match set("SENTRY_SAMPLE_RATE") {
            None => 1.0,
            Some(raw) => match raw.parse::<f32>() {
                Ok(rate) if (0.0..=1.0).contains(&rate) => rate,
                _ => {
                    return Err(format!(
                        "SENTRY_SAMPLE_RATE must be a number from 0 to 1, got {raw:?}"
                    ))
                }
            },
        };
        Ok(Some(Self {
            dsn,
            environment: set("SENTRY_ENVIRONMENT").unwrap_or_else(|| "production".to_string()),
            sample_rate,
        }))
    }
}

/// The decision for one process, made before logging is set up so the
/// tracing layers are only installed when they have somewhere to send to.
#[derive(Debug)]
pub enum Plan {
    Disabled,
    Invalid(String),
    Enabled {
        role: &'static str,
        settings: Settings,
    },
}

impl Plan {
    /// The plan for the role named by the first CLI argument.
    pub fn for_role(role: Option<&str>) -> Self {
        let Some(role) = role.and_then(|role| REPORTING_ROLES.iter().find(|r| **r == role)) else {
            return Self::Disabled;
        };
        let env = |name: &str| std::env::var(name).ok();
        // The config file is only read when the environment has no DSN; the
        // role loads (and validates) it again on its own.
        let config_dsn = || {
            camelmailer_config::Config::load_from_env()
                .ok()
                .and_then(|config| config.logging.sentry_dsn)
        };
        let resolved = match env("SENTRY_DSN").filter(|dsn| !dsn.trim().is_empty()) {
            Some(_) => Settings::resolve(env, None),
            None => Settings::resolve(env, config_dsn().as_deref()),
        };
        match resolved {
            Ok(Some(settings)) => Self::Enabled { role, settings },
            Ok(None) => Self::Disabled,
            Err(reason) => Self::Invalid(reason),
        }
    }

    /// The tracing layers, or `None` (a no-op layer) when reporting is off.
    pub fn layers<S>(&self) -> Option<Vec<Box<dyn Layer<S> + Send + Sync + 'static>>>
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        matches!(self, Self::Enabled { .. }).then(layers)
    }

    /// Starts the client. Call after logging is set up and before the Tokio
    /// runtime starts, so every runtime thread inherits the client. The guard
    /// flushes pending events when it is dropped at the end of the process.
    pub fn start(self) -> Option<sentry::ClientInitGuard> {
        match self {
            Self::Disabled => None,
            Self::Invalid(reason) => {
                tracing::warn!("error reporting is off: {reason}");
                None
            }
            Self::Enabled { role, settings } => {
                // The transport uses rustls without a built-in provider; the
                // workspace uses ring everywhere.
                let _ = rustls::crypto::ring::default_provider().install_default();
                let guard = sentry::init((settings.dsn.as_str(), client_options(&settings)));
                sentry::configure_scope(|scope| scope.set_tag("role", role));
                tracing::info!(
                    environment = %settings.environment,
                    sample_rate = settings.sample_rate,
                    "error reporting enabled"
                );
                Some(guard)
            }
        }
    }
}

/// The client options: everything that decides what leaves the process.
pub fn client_options(settings: &Settings) -> sentry::ClientOptions {
    sentry::ClientOptions::new()
        .release(concat!("camelmailer@", env!("CARGO_PKG_VERSION")))
        .environment(settings.environment.clone())
        .sample_rate(settings.sample_rate)
        .traces_sample_rate(0.0)
        .send_default_pii(false)
        .max_breadcrumbs(50)
        .shutdown_timeout(std::time::Duration::from_secs(2))
        .before_send(|mut event| {
            scrub_event(&mut event);
            Some(event)
        })
        .before_breadcrumb(|mut breadcrumb| {
            scrub_breadcrumb(&mut breadcrumb);
            Some(breadcrumb)
        })
}

/// `WARN` events become breadcrumbs, `ERROR` events become Sentry events.
/// Span fields are not attached to either. A `Vec` rather than `and_then`:
/// per-layer filters are only honoured on layers added to the registry
/// one by one (or as a `Vec`); inside `and_then` the breadcrumb filter
/// never matches.
pub fn layers<S>() -> Vec<Box<dyn Layer<S> + Send + Sync + 'static>>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    vec![
        BreadcrumbLayer::new()
            .with_filter(filter_fn(|metadata| *metadata.level() == Level::WARN))
            .boxed(),
        ErrorLayer::new().with_filter(LevelFilter::ERROR).boxed(),
    ]
}

/// Reports the error a role exits with. A no-op without a client.
pub fn capture_fatal(error: &std::io::Error) {
    sentry::capture_error(error);
}

/// Field names whose value is dropped whole: mail content, addresses and
/// anything credential-like. Matched case-insensitively against the full
/// name.
const SENSITIVE_NAMES: &[&str] = &[
    "to",
    "cc",
    "bcc",
    "from",
    "rcpt",
    "rcpt_to",
    "mail_from",
    "reply_to",
    "sender",
    "recipient",
    "recipients",
    "address",
    "addresses",
    "email",
    "email_address",
    "subject",
    "body",
    "html",
    "text",
    "raw",
    "raw_message",
    "message_body",
    "headers",
    "header",
    "content",
    "attachment",
    "attachments",
    "username",
    "user_name",
    "login",
];

/// Substrings that mark a field name as sensitive wherever they appear.
const SENSITIVE_PARTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "private_key",
    "authorization",
    "cookie",
    "credential",
    "signature",
    "_body",
    "_address",
    "recipient",
    "rcpt",
];

fn is_sensitive(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SENSITIVE_NAMES.contains(&name.as_str()) || SENSITIVE_PARTS.iter().any(|p| name.contains(p))
}

static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<scheme>[A-Za-z][A-Za-z0-9+.-]*://)[^/\s@]+@").expect("valid regex")
});
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+")
        .expect("valid regex")
});
static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]+").expect("valid regex")
});

/// Masks credentials in URLs, bearer/basic tokens and e-mail addresses.
pub fn scrub_text(text: &str) -> String {
    let text = URL_CREDENTIALS.replace_all(text, "${scheme}[redacted]@");
    let text = BEARER.replace_all(&text, "$1 [redacted]");
    EMAIL.replace_all(&text, "[email]").into_owned()
}

fn scrub_value(value: &mut Value) {
    match value {
        Value::String(text) => *text = scrub_text(text),
        Value::Array(items) => items.iter_mut().for_each(scrub_value),
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_sensitive(key) {
                    *value = Value::String(REDACTED.into());
                } else {
                    scrub_value(value);
                }
            }
        }
        _ => {}
    }
}

fn scrub_map(map: &mut Map<String, Value>) {
    for (key, value) in map.iter_mut() {
        if is_sensitive(key) {
            *value = Value::String(REDACTED.into());
        } else {
            scrub_value(value);
        }
    }
}

fn scrub_option(text: &mut Option<String>) {
    if let Some(text) = text {
        *text = scrub_text(text);
    }
}

/// Scrubs everything in an event that can carry free text or field values.
pub fn scrub_event(event: &mut Event<'static>) {
    event.user = None;
    event.request = None;
    scrub_option(&mut event.message);
    scrub_option(&mut event.culprit);
    scrub_option(&mut event.transaction);
    if let Some(entry) = &mut event.logentry {
        entry.message = scrub_text(&entry.message);
        entry.params.iter_mut().for_each(scrub_value);
    }
    for exception in &mut event.exception.values {
        scrub_option(&mut exception.value);
    }
    for breadcrumb in &mut event.breadcrumbs.values {
        scrub_breadcrumb(breadcrumb);
    }
    for (key, value) in event.tags.iter_mut() {
        *value = if is_sensitive(key) {
            REDACTED.into()
        } else {
            scrub_text(value)
        };
    }
    scrub_map(&mut event.extra);
    for context in event.contexts.values_mut() {
        if let Context::Other(map) = context {
            scrub_map(map);
        }
    }
}

/// Scrubs a breadcrumb's message and data.
pub fn scrub_breadcrumb(breadcrumb: &mut Breadcrumb) {
    scrub_option(&mut breadcrumb.message);
    scrub_map(&mut breadcrumb.data);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tracing_subscriber::layer::SubscriberExt;

    const DSN: &str = "https://public@errors.example.com/22";

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn no_dsn_means_off() {
        assert_eq!(Settings::resolve(env(&[]), None), Ok(None));
        assert_eq!(
            Settings::resolve(env(&[("SENTRY_DSN", "  ")]), None),
            Ok(None)
        );
        assert_eq!(
            Settings::resolve(env(&[("SENTRY_DSN", "")]), Some("")),
            Ok(None)
        );
    }

    #[test]
    fn the_environment_wins_over_the_config_file() {
        let settings = Settings::resolve(env(&[("SENTRY_DSN", DSN)]), Some("not used"))
            .unwrap()
            .unwrap();
        assert_eq!(settings.dsn, DSN);
        assert_eq!(settings.environment, "production");
        assert_eq!(settings.sample_rate, 1.0);

        let from_file = Settings::resolve(env(&[("SENTRY_DSN", "")]), Some(DSN))
            .unwrap()
            .unwrap();
        assert_eq!(from_file.dsn, DSN);
    }

    #[test]
    fn environment_and_sample_rate_are_read() {
        let settings = Settings::resolve(
            env(&[
                ("SENTRY_DSN", DSN),
                ("SENTRY_ENVIRONMENT", "staging"),
                ("SENTRY_SAMPLE_RATE", "0.25"),
            ]),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(settings.environment, "staging");
        assert_eq!(settings.sample_rate, 0.25);
    }

    #[test]
    fn unusable_settings_are_refused_without_echoing_the_dsn() {
        let error =
            Settings::resolve(env(&[("SENTRY_DSN", "secret-but-not-a-dsn")]), None).unwrap_err();
        assert!(!error.contains("secret-but-not-a-dsn"), "{error}");
        for rate in ["2", "-0.1", "half"] {
            assert!(Settings::resolve(
                env(&[("SENTRY_DSN", DSN), ("SENTRY_SAMPLE_RATE", rate)]),
                None
            )
            .is_err());
        }
    }

    #[test]
    fn only_the_long_running_roles_report() {
        for role in [None, Some("version"), Some("make-user"), Some("initialize")] {
            assert!(matches!(Plan::for_role(role), Plan::Disabled), "{role:?}");
        }
    }

    #[test]
    fn text_loses_addresses_and_credentials() {
        assert_eq!(
            scrub_text("delivery to <jane.doe+x@example.co.uk> failed"),
            "delivery to <[email]> failed"
        );
        assert_eq!(
            scrub_text("connect postgres://app:hunter2@db.internal:5432/mail"),
            "connect postgres://[redacted]@db.internal:5432/mail"
        );
        assert_eq!(
            scrub_text("smtp://user%40x.com:pw@relay.example.com:587"),
            "smtp://[redacted]@relay.example.com:587"
        );
        assert_eq!(
            scrub_text("header Authorization: Bearer abc.DEF-123"),
            "header Authorization: Bearer [redacted]"
        );
        assert_eq!(scrub_text("queue row 42 lost"), "queue row 42 lost");
    }

    #[test]
    fn sensitive_fields_are_dropped_whole() {
        let mut map = Map::new();
        map.insert("rcpt_to".into(), Value::from("a@b.example"));
        map.insert("html_body".into(), Value::from("<p>hello</p>"));
        map.insert("smtp_password".into(), Value::from("pw"));
        map.insert("Subject".into(), Value::from("Your invoice"));
        map.insert("message_id".into(), Value::from(42));
        map.insert(
            "nested".into(),
            serde_json::json!({ "to": ["x@y.example"], "reason": "for x@y.example" }),
        );
        scrub_map(&mut map);
        assert_eq!(map["rcpt_to"], REDACTED);
        assert_eq!(map["html_body"], REDACTED);
        assert_eq!(map["smtp_password"], REDACTED);
        assert_eq!(map["Subject"], REDACTED);
        assert_eq!(map["message_id"], 42);
        assert_eq!(
            map["nested"],
            serde_json::json!({ "to": REDACTED, "reason": "for [email]" })
        );
    }

    /// End to end through the tracing layers and the client options: what
    /// an `error!` with a recipient field turns into.
    #[test]
    fn an_error_event_arrives_scrubbed() {
        let settings = Settings {
            dsn: DSN.into(),
            environment: "test".into(),
            sample_rate: 1.0,
        };
        let events = sentry::test::with_captured_events_options(
            || {
                let subscriber = tracing_subscriber::registry().with(layers());
                tracing::subscriber::with_default(subscriber, || {
                    tracing::info!(rcpt_to = "info@example.com", "not reported");
                    tracing::warn!("retrying delivery to bob@example.com");
                    tracing::error!(
                        rcpt_to = "alice@example.com",
                        message_id = 7,
                        "delivery for alice@example.com failed"
                    );
                });
            },
            client_options(&settings),
        );
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.level, sentry::Level::Error);
        assert_eq!(event.environment.as_deref(), Some("test"));
        assert_eq!(
            event.release.as_deref(),
            Some(concat!("camelmailer@", env!("CARGO_PKG_VERSION")))
        );
        let json = serde_json::to_string(event).unwrap();
        assert!(!json.contains("example.com"), "{json}");
        assert!(json.contains("delivery for [email] failed"), "{json}");
        assert!(json.contains("retrying delivery to [email]"), "{json}");
        assert!(!json.contains("not reported"), "{json}");
        assert!(event.user.is_none());
    }

    #[test]
    fn a_fatal_error_arrives_scrubbed() {
        let settings = Settings {
            dsn: DSN.into(),
            environment: "test".into(),
            sample_rate: 1.0,
        };
        let events = sentry::test::with_captured_events_options(
            || {
                let error = std::io::Error::other("rejected mallory@example.org");
                capture_fatal(&error);
            },
            client_options(&settings),
        );
        assert_eq!(events.len(), 1);
        let json = serde_json::to_string(&events[0]).unwrap();
        assert!(!json.contains("mallory"), "{json}");
    }
}
