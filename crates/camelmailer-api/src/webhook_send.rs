//! Synchronous webhook test delivery (`POST …/webhooks/{id}/test`).
//!
//! The real event pipeline lives in the worker (queue, retries, audit log);
//! a test send is a one-shot HTTP POST from the API process that mirrors
//! the worker's request shape exactly: custom webhook headers first, then
//! the platform `X-CamelMailer-*` headers (so the platform always wins),
//! and the RSA payload signature when the webhook asks for it.
//!
//! The HTTP call itself sits behind the [`WebhookSender`] trait so router
//! tests exercise the full path against a local axum mock (and can inject
//! a short timeout for the timeout case).

use async_trait::async_trait;
use base64::Engine;
use camelmailer_core::Webhook;
use serde_json::{json, Value};
use std::time::Duration;

/// Default timeout for a test delivery.
pub const TEST_SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// One prepared outgoing webhook request (headers already ordered:
/// custom first, platform last).
pub struct WebhookRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// What came back from the endpoint (any HTTP response counts as a
/// response — success is judged by the caller via `status`).
pub struct WebhookResponse {
    pub status: u16,
}

/// The HTTP leg of a webhook delivery.
#[async_trait]
pub trait WebhookSender: Send + Sync {
    /// POST the request; `Err` is a transport failure (connect error,
    /// timeout, …) — an HTTP error status is an `Ok` response.
    async fn send(&self, request: WebhookRequest) -> Result<WebhookResponse, String>;
}

/// The production sender: reqwest with a hard timeout.
pub struct ReqwestWebhookSender {
    client: reqwest::Client,
}

impl ReqwestWebhookSender {
    pub fn new() -> Self {
        Self::with_timeout(TEST_SEND_TIMEOUT)
    }

    /// A sender with a custom timeout (tests use a short one).
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .expect("reqwest client construction cannot fail"),
        }
    }
}

impl Default for ReqwestWebhookSender {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebhookSender for ReqwestWebhookSender {
    async fn send(&self, request: WebhookRequest) -> Result<WebhookResponse, String> {
        let mut http_request = self
            .client
            .post(&request.url)
            .header("content-type", "application/json");
        for (name, value) in &request.headers {
            use reqwest::header::{HeaderName, HeaderValue};
            match (
                HeaderName::try_from(name.as_str()),
                HeaderValue::try_from(value.as_str()),
            ) {
                (Ok(name), Ok(value)) => {
                    http_request = http_request.header(name, value);
                }
                // values are secrets — never logged
                _ => tracing::warn!(header = %name, "skipping invalid webhook header"),
            }
        }
        let response = http_request
            .body(request.body)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        Ok(WebhookResponse {
            status: response.status().as_u16(),
        })
    }
}

/// The event-specific `details` string of a sample payload.
fn sample_details(event: &str) -> &'static str {
    match event {
        "MessageSent" => "Message for recipient@example.com accepted by mx.example.com",
        "MessageDelayed" => "421 4.7.0 try again later (attempt 2 of 18)",
        "MessageDeliveryFailed" => "550 5.1.1 recipient address rejected: user unknown",
        "MessageHeld" => "Message held for manual review (spam score above threshold)",
        _ => "Test delivery",
    }
}

/// A realistic sample body for `event`, marked as a test (`"test": true`)
/// and shaped exactly like the worker's real deliveries.
pub fn sample_payload(event: &str, uuid: &str) -> Value {
    json!({
        "event": event,
        "timestamp": chrono::Utc::now().timestamp(),
        "uuid": uuid,
        "test": true,
        "payload": {
            "message": {
                "id": 1234,
                "token": "AbCdEf123456",
                "rcpt_to": "recipient@example.com",
                "mail_from": "sender@yourdomain.com",
                "scope": "outgoing",
                "bounce": false,
            },
            "details": sample_details(event),
        },
    })
}

