//! Outbound delivery target resolution and sending — the port of
//! `app/senders/smtp_sender.rb`.
//!
//! Targets are resolved in this order:
//! 1. configured SMTP relays (`camelmailer.smtp_relays`)
//! 2. the destination domain's MX records (by preference)
//! 3. the destination domain itself on port 25 (implicit-MX fallback)
//!
//! Certificate verification differs by target. **Direct-MX** endpoints use
//! opportunistic TLS *without* verification (`TlsMode::AcceptAny`): a foreign
//! MX presents a certificate for its own name/issuer, not ours, so requiring
//! a webpki trust chain would fail against essentially every real MX (the
//! Microsoft/Outlook `UnknownIssuer` soft-fail loop). `smtp.openssl_verify_mode`
//! therefore governs *only* configured relays, whose identity is known.
//!
//! Relay URLs express port, TLS mode and credentials:
//! - `smtp://host:25` — plaintext + opportunistic STARTTLS
//! - `smtp://host:587` — submission: STARTTLS is *mandatory* (a failure
//!   instead of a plaintext fallback when the relay does not offer it)
//! - `smtps://host:465` — implicit TLS from the first byte
//! - `smtp://user:pass@host:587` — AUTH PLAIN after the TLS handshake
//!   (special characters in user/pass can be percent-encoded)

use crate::smtp_client::{self, ConnectionSecurity, SendOutcome, SendParams, SmtpAuth, TlsMode};
use std::net::IpAddr;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub security: ConnectionSecurity,
    /// How the remote certificate is treated during TLS. Direct-MX endpoints
    /// are always [`TlsMode::AcceptAny`] (opportunistic, unverified);
    /// configured relays derive this from `smtp.openssl_verify_mode`.
    pub tls_mode: TlsMode,
    pub auth: Option<SmtpAuth>,
}

impl Endpoint {
    /// A plain MX endpoint: port 25, opportunistic STARTTLS, no auth.
    ///
    /// TLS is opportunistic and the certificate is **not** verified: a
    /// foreign MX's certificate is not issued for our benefit, so verifying
    /// it against webpki roots would fail on virtually every real MX (the
    /// Outlook `UnknownIssuer` bug). We still encrypt when STARTTLS is
    /// offered, and the client falls back to plaintext on a handshake error.
    fn mx(host: String) -> Self {
        Self {
            host,
            port: 25,
            security: ConnectionSecurity::Opportunistic,
            tls_mode: TlsMode::AcceptAny,
            auth: None,
        }
    }
}

/// Minimal percent-decoding for the userinfo part of a relay URL (so
/// passwords may contain `@`, `:` etc. as `%40`, `%3A`, …).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match (bytes[index], bytes.get(index + 1..index + 3)) {
            (b'%', Some(hex)) => {
                if let Ok(value) = u8::from_str_radix(std::str::from_utf8(hex).unwrap_or(""), 16) {
                    out.push(value);
                    index += 3;
                    continue;
                }
                out.push(b'%');
                index += 1;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a relay URL into an [`Endpoint`]. `relay_tls_mode` is the
/// verification mode configured for relays (derived from
/// `smtp.openssl_verify_mode`) and applies to STARTTLS/implicit-TLS relays —
/// unlike direct-MX endpoints, a configured relay has a known identity worth
/// verifying.
fn parse_relay(relay: &str, relay_tls_mode: TlsMode) -> Option<Endpoint> {
    let (rest, implicit_tls) = match relay.strip_prefix("smtps://") {
        Some(rest) => (rest, true),
        None => (relay.strip_prefix("smtp://")?, false),
    };
    let rest = rest.split(['/', '?']).next().unwrap_or(rest);
    let (auth, host_port) = match rest.rsplit_once('@') {
        Some((userinfo, host_port)) => {
            let (username, password) = userinfo.split_once(':')?;
            (
                Some(SmtpAuth {
                    username: percent_decode(username),
                    password: percent_decode(password),
                }),
                host_port,
            )
        }
        None => (None, rest),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse().ok()?),
        None => (host_port.to_string(), if implicit_tls { 465 } else { 25 }),
    };
    let security = if implicit_tls {
        ConnectionSecurity::ImplicitTls
    } else if port == 587 {
        // Submission: STARTTLS is mandatory, never a plaintext fallback.
        ConnectionSecurity::RequireStartTls
    } else {
        ConnectionSecurity::Opportunistic
    };
    Some(Endpoint {
        host,
        port,
        security,
        tls_mode: relay_tls_mode,
        auth,
    })
}

pub struct SmtpSender {
    relays: Vec<Endpoint>,
    helo_hostname: String,
    timeout: Duration,
}

