//! Received header generation, ported from `app/lib/received_header.rb`.

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveMethod {
    Smtp,
    Http,
}

impl ReceiveMethod {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Smtp => "SMTP",
            Self::Http => "HTTP",
        }
    }
}

/// Format a timestamp like Ruby's `Time#rfc2822` (`Wed, 09 Jul 2026 12:34:56 +0000`).
pub fn rfc2822(time: DateTime<Utc>) -> String {
    time.format("%a, %d %b %Y %H:%M:%S +0000").to_string()
}

/// Generate a Received header.
///
/// `resolved_hostname` is the reverse-DNS name of `ip_address` (the caller
/// resolves it; in Ruby this was `DNSResolver.local.ip_to_hostname`). When
/// `privacy_mode` is enabled for the receiving server, the client details
/// (helo/hostname/ip) are omitted entirely.
pub fn generate(
    privacy_mode: bool,
    helo: &str,
    resolved_hostname: &str,
    ip_address: &str,
    method: ReceiveMethod,
    our_hostname: &str,
    now: DateTime<Utc>,
) -> String {
    let header = format!(
        "by {} with {}; {}",
        our_hostname,
        method.as_str(),
        rfc2822(now)
    );
    if privacy_mode {
        header
    } else {
        format!("from {helo} ({resolved_hostname} [{ip_address}]) {header}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn frozen_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 9, 12, 34, 56).unwrap()
    }

    #[test]
    fn includes_client_details_without_privacy_mode() {
        let header = generate(
            false,
            "test.example.com",
            "1.2.3.4",
            "1.2.3.4",
            ReceiveMethod::Smtp,
            "postal.example.com",
            frozen_time(),
        );
        assert_eq!(
            header,
            "from test.example.com (1.2.3.4 [1.2.3.4]) by postal.example.com with SMTP; Thu, 09 Jul 2026 12:34:56 +0000"
        );
    }

    #[test]
    fn omits_client_details_with_privacy_mode() {
        let header = generate(
            true,
            "test.example.com",
            "host.example.com",
            "1.2.3.4",
            ReceiveMethod::Smtp,
            "postal.example.com",
            frozen_time(),
        );
        assert_eq!(
            header,
            "by postal.example.com with SMTP; Thu, 09 Jul 2026 12:34:56 +0000"
        );
        assert!(!header.contains("1.2.3.4"));
    }

    #[test]
    fn http_method_uses_web_hostname_wording() {
        let header = generate(
            true,
            "",
            "",
            "",
            ReceiveMethod::Http,
            "web.example.com",
            frozen_time(),
        );
        assert!(header.starts_with("by web.example.com with HTTP; "));
    }
}
