//! Per-message deliverability insights
//! (`GET /api/v2/server/messages/{id}/insights`).
//!
//! A rule catalog evaluated purely from stored data (the raw MIME, the
//! server's domain configuration) plus one live DNS lookup (the DMARC
//! record of the From domain). Every check reports `ok` or `warning`;
//! a check that cannot be evaluated (DNS lookup failure) is skipped
//! instead of failing the request — no check may ever kill the
//! endpoint.

use crate::app::ApiState;
use camelmailer_core::{MessageRecord, Server};
use serde_json::{json, Value};

/// One evaluated rule.
pub(crate) struct Check {
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    /// "ok" | "warning"
    pub(crate) status: &'static str,
    pub(crate) detail: String,
}

impl Check {
    pub(crate) fn json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "status": self.status,
            "detail": self.detail,
        })
    }
}

/// Well-known URL-shortener hosts — mail with links through these is a
/// common spam heuristic, so they are called out explicitly.
const URL_SHORTENERS: [&str; 8] = [
    "bit.ly",
    "tinyurl.com",
    "goo.gl",
    "t.co",
    "ow.ly",
    "is.gd",
    "buff.ly",
    "cutt.ly",
];

/// The longest subject length that survives without folding (RFC 5322
/// recommends lines of at most 78 characters).
const MAX_SUBJECT_LENGTH: usize = 78;

/// Bodies above this size get clipped by Gmail and friends.
const MAX_BODY_BYTES: i64 = 100 * 1024;

