//! Outbound-request address guard (SSRF protection).
//!
//! Webhook URLs and HTTP route endpoints are tenant-controlled. Without a
//! guard a tenant can point one at `http://169.254.169.254/…` (the cloud
//! metadata service), `http://127.0.0.1/…` or any internal host and have the
//! worker fetch it — and, for webhooks, read the first 2 KB of the reply back
//! out of the audit log (a semi-blind SSRF read primitive). This mirrors the
//! address guard Postal shipped in v3.3.7.
//!
//! Before making either request the guard parses the URL, resolves its host,
//! and rejects it when *any* resolved address (or a literal-IP host) is
//! loopback, private (RFC1918), link-local, unique-local (`fc00::/7`),
//! IPv4-mapped / IPv4-compatible IPv6, or otherwise non-global. A self-hoster
//! who deliberately targets internal endpoints can opt specific hosts back in
//! via `camelmailer.outbound_allowed_hosts`, or disable the guard entirely
//! with `camelmailer.outbound_ssrf_protection: false`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Reasons the guard refuses to send a request.
#[derive(Debug, thiserror::Error)]
pub enum SsrfError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("URL has no host")]
    NoHost,
    #[error("could not resolve host {host}: {source}")]
    Resolve {
        host: String,
        source: std::io::Error,
    },
    #[error("host {host} did not resolve to any address")]
    Unresolved { host: String },
    #[error("destination {host} maps to a non-global address ({ip})")]
    Blocked { host: String, ip: IpAddr },
}

/// Address guard applied to every outbound webhook / route-endpoint request.
#[derive(Debug, Clone)]
pub struct SsrfGuard {
    enabled: bool,
    /// Hosts (domains or IP literals) that bypass the guard even when they
    /// resolve to a non-global address. Compared case-insensitively.
    allowed_hosts: Vec<String>,
}

impl SsrfGuard {
    pub fn new(enabled: bool, allowed_hosts: Vec<String>) -> Self {
        Self {
            enabled,
            allowed_hosts: allowed_hosts
                .into_iter()
                .map(|h| h.to_lowercase())
                .collect(),
        }
    }

    /// Build the guard from the `camelmailer` config group.
    pub fn from_config(config: &camelmailer_config::Config) -> Self {
        Self::new(
            config.camelmailer.outbound_ssrf_protection,
            config.camelmailer.outbound_allowed_hosts.clone(),
        )
    }

    fn is_allowlisted(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        self.allowed_hosts.contains(&host)
    }

    /// Reject the URL when its host resolves to (or literally is) a non-global
    /// address. `Ok(())` means the request is safe to send.
    pub async fn check_url(&self, url: &str) -> Result<(), SsrfError> {
        if !self.enabled {
            return Ok(());
        }

        let parsed =
            reqwest::Url::parse(url).map_err(|error| SsrfError::InvalidUrl(error.to_string()))?;
        let raw_host = parsed.host_str().ok_or(SsrfError::NoHost)?;
        // `host_str` serialises IPv6 hosts with brackets (`[::1]`); strip them
        // so the literal-IP parse below sees a bare address.
        let host = raw_host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(raw_host);

        if self.is_allowlisted(host) {
            return Ok(());
        }

        // Literal-IP host: classify directly, no DNS.
        if let Ok(ip) = host.parse::<IpAddr>() {
            if ip_blocked(ip) {
                return Err(SsrfError::Blocked {
                    host: host.to_string(),
                    ip,
                });
            }
            return Ok(());
        }

        // Domain host: resolve and classify every address it maps to. Any
        // non-global address is fatal, so a name that resolves to a mix of
        // public and private addresses is still refused.
        let port = parsed.port_or_known_default().unwrap_or(80);
        let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|source| SsrfError::Resolve {
                host: host.to_string(),
                source,
            })?
            .collect();

        if addrs.is_empty() {
            return Err(SsrfError::Unresolved {
                host: host.to_string(),
            });
        }
        for addr in addrs {
            let ip = addr.ip();
            if ip_blocked(ip) {
                return Err(SsrfError::Blocked {
                    host: host.to_string(),
                    ip,
                });
            }
        }
        Ok(())
    }
}

/// True when `ip` is anything other than a globally routable unicast address.
pub fn ip_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_blocked(v4),
        IpAddr::V6(v6) => ipv6_blocked(v6),
    }
}

fn ipv4_blocked(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()                    // 127.0.0.0/8
        || ip.is_private()              // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local()           // 169.254.0.0/16
        || ip.is_broadcast()            // 255.255.255.255
        || ip.is_documentation()        // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || ip.is_unspecified()          // 0.0.0.0
        || ip.is_multicast()            // 224.0.0.0/4
        || a == 0                       // 0.0.0.0/8 "this network"
        || (a == 100 && (64..=127).contains(&b)) // 100.64.0.0/10 CGNAT / shared
        || (a == 192 && b == 0)         // 192.0.0.0/24 IETF protocol assignments
        || (a == 198 && (b == 18 || b == 19)) // 198.18.0.0/15 benchmarking
        || a >= 240 // 240.0.0.0/4 reserved (excludes 255.255.255.255, already caught)
}

