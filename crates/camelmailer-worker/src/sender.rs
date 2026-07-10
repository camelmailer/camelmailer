//! Outbound delivery target resolution and sending — the port of
//! `app/senders/smtp_sender.rb`.
//!
//! Targets are resolved in this order:
//! 1. configured SMTP relays (`camelmailer.smtp_relays`, `smtp://host:port`)
//! 2. the destination domain's MX records (by preference)
//! 3. the destination domain itself on port 25 (implicit-MX fallback)

use crate::smtp_client::{self, SendOutcome, SendParams, TlsMode};
use std::net::IpAddr;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

fn parse_relay(relay: &str) -> Option<Endpoint> {
    let rest = relay.strip_prefix("smtp://")?;
    let rest = rest.split(['/', '?']).next().unwrap_or(rest);
    match rest.rsplit_once(':') {
        Some((host, port)) => Some(Endpoint {
            host: host.to_string(),
            port: port.parse().ok()?,
        }),
        None => Some(Endpoint {
            host: rest.to_string(),
            port: 25,
        }),
    }
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
                        .map(|mx| Endpoint {
                            host: mx.exchange().to_utf8().trim_end_matches('.').to_string(),
                            port: 25,
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
    Endpoint {
        host: domain.to_string(),
        port: 25,
    }
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
                port: 2525
            })
        );
        assert_eq!(
            parse_relay("smtp://relay.example"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 25
            })
        );
        assert_eq!(
            parse_relay("smtp://relay.example:25?ssl_mode=Auto"),
            Some(Endpoint {
                host: "relay.example".into(),
                port: 25
            })
        );
        assert_eq!(parse_relay("not-a-relay"), None);
    }
}
