//! Outbound delivery target resolution and sending — the port of
//! `app/senders/smtp_sender.rb`.
//!
//! Targets are resolved in this order:
//! 1. configured SMTP relays (`camelmailer.smtp_relays`)
//! 2. the destination domain's MX records (by preference)
//! 3. the destination domain itself on port 25 (implicit-MX fallback)
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
    pub auth: Option<SmtpAuth>,
}

impl Endpoint {
    /// A plain MX endpoint: port 25, opportunistic STARTTLS, no auth.
    fn mx(host: String) -> Self {
        Self {
            host,
            port: 25,
            security: ConnectionSecurity::Opportunistic,
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

fn parse_relay(relay: &str) -> Option<Endpoint> {
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
        auth,
    })
}

pub struct SmtpSender {
    relays: Vec<Endpoint>,
    helo_hostname: String,
    timeout: Duration,
    tls_mode: TlsMode,
}

impl SmtpSender {
    pub fn new(config: &camelmailer_config::Config) -> Self {
        let relays = config
            .camelmailer
            .smtp_relays
            .iter()
            .filter_map(|relay| parse_relay(relay))
            .collect();
        let helo_hostname = config
            .dns
            .helo_hostname
            .clone()
            .unwrap_or_else(|| config.camelmailer.smtp_hostname.clone());
        // Opportunistic STARTTLS on outbound delivery, honoring the
        // configured verification mode. enable_starttls_auto keeps it on by
        // default; enable_starttls forces it (still opportunistic here since
        // we cannot require what a remote MX does not offer).
        let tls_mode = TlsMode::from_verify_mode(
            &config.smtp.openssl_verify_mode,
            config.smtp.enable_starttls || config.smtp.enable_starttls_auto,
        );
        Self {
            relays,
            helo_hostname,
            timeout: Duration::from_secs(config.smtp_client.open_timeout as u64),
            tls_mode,
        }
    }

    /// Resolve the delivery endpoints for a destination domain.
    pub async fn resolve_endpoints(&self, domain: &str) -> Vec<Endpoint> {
        if !self.relays.is_empty() {
            return self.relays.clone();
        }
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
                tls_mode: self.tls_mode,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relay_urls() {
        assert_eq!(
            parse_relay("smtp://relay.example:2525"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 2525,
                security: ConnectionSecurity::Opportunistic,
                auth: None,
            })
        );
        assert_eq!(
            parse_relay("smtp://relay.example"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 25,
                security: ConnectionSecurity::Opportunistic,
                auth: None,
            })
        );
        assert_eq!(
            parse_relay("smtp://relay.example:25?ssl_mode=Auto"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 25,
                security: ConnectionSecurity::Opportunistic,
                auth: None,
            })
        );
        assert_eq!(parse_relay("not-a-relay"), None);
    }

    #[test]
    fn submission_port_requires_starttls() {
        assert_eq!(
            parse_relay("smtp://relay.example:587"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 587,
                security: ConnectionSecurity::RequireStartTls,
                auth: None,
            })
        );
    }

    #[test]
    fn smtps_scheme_means_implicit_tls_and_defaults_to_465() {
        assert_eq!(
            parse_relay("smtps://relay.example"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 465,
                security: ConnectionSecurity::ImplicitTls,
                auth: None,
            })
        );
        assert_eq!(
            parse_relay("smtps://relay.example:8465"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 8465,
                security: ConnectionSecurity::ImplicitTls,
                auth: None,
            })
        );
    }

    #[test]
    fn userinfo_becomes_auth_plain_credentials() {
        assert_eq!(
            parse_relay("smtp://mailer:s3cret@relay.example:587"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 587,
                security: ConnectionSecurity::RequireStartTls,
                auth: Some(SmtpAuth {
                    username: "mailer".into(),
                    password: "s3cret".into(),
                }),
            })
        );
        // Percent-encoded special characters in user/pass.
        assert_eq!(
            parse_relay("smtps://user%40example.com:p%40ss%3Aword@relay.example:465"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 465,
                security: ConnectionSecurity::ImplicitTls,
                auth: Some(SmtpAuth {
                    username: "user@example.com".into(),
                    password: "p@ss:word".into(),
                }),
            })
        );
        // Userinfo without a password is not a valid relay.
        assert_eq!(parse_relay("smtp://user-only@relay.example:587"), None);
    }
}