fn ipv6_blocked(ip: Ipv6Addr) -> bool {
    // IPv4-mapped (`::ffff:a.b.c.d`) and IPv4-compatible (`::a.b.c.d`) forms are
    // a classic guard-bypass vector; reject them outright.
    if ip.to_ipv4_mapped().is_some() || is_ipv4_compatible(ip) {
        return true;
    }
    ip.is_loopback()                     // ::1
        || ip.is_unspecified()           // ::
        || ip.is_multicast()             // ff00::/8
        || is_unique_local(ip)           // fc00::/7
        || is_unicast_link_local(ip)     // fe80::/10
        || is_documentation_v6(ip) // 2001:db8::/32
}

/// `::a.b.c.d` (top 96 bits zero) — but not `::` or `::1`, which the caller
/// already rejects as unspecified / loopback.
fn is_ipv4_compatible(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 && s[6] != 0
}

fn is_unique_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn is_unicast_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

fn is_documentation_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    s[0] == 0x2001 && s[1] == 0x0db8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        s.parse().unwrap()
    }
    fn v6(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn loopback_is_blocked() {
        assert!(ip_blocked(v4("127.0.0.1")));
        assert!(ip_blocked(v4("127.10.20.30")));
        assert!(ip_blocked(v6("::1")));
    }

    #[test]
    fn private_ranges_are_blocked() {
        assert!(ip_blocked(v4("10.0.0.1")));
        assert!(ip_blocked(v4("172.16.5.4")));
        assert!(ip_blocked(v4("172.31.255.255")));
        assert!(ip_blocked(v4("192.168.1.1")));
    }

    #[test]
    fn link_local_is_blocked() {
        // includes the cloud metadata service address
        assert!(ip_blocked(v4("169.254.169.254")));
        assert!(ip_blocked(v6("fe80::1")));
    }

    #[test]
    fn unique_local_v6_is_blocked() {
        assert!(ip_blocked(v6("fc00::1")));
        assert!(ip_blocked(v6("fd12:3456:789a::1")));
    }

    #[test]
    fn ipv4_mapped_and_compatible_v6_are_blocked() {
        // ::ffff:169.254.169.254 and ::ffff:8.8.8.8 both rejected
        assert!(ip_blocked(v6("::ffff:169.254.169.254")));
        assert!(ip_blocked("::ffff:8.8.8.8".parse().unwrap()));
        // IPv4-compatible form
        assert!(ip_blocked(v6("::93.184.216.34")));
    }

    #[test]
    fn other_non_global_ranges_are_blocked() {
        assert!(ip_blocked(v4("0.0.0.0")));
        assert!(ip_blocked(v4("100.64.0.1"))); // CGNAT
        assert!(ip_blocked(v4("240.0.0.1"))); // reserved
        assert!(ip_blocked(v4("255.255.255.255"))); // broadcast
        assert!(ip_blocked(v4("224.0.0.1"))); // multicast
    }

    #[test]
    fn normal_public_addresses_are_allowed() {
        assert!(!ip_blocked(v4("8.8.8.8")));
        assert!(!ip_blocked(v4("1.1.1.1")));
        assert!(!ip_blocked(v4("93.184.216.34")));
        assert!(!ip_blocked(v6("2606:4700:4700::1111")));
        assert!(!ip_blocked(v6("2001:4860:4860::8888")));
    }

    #[tokio::test]
    async fn check_url_rejects_literal_private_hosts() {
        let guard = SsrfGuard::new(true, vec![]);
        assert!(guard.check_url("http://127.0.0.1/hook").await.is_err());
        assert!(guard
            .check_url("http://169.254.169.254/latest")
            .await
            .is_err());
        assert!(guard.check_url("http://[::1]:8080/x").await.is_err());
        assert!(guard.check_url("http://10.1.2.3/x").await.is_err());
    }

    #[tokio::test]
    async fn check_url_allows_literal_public_hosts() {
        let guard = SsrfGuard::new(true, vec![]);
        assert!(guard.check_url("https://8.8.8.8/x").await.is_ok());
        assert!(guard
            .check_url("https://[2606:4700:4700::1111]/x")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn allowlist_bypasses_the_guard() {
        let guard = SsrfGuard::new(true, vec!["127.0.0.1".into()]);
        assert!(guard.check_url("http://127.0.0.1:9000/hook").await.is_ok());
        // a host not on the allowlist is still checked
        assert!(guard.check_url("http://10.0.0.1/hook").await.is_err());
    }

    #[tokio::test]
    async fn disabled_guard_allows_everything() {
        let guard = SsrfGuard::new(false, vec![]);
        assert!(guard.check_url("http://127.0.0.1/hook").await.is_ok());
        assert!(guard.check_url("http://169.254.169.254/x").await.is_ok());
    }

    #[tokio::test]
    async fn invalid_and_hostless_urls_are_rejected() {
        let guard = SsrfGuard::new(true, vec![]);
        assert!(guard.check_url("not a url").await.is_err());
    }
}