impl SmtpSender {
    pub fn new(config: &camelmailer_config::Config) -> Self {
        // `smtp.openssl_verify_mode` governs certificate verification for
        // *configured relays* only (a smarthost has a known identity):
        // `peer` -> Verify, `none` -> AcceptAny. Direct-MX endpoints ignore
        // it and always use opportunistic AcceptAny (see `Endpoint::mx`).
        // enable_starttls_auto keeps STARTTLS on by default; enable_starttls
        // forces it (still opportunistic for :25 relays, since we cannot
        // require what a relay does not offer — :587 handles the mandatory case).
        let relay_tls_mode = TlsMode::from_verify_mode(
            &config.smtp.openssl_verify_mode,
            config.smtp.enable_starttls || config.smtp.enable_starttls_auto,
        );
        let relays = config
            .camelmailer
            .smtp_relays
            .iter()
            .filter_map(|relay| parse_relay(relay, relay_tls_mode))
            .collect();
        let helo_hostname = config
            .dns
            .helo_hostname
            .clone()
            .unwrap_or_else(|| config.camelmailer.smtp_hostname.clone());
        Self {
            relays,
            helo_hostname,
            timeout: Duration::from_secs(config.smtp_client.open_timeout as u64),
        }
    }

    /// Resolve the delivery endpoints for a destination domain.
    pub async fn resolve_endpoints(&self, domain: &str) -> Vec<Endpoint> {
        if !self.relays.is_empty() {
            return self.relays.clone();
        }
        // IDNA/Punycode-encode before any DNS work: a Unicode domain such as
        // `münchen.de` has no MX records under its Unicode form — it must be
        // looked up (and connected to) as its ASCII-compatible
        // `xn--mnchen-3ya.de` form.
        let domain = to_ascii_domain(domain);
        let domain = domain.as_str();
        match hickory_resolver::TokioAsyncResolver::tokio_from_system_conf() {
            Ok(resolver) => match resolver.mx_lookup(format!("{domain}.")).await {
                Ok(lookup) => {
                    let mut records: Vec<_> = lookup.iter().collect();
                    records.sort_by_key(|mx| mx.preference());
                    let endpoints: Vec<Endpoint> = records
                        .iter()
                        .map(|mx| {
                            Endpoint::mx(mx.exchange().to_utf8().trim_end_matches('.').to_string())
                        })
                        .collect();
                    if endpoints.is_empty() {
                        vec![implicit_mx(domain)]
                    } else {
                        endpoints
                    }
                }
                Err(_) => vec![implicit_mx(domain)],
            },
            Err(_) => vec![implicit_mx(domain)],
        }
    }

    /// Try each endpoint in order until one accepts (or hard-rejects) the
    /// message. Soft failures fall through to the next endpoint. `source_ip`
    /// binds the local socket for IP-pool-aware delivery.
    pub async fn send(
        &self,
        domain: &str,
        mail_from: &str,
        rcpt_to: &str,
        raw_message: &[u8],
        source_ip: Option<IpAddr>,
    ) -> SendOutcome {
        let endpoints = self.resolve_endpoints(domain).await;
        let mut last = SendOutcome::SoftFail {
            response: format!("no delivery endpoints for {domain}"),
        };
        for endpoint in endpoints {
            let params = SendParams {
                host: endpoint.host.clone(),
                port: endpoint.port,
                helo_hostname: self.helo_hostname.clone(),
                mail_from: mail_from.to_string(),
                rcpt_to: rcpt_to.to_string(),
                timeout: self.timeout,
                tls_mode: endpoint.tls_mode,
                security: endpoint.security,
                auth: endpoint.auth.clone(),
                source_ip,
            };
            let outcome = smtp_client::send_message(&params, raw_message).await;
            match outcome {
                SendOutcome::SoftFail { .. } => last = outcome,
                terminal => return terminal,
            }
        }
        last
    }
}

fn implicit_mx(domain: &str) -> Endpoint {
    Endpoint::mx(domain.to_string())
}