/// `host` equals `domain` or is a subdomain of it (case-insensitive).
fn is_same_or_subdomain(host: &str, domain: &str) -> bool {
    let host = host.to_lowercase();
    let domain = domain.to_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// The host part of an absolute http(s) URL, lowercased.
fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .split('@')
        .next_back()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

/// All values of `attribute="…"` / `attribute='…'` in `html`, in order.
fn attribute_values(html: &str, attribute: &str) -> Vec<String> {
    let lower = html.to_lowercase();
    let needle = format!("{attribute}=");
    let mut values = Vec::new();
    let mut offset = 0;
    while let Some(position) = lower[offset..].find(&needle) {
        let value_start = offset + position + needle.len();
        let Some(quote) = html[value_start..].chars().next() else {
            break;
        };
        offset = value_start + 1;
        if quote != '"' && quote != '\'' {
            continue;
        }
        if let Some(end) = html[value_start + 1..].find(quote) {
            values.push(html[value_start + 1..value_start + 1 + end].to_string());
            offset = value_start + 1 + end + 1;
        } else {
            break;
        }
    }
    values
}

/// Hosts of all absolute links (`href`) in the HTML body.
fn link_hosts(html: &str) -> Vec<String> {
    attribute_values(html, "href")
        .iter()
        .filter_map(|url| host_of(url))
        .collect()
}

/// Hosts of all absolute image sources (`src`) in the HTML body.
fn image_hosts(html: &str) -> Vec<String> {
    attribute_values(html, "src")
        .iter()
        .filter_map(|url| host_of(url))
        .collect()
}

fn dedup(mut hosts: Vec<String>) -> Vec<String> {
    hosts.sort();
    hosts.dedup();
    hosts
}

/// Evaluate the full rule catalog for one stored message.
pub(crate) async fn evaluate(
    state: &ApiState,
    server: &Server,
    message: &MessageRecord,
) -> Vec<Check> {
    let bodies = camelmailer_core::mime::extract_bodies(&message.raw_message);
    let from_domain = message
        .mail_from
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_lowercase());

    let mut checks = Vec::new();

    // 1. plain-text version
    checks.push(if bodies.text.is_some() {
        Check {
            id: "plain_text",
            title: "Plain-text version",
            status: "ok",
            detail: "The message includes a plain-text alternative.".into(),
        }
    } else {
        Check {
            id: "plain_text",
            title: "Plain-text version",
            status: "warning",
            detail: "The message has no plain-text part — HTML-only mail scores worse \
                     with spam filters and some clients."
                .into(),
        }
    });

    // 2. subject present and short enough
    let subject = message.subject.as_deref().unwrap_or("").trim().to_string();
    checks.push(if subject.is_empty() {
        Check {
            id: "subject",
            title: "Subject line",
            status: "warning",
            detail: "The message has no subject.".into(),
        }
    } else if subject.chars().count() > MAX_SUBJECT_LENGTH {
        Check {
            id: "subject",
            title: "Subject line",
            status: "warning",
            detail: format!(
                "The subject is {} characters long — keep it at {MAX_SUBJECT_LENGTH} or fewer.",
                subject.chars().count()
            ),
        }
    } else {
        Check {
            id: "subject",
            title: "Subject line",
            status: "ok",
            detail: format!("Subject present ({} characters).", subject.chars().count()),
        }
    });

    // 3. From is not a no-reply address
    let local_part = message
        .mail_from
        .split('@')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .replace(['-', '_', '.'], "");
    checks.push(
        if local_part.contains("noreply") || local_part.contains("donotreply") {
            Check {
                id: "from_address",
                title: "From address",
                status: "warning",
                detail: format!(
                    "{} looks like a no-reply address — mail people can answer builds \
                 trust and engagement.",
                    message.mail_from
                ),
            }
        } else {
            Check {
                id: "from_address",
                title: "From address",
                status: "ok",
                detail: "The From address accepts replies.".into(),
            }
        },
    );

    // 4. links point at the From domain (shorteners called out)
    let html = bodies.html.as_deref().unwrap_or("");
    let hosts = dedup(link_hosts(html));
    let foreign: Vec<String> = match &from_domain {
        Some(domain) => hosts
            .iter()
            .filter(|host| !is_same_or_subdomain(host, domain))
            .cloned()
            .collect(),
        None => hosts.clone(),
    };
    let shorteners: Vec<String> = foreign
        .iter()
        .filter(|host| URL_SHORTENERS.contains(&host.as_str()))
        .cloned()
        .collect();
    checks.push(if hosts.is_empty() {
        Check {
            id: "links",
            title: "Link domains",
            status: "ok",
            detail: "No external links found.".into(),
        }
    } else if !shorteners.is_empty() {
        Check {
            id: "links",
            title: "Link domains",
            status: "warning",
            detail: format!(
                "The message links through URL shorteners ({}) — spam filters \
                 penalize shortened links; link to your own domain instead.",
                shorteners.join(", ")
            ),
        }
    } else if !foreign.is_empty() {
        Check {
            id: "links",
            title: "Link domains",
            status: "warning",
            detail: format!(
                "Some links point away from the sending domain ({}) — links on \
                 your own domain align better with your sender reputation.",
                foreign.join(", ")
            ),
        }
    } else {
        Check {
            id: "links",
            title: "Link domains",
            status: "ok",
            detail: "All links point at the sending domain or its subdomains.".into(),
        }
    });

    // 5. images hosted on the sending domain
    let image_hosts = dedup(image_hosts(html));
    let foreign_images: Vec<String> = match &from_domain {
        Some(domain) => image_hosts
            .iter()
            .filter(|host| !is_same_or_subdomain(host, domain))
            .cloned()
            .collect(),
        None => image_hosts.clone(),
    };
    checks.push(if image_hosts.is_empty() {
        Check {
            id: "images",
            title: "Image hosting",
            status: "ok",
            detail: "No remote images found.".into(),
        }
    } else if foreign_images.is_empty() {
        Check {
            id: "images",
            title: "Image hosting",
            status: "ok",
            detail: "All images are hosted on the sending domain.".into(),
        }
    } else {
        Check {
            id: "images",
            title: "Image hosting",
            status: "warning",
            detail: format!(
                "Images are loaded from third-party hosts ({}) — host them on \
                 your own domain for consistency and deliverability.",
                foreign_images.join(", ")
            ),
        }
    });

    // 6. body size
    checks.push(if message.size < MAX_BODY_BYTES {
        Check {
            id: "size",
            title: "Message size",
            status: "ok",
            detail: format!(
                "{} bytes — well under the 100 KB clipping limit.",
                message.size
            ),
        }
    } else {
        Check {
            id: "size",
            title: "Message size",
            status: "warning",
            detail: format!(
                "{} bytes — Gmail clips messages above roughly 100 KB.",
                message.size
            ),
        }
    });

    // 7. From domain is a verified sending domain
    let verified_domain = match &from_domain {
        Some(domain) => state
            .store
            .authenticated_domain(server.id, domain)
            .await
            .unwrap_or(None),
        None => None,
    };
    checks.push(if verified_domain.is_some() {
        Check {
            id: "sending_domain",
            title: "Verified sending domain",
            status: "ok",
            detail: format!(
                "{} is a verified sending domain of this server.",
                from_domain.clone().unwrap_or_default()
            ),
        }
    } else {
        Check {
            id: "sending_domain",
            title: "Verified sending domain",
            status: "warning",
            detail: match &from_domain {
                Some(domain) => format!(
                    "{domain} is not a verified sending domain of this server — \
                     verify it so SPF and DKIM align."
                ),
                None => "The From address has no domain.".into(),
            },
        }
    });

    // 8. DMARC record of the From domain (live DNS; a lookup failure
    // skips the check instead of failing the request)
    match &from_domain {
        None => checks.push(Check {
            id: "dmarc",
            title: "DMARC record",
            status: "warning",
            detail: "The From address has no domain to check.".into(),
        }),
        Some(domain) => {
            let record_name = format!("_dmarc.{domain}");
            match state.dns_resolver.txt_records(&record_name).await {
                // DNS failure: nothing can be said either way — skip
                Err(_) => {}
                Ok(records) => {
                    let found = records
                        .iter()
                        .any(|record| camelmailer_core::dmarc::is_dmarc_record(record));
                    checks.push(if found {
                        Check {
                            id: "dmarc",
                            title: "DMARC record",
                            status: "ok",
                            detail: format!("A DMARC policy is published at {record_name}."),
                        }
                    } else {
                        Check {
                            id: "dmarc",
                            title: "DMARC record",
                            status: "warning",
                            detail: format!(
                                "No v=DMARC1 TXT record found at {record_name} — publish \
                                 one (start with p=none) to protect the domain."
                            ),
                        }
                    });
                }
            }
        }
    }

    // 9. DKIM active: the domain's own key or the installation key
    let domain_key = match &from_domain {
        Some(domain) => state
            .store
            .list_domains(server.id)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|d| d.name.eq_ignore_ascii_case(domain))
            .map(|d| d.dkim_private_key.is_some()),
        None => None,
    };
    let dkim_active = domain_key.unwrap_or(false) || state.installation_dkim_public_key.is_some();
    checks.push(if dkim_active {
        Check {
            id: "dkim",
            title: "DKIM signing",
            status: "ok",
            detail: if domain_key.unwrap_or(false) {
                "Outbound mail is DKIM-signed with the domain's own key.".into()
            } else {
                "Outbound mail is DKIM-signed with the installation key.".into()
            },
        }
    } else {
        Check {
            id: "dkim",
            title: "DKIM signing",
            status: "warning",
            detail: "Neither the domain nor the installation has a DKIM key — \
                     unsigned mail fails DMARC alignment."
                .into(),
        }
    });

    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_extraction_handles_ports_paths_and_userinfo() {
        assert_eq!(
            host_of("https://example.com/x?y#z"),
            Some("example.com".into())
        );
        assert_eq!(
            host_of("http://Example.COM:8080/x"),
            Some("example.com".into())
        );
        assert_eq!(
            host_of("https://user@sub.example.com/"),
            Some("sub.example.com".into())
        );
        assert_eq!(host_of("mailto:x@example.com"), None);
        assert_eq!(host_of("/relative"), None);
    }

    #[test]
    fn subdomains_match_but_lookalikes_do_not() {
        assert!(is_same_or_subdomain("example.com", "example.com"));
        assert!(is_same_or_subdomain("mail.example.com", "example.com"));
        assert!(!is_same_or_subdomain("evilexample.com", "example.com"));
        assert!(!is_same_or_subdomain("example.com.evil.net", "example.com"));
    }

    #[test]
    fn html_attribute_scanning_finds_links_and_images() {
        let html = r#"<a href="https://a.example/x">a</a>
                      <a HREF='http://b.example'>b</a>
                      <img src="https://cdn.example/i.png">
                      <a href="/relative">rel</a>"#;
        assert_eq!(link_hosts(html), vec!["a.example", "b.example"]);
        assert_eq!(image_hosts(html), vec!["cdn.example"]);
    }
}