/// Build the full request for a test delivery of `event` to `webhook`:
/// sample body, custom headers, platform headers, and — when the webhook
/// signs and an installation signing key exists — the RSA-SHA256 payload
/// signature the worker would attach.
pub fn build_test_request(
    webhook: &Webhook,
    event: &str,
    signing_key_pem: Option<&str>,
) -> WebhookRequest {
    let uuid = camelmailer_core::token::generate_uuid();
    let body = sample_payload(event, &uuid).to_string();

    // custom webhook headers first, then the platform headers, so the
    // latter always win (mirrors the worker)
    let mut headers: Vec<(String, String)> = webhook
        .headers
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    headers.push(("X-CamelMailer-Event".into(), event.to_string()));
    headers.push(("X-CamelMailer-UUID".into(), uuid));
    if webhook.sign {
        if let Some(signature) = signing_key_pem.and_then(|pem| sign_payload(pem, body.as_bytes()))
        {
            headers.push(("X-CamelMailer-Signature".into(), signature));
        }
    }

    WebhookRequest {
        url: webhook.url.clone(),
        headers,
        body,
    }
}

/// base64(RSA-SHA256 PKCS#1 v1.5 signature) of `data` with the
/// installation signing key — the same signature the worker attaches.
fn sign_payload(pem: &str, data: &[u8]) -> Option<String> {
    use rsa::pkcs1::DecodeRsaPrivateKey;
    use rsa::pkcs8::DecodePrivateKey;
    use sha2::{Digest, Sha256};
    let key = rsa::RsaPrivateKey::from_pkcs8_pem(pem)
        .or_else(|_| rsa::RsaPrivateKey::from_pkcs1_pem(pem))
        .ok()?;
    let digest = Sha256::digest(data);
    let signature = key.sign(rsa::Pkcs1v15Sign::new::<Sha256>(), &digest).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(signature))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn webhook(sign: bool) -> Webhook {
        Webhook {
            id: 1,
            uuid: "u".into(),
            server_id: 1,
            name: "hook".into(),
            url: "https://example.com/hook".into(),
            all_events: true,
            enabled: true,
            sign,
            events: vec![],
            headers: std::collections::BTreeMap::from([(
                "Authorization".to_string(),
                "Bearer secret".to_string(),
            )]),
        }
    }

    #[test]
    fn sample_payload_is_marked_as_test_and_carries_the_event() {
        let payload = sample_payload("MessageSent", "uuid-1");
        assert_eq!(payload["test"], true);
        assert_eq!(payload["event"], "MessageSent");
        assert_eq!(payload["uuid"], "uuid-1");
        assert_eq!(
            payload["payload"]["message"]["rcpt_to"],
            "recipient@example.com"
        );
        assert!(payload["payload"]["details"]
            .as_str()
            .unwrap()
            .contains("accepted"));
    }

    #[test]
    fn custom_headers_come_before_platform_headers() {
        let request = build_test_request(&webhook(false), "MessageHeld", None);
        let names: Vec<&str> = request.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["Authorization", "X-CamelMailer-Event", "X-CamelMailer-UUID"]
        );
        assert_eq!(request.headers[1].1, "MessageHeld");
    }

    #[test]
    fn signature_is_attached_only_with_a_key_and_sign_enabled() {
        let pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap(),
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap()
        .to_string();

        let signed = build_test_request(&webhook(true), "MessageSent", Some(&pem));
        assert!(signed
            .headers
            .iter()
            .any(|(name, _)| name == "X-CamelMailer-Signature"));

        let unsigned = build_test_request(&webhook(true), "MessageSent", None);
        assert!(!unsigned
            .headers
            .iter()
            .any(|(name, _)| name == "X-CamelMailer-Signature"));

        let sign_off = build_test_request(&webhook(false), "MessageSent", Some(&pem));
        assert!(!sign_off
            .headers
            .iter()
            .any(|(name, _)| name == "X-CamelMailer-Signature"));
    }
}