/// IDNA/Punycode-encode a destination domain for DNS/MX resolution. A Unicode
/// domain like `münchen.de` must be resolved and connected to as its
/// ASCII-compatible `xn--mnchen-3ya.de` form; ASCII domains pass through
/// (lower-cased). If encoding fails we fall back to the input so delivery
/// still attempts and fails visibly downstream rather than silently vanishing.
fn to_ascii_domain(domain: &str) -> String {
    idna::domain_to_ascii(domain).unwrap_or_else(|_| domain.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relay_urls() {
        assert_eq!(
            parse_relay("smtp://relay.example:2525", TlsMode::Verify),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 2525,
                security: ConnectionSecurity::Opportunistic,
                tls_mode: TlsMode::Verify,
                auth: None,
            })
        );
        assert_eq!(
            parse_relay("smtp://relay.example", TlsMode::Verify),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 25,
                security: ConnectionSecurity::Opportunistic,
                tls_mode: TlsMode::Verify,
                auth: None,
            })
        );
        assert_eq!(
            parse_relay("smtp://relay.example:25?ssl_mode=Auto", TlsMode::Verify),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 25,
                security: ConnectionSecurity::Opportunistic,
                tls_mode: TlsMode::Verify,
                auth: None,
            })
        );
        assert_eq!(parse_relay("not-a-relay", TlsMode::Verify), None);
    }

    #[test]
    fn submission_port_requires_starttls() {
        assert_eq!(
            parse_relay("smtp://relay.example:587", TlsMode::Verify),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 587,
                security: ConnectionSecurity::RequireStartTls,
                tls_mode: TlsMode::Verify,
                auth: None,
            })
        );
    }

    #[test]
    fn smtps_scheme_means_implicit_tls_and_defaults_to_465() {
        assert_eq!(
            parse_relay("smtps://relay.example", TlsMode::Verify),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 465,
                security: ConnectionSecurity::ImplicitTls,
                tls_mode: TlsMode::Verify,
                auth: None,
            })
        );
        assert_eq!(
            parse_relay("smtps://relay.example:8465", TlsMode::Verify),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 8465,
                security: ConnectionSecurity::ImplicitTls,
                tls_mode: TlsMode::Verify,
                auth: None,
            })
        );
    }

    #[test]
    fn userinfo_becomes_auth_plain_credentials() {
        assert_eq!(
            parse_relay("smtp://mailer:s3cret@relay.example:587", TlsMode::Verify),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 587,
                security: ConnectionSecurity::RequireStartTls,
                tls_mode: TlsMode::Verify,
                auth: Some(SmtpAuth {
                    username: "mailer".into(),
                    password: "s3cret".into(),
                }),
            })
        );
        // Percent-encoded special characters in user/pass.
        assert_eq!(
            parse_relay(
                "smtps://user%40example.com:p%40ss%3Aword@relay.example:465",
                TlsMode::Verify
            ),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 465,
                security: ConnectionSecurity::ImplicitTls,
                tls_mode: TlsMode::Verify,
                auth: Some(SmtpAuth {
                    username: "user@example.com".into(),
                    password: "p@ss:word".into(),
                }),
            })
        );
        // Userinfo without a password is not a valid relay.
        assert_eq!(
            parse_relay("smtp://user-only@relay.example:587", TlsMode::Verify),
            None
        );
    }

    // Regression ("Outlook UnknownIssuer"): direct-MX endpoints must never
    // verify the remote certificate — they carry opportunistic AcceptAny,
    // independent of `smtp.openssl_verify_mode`. Verifying a foreign MX's
    // cert against webpki roots was what soft-failed all Microsoft/Outlook
    // (and most other) delivery forever.
    #[test]
    fn direct_mx_endpoints_never_verify_the_certificate() {
        let endpoint = Endpoint::mx("mx.outlook.com".into());
        assert_eq!(endpoint.port, 25);
        assert_eq!(endpoint.security, ConnectionSecurity::Opportunistic);
        assert_eq!(endpoint.tls_mode, TlsMode::AcceptAny);
    }

    // IDN destination domains must be Punycode-encoded before the MX lookup:
    // a Unicode domain has no DNS records under its raw Unicode form.
    #[test]
    fn idn_domains_are_punycode_encoded_before_lookup() {
        assert_eq!(to_ascii_domain("münchen.de"), "xn--mnchen-3ya.de");
        assert_eq!(to_ascii_domain("例え.jp"), "xn--r8jz45g.jp");
        // ASCII domains pass through (lower-cased), never mangled.
        assert_eq!(to_ascii_domain("mx.example.com"), "mx.example.com");
        assert_eq!(to_ascii_domain("Example.COM"), "example.com");
    }

    // `openssl_verify_mode` governs configured relays only: `peer` verifies a
    // smarthost's certificate, `none` accepts any. This is orthogonal to the
    // direct-MX behaviour above.
    #[test]
    fn relay_verify_mode_follows_openssl_verify_mode() {
        assert_eq!(
            parse_relay("smtp://smarthost.example:587", TlsMode::Verify)
                .unwrap()
                .tls_mode,
            TlsMode::Verify
        );
        assert_eq!(
            parse_relay("smtp://smarthost.example:587", TlsMode::AcceptAny)
                .unwrap()
                .tls_mode,
            TlsMode::AcceptAny
        );
    }
}
