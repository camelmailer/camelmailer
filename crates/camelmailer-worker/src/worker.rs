//! The message dequeuer — the port of `app/lib/message_dequeuer` plus the
//! webhook dispatch of `app/models/webhook_request.rb`.
//!
//! The worker owns the cross-tenant queue but enters each message's tenant
//! context (via the RLS-aware accessors on `camelmailer-db`) to read
//! content, so tenant isolation never depends on worker code being careful.

use crate::dkim;
use crate::inspection::{ClamavInspector, RspamdInspector};
use crate::sender::SmtpSender;
use crate::tracking;
use crate::signer::Signer;
use crate::smtp_client::SendOutcome;
use base64::Engine;
use camelmailer_core::{AdminStore, Id, RouteMode};
use camelmailer_db::{PgMessageSink, PgQueue, PgStore, PgWebhookQueue, StoredMessage};
use serde_json::json;
use std::time::Duration;

/// Webhook deliveries are retried this many times before giving up
/// (mirrors Postal's webhook retry schedule length).
const WEBHOOK_MAX_ATTEMPTS: i32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// Outgoing message delivered via SMTP.
    Delivered { response: String },
    /// Soft failure — requeued with backoff.
    Delayed { response: String },
    /// Terminal failure (hard fail or attempts exhausted).
    Failed { response: String },
    /// Recipient is on the tenant's suppression list.
    Held,
    /// Incoming message POSTed to its route endpoint.
    Routed,
    /// Nothing to deliver (incoming without an endpoint, bounces).
    NothingToDo,
    /// The queued message no longer exists.
    MessageMissing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookOutcome {
    Delivered,
    Retrying,
    GivenUp,
}

pub struct Worker {
    store: PgStore,
    sink: PgMessageSink,
    queue: PgQueue,
    webhook_queue: PgWebhookQueue,
    sender: SmtpSender,
    signer: Option<Signer>,
    dkim_selector: String,
    rspamd: Option<RspamdInspector>,
    clamav: Option<ClamavInspector>,
    spam_threshold: f64,
    spam_failure_threshold: f64,
    /// Base URL for tracking links, e.g. `https://track.example.com`.
    tracking_base_url: String,
    http: reqwest::Client,
    max_attempts: i32,
    worker_id: String,
}

impl Worker {
    pub fn new(config: &camelmailer_config::Config, store: PgStore) -> Self {
        let queue = PgQueue::new(store.pool().clone());
        let sink = PgMessageSink::new(store.clone());
        let sender = SmtpSender::new(config);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        let signer = Signer::from_pem_file(&config.camelmailer.signing_key_path)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "could not load signing key; webhook signing disabled");
                None
            });
        let webhook_queue = PgWebhookQueue::new(store.pool().clone());
        let rspamd = config
            .rspamd
            .enabled
            .then(|| RspamdInspector::new(&config.rspamd));
        let clamav = config
            .clamav
            .enabled
            .then(|| ClamavInspector::new(&config.clamav));
        Self {
            store,
            sink,
            queue,
            webhook_queue,
            signer,
            dkim_selector: config.dns.dkim_identifier.clone(),
            rspamd,
            clamav,
            spam_threshold: config.camelmailer.default_spam_threshold as f64,
            spam_failure_threshold: config.camelmailer.default_spam_failure_threshold as f64,
            tracking_base_url: format!(
                "{}://{}",
                config.camelmailer.web_protocol, config.dns.track_domain
            ),
            sender,
            http,
            max_attempts: config.camelmailer.default_maximum_delivery_attempts as i32,
            worker_id: format!("worker-{}", camelmailer_core::token::generate_token(6)),
        }
    }

    /// Process one queued message, if any is ready. Returns `None` when the
    /// queue is empty.
    pub async fn process_next(&self) -> Result<Option<ProcessOutcome>, sqlx::Error> {
        let Some(queued) = self.queue.dequeue(&self.worker_id).await? else {
            return Ok(None);
        };

        let message = self
            .sink
            .message_by_id(queued.server_id, queued.message_id)
            .await?;
        let Some(message) = message else {
            self.queue.complete(queued.id).await?;
            return Ok(Some(ProcessOutcome::MessageMissing));
        };

        let outcome = if message.scope == "outgoing" {
            self.process_outgoing(&queued, &message).await?
        } else {
            self.process_incoming(&queued, &message).await?
        };
        Ok(Some(outcome))
    }

    async fn process_outgoing(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        // Suppression list check (tenant-scoped, RLS-protected)
        if self
            .sink
            .address_suppressed(message.server_id, &message.rcpt_to)
            .await?
        {
            self.queue.complete(queued.id).await?;
            self.sink
                .record_delivery(
                    message.server_id,
                    message.id,
                    "Held",
                    "recipient is on the suppression list",
                    "",
                    false,
                )
                .await?;
            self.send_webhooks(
                message.server_id,
                "MessageHeld",
                self.message_payload(message, "recipient is on the suppression list"),
            )
            .await;
            return Ok(ProcessOutcome::Held);
        }

        // Rewrite HTML links for click tracking and append an open pixel
        // before signing, so the DKIM signature covers the final body.
        let tracked = self.apply_tracking(message).await?;

        // DKIM-sign at delivery time when the message carries an
        // authenticated domain and a signing key is configured. The stored
        // message stays unsigned, matching the Ruby behaviour.
        let raw_message = match (&self.signer, message.domain_id) {
            (Some(signer), Some(domain_id)) => {
                match self.store.domain_by_id(domain_id).await {
                    Ok(Some(domain)) => dkim::sign_and_prepend(
                        &tracked,
                        &domain.name,
                        &self.dkim_selector,
                        signer,
                        chrono::Utc::now().timestamp(),
                    ),
                    _ => tracked,
                }
            }
            _ => tracked,
        };

        let outcome = self
            .sender
            .send(
                &queued.domain,
                &message.mail_from,
                &message.rcpt_to,
                &raw_message,
            )
            .await;

        match outcome {
            SendOutcome::Sent { response } => {
                self.queue.complete(queued.id).await?;
                self.sink
                    .record_delivery(
                        message.server_id,
                        message.id,
                        "Sent",
                        "message accepted by the remote server",
                        &response,
                        false,
                    )
                    .await?;
                self.send_webhooks(
                    message.server_id,
                    "MessageSent",
                    self.message_payload(message, &response),
                )
                .await;
                Ok(ProcessOutcome::Delivered { response })
            }
            SendOutcome::SoftFail { response } => {
                if queued.attempts + 1 >= self.max_attempts {
                    self.queue.complete(queued.id).await?;
                    self.sink
                        .record_delivery(
                            message.server_id,
                            message.id,
                            "HardFail",
                            "delivery attempts exhausted",
                            &response,
                            false,
                        )
                        .await?;
                    self.send_webhooks(
                        message.server_id,
                        "MessageDeliveryFailed",
                        self.message_payload(message, &response),
                    )
                    .await;
                    Ok(ProcessOutcome::Failed { response })
                } else {
                    self.queue.retry(queued.id, queued.attempts).await?;
                    self.sink
                        .record_delivery(
                            message.server_id,
                            message.id,
                            "SoftFail",
                            "temporary delivery failure",
                            &response,
                            false,
                        )
                        .await?;
                    self.send_webhooks(
                        message.server_id,
                        "MessageDelayed",
                        self.message_payload(message, &response),
                    )
                    .await;
                    Ok(ProcessOutcome::Delayed { response })
                }
            }
            SendOutcome::HardFail { response } => {
                self.queue.complete(queued.id).await?;
                self.sink
                    .record_delivery(
                        message.server_id,
                        message.id,
                        "HardFail",
                        "message rejected by the remote server",
                        &response,
                        false,
                    )
                    .await?;
                self.send_webhooks(
                    message.server_id,
                    "MessageDeliveryFailed",
                    self.message_payload(message, &response),
                )
                .await;
                Ok(ProcessOutcome::Failed { response })
            }
        }
    }

    /// Inspect an incoming message with rspamd/clamav (when enabled) and
    /// record the verdict. Returns true when the message is a virus threat
    /// or exceeds the spam-failure threshold and should be held.
    async fn inspect(&self, message: &StoredMessage) -> Result<bool, sqlx::Error> {
        if self.rspamd.is_none() && self.clamav.is_none() {
            return Ok(false);
        }
        if message.inspected {
            return Ok(message.threat || message.spam_status == "SpamFailure");
        }

        let mut spam_status = "NotChecked".to_string();
        let mut spam_score = 0.0;
        if let Some(rspamd) = &self.rspamd {
            match rspamd.check(&message.raw_message, self.spam_threshold).await {
                Ok(result) => {
                    spam_score = result.score;
                    spam_status = if result.score >= self.spam_failure_threshold {
                        "SpamFailure".to_string()
                    } else if result.score >= self.spam_threshold {
                        "Spam".to_string()
                    } else {
                        "NotSpam".to_string()
                    };
                }
                Err(error) => tracing::warn!(%error, "rspamd inspection failed"),
            }
        }

        let mut threat = false;
        let mut threat_details = None;
        if let Some(clamav) = &self.clamav {
            match clamav.scan(&message.raw_message).await {
                Ok(result) => {
                    threat = result.found;
                    threat_details = result.details;
                }
                Err(error) => tracing::warn!(%error, "clamav inspection failed"),
            }
        }

        self.sink
            .record_inspection(
                message.server_id,
                message.id,
                &spam_status,
                spam_score,
                threat,
                threat_details.as_deref(),
            )
            .await?;

        Ok(threat || spam_status == "SpamFailure")
    }

    async fn process_incoming(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        // Inspect incoming mail before routing; a virus or spam-failure
        // message is held (stored, not delivered).
        if self.inspect(message).await? {
            self.sink
                .record_delivery(
                    message.server_id,
                    message.id,
                    "Held",
                    "message failed inspection (spam or virus)",
                    "",
                    false,
                )
                .await?;
            self.queue.complete(queued.id).await?;
            return Ok(ProcessOutcome::Held);
        }

        let route = match message.route_id {
            Some(route_id) => self
                .store
                .route_by_id(message.server_id, route_id)
                .await
                .unwrap_or(None),
            None => None,
        };

        let endpoint_url = route.as_ref().and_then(|r| {
            (r.mode == RouteMode::Endpoint).then(|| r.endpoint_url.clone()).flatten()
        });

        let Some(endpoint_url) = endpoint_url else {
            // Accept/Hold routes and bounces: the message is stored; there is
            // nothing to deliver.
            self.queue.complete(queued.id).await?;
            return Ok(ProcessOutcome::NothingToDo);
        };

        let payload = json!({
            "message": {
                "id": message.id,
                "token": message.token,
                "rcpt_to": message.rcpt_to,
                "mail_from": message.mail_from,
                "bounce": message.bounce,
            },
            "raw_base64": base64::engine::general_purpose::STANDARD.encode(&message.raw_message),
        });

        let result = self.http.post(&endpoint_url).json(&payload).send().await;
        let success = matches!(&result, Ok(response) if response.status().is_success());
        if success {
            self.queue.complete(queued.id).await?;
            Ok(ProcessOutcome::Routed)
        } else {
            let response = match result {
                Ok(response) => format!("endpoint returned {}", response.status()),
                Err(error) => format!("endpoint request failed: {error}"),
            };
            if queued.attempts + 1 >= self.max_attempts {
                self.queue.complete(queued.id).await?;
                Ok(ProcessOutcome::Failed { response })
            } else {
                self.queue.retry(queued.id, queued.attempts).await?;
                Ok(ProcessOutcome::Delayed { response })
            }
        }
    }

    /// Register tracking tokens and rewrite the HTML body of an outgoing
    /// message. No-op for non-HTML messages. Returns the (possibly
    /// rewritten) raw message.
    async fn apply_tracking(&self, message: &StoredMessage) -> Result<Vec<u8>, sqlx::Error> {
        let Some((headers, body)) = tracking::html_body(&message.raw_message) else {
            return Ok(message.raw_message.clone());
        };

        // register a link + click token for every rewritten URL
        let mut pending_links: Vec<String> = Vec::new();
        let (rewritten, urls) = tracking::rewrite_links(&body, |url| {
            pending_links.push(url.to_string());
            format!("__CM_CLICK_{}__", pending_links.len() - 1)
        });
        let _ = urls;

        let mut click_tokens = Vec::with_capacity(pending_links.len());
        for url in &pending_links {
            let (link_id, _) = self
                .sink
                .create_link(message.server_id, message.id, url)
                .await?;
            let token = self
                .store
                .create_click_token(message.server_id, message.id, link_id, url)
                .await?;
            click_tokens.push(token);
        }

        let mut rewritten = rewritten;
        for (index, token) in click_tokens.iter().enumerate() {
            rewritten = rewritten.replace(
                &format!("__CM_CLICK_{index}__"),
                &format!("{}/track/c/{token}", self.tracking_base_url),
            );
        }

        // open-tracking pixel
        let open_token = self
            .store
            .create_open_token(message.server_id, message.id)
            .await?;
        let pixel_url = format!("{}/track/o/{open_token}.gif", self.tracking_base_url);
        let rewritten = tracking::inject_open_pixel(&rewritten, &pixel_url);

        Ok(tracking::reassemble(&headers, &rewritten))
    }

    fn message_payload(&self, message: &StoredMessage, details: &str) -> serde_json::Value {
        json!({
            "message": {
                "id": message.id,
                "token": message.token,
                "rcpt_to": message.rcpt_to,
                "mail_from": message.mail_from,
                "scope": message.scope,
                "bounce": message.bounce,
            },
            "details": details,
        })
    }

    /// Enqueue an event for every enabled webhook of the server. Delivery,
    /// signing, retrying and audit logging happen in
    /// [`Worker::process_next_webhook`].
    async fn send_webhooks(&self, server_id: Id, event: &str, payload: serde_json::Value) {
        let webhooks = match self.store.list_webhooks(server_id).await {
            Ok(webhooks) => webhooks,
            Err(error) => {
                tracing::warn!(%error, server_id, "could not load webhooks");
                return;
            }
        };
        for webhook in webhooks.into_iter().filter(|w| w.enabled) {
            let uuid = camelmailer_core::token::generate_uuid();
            let body = json!({
                "event": event,
                "timestamp": chrono::Utc::now().timestamp(),
                "uuid": uuid,
                "payload": payload,
            });
            if let Err(error) = self
                .webhook_queue
                .enqueue(
                    server_id,
                    webhook.id,
                    &uuid,
                    event,
                    &webhook.url,
                    &body.to_string(),
                    webhook.sign,
                )
                .await
            {
                tracing::warn!(%error, webhook = %webhook.url, "could not enqueue webhook");
            }
        }
    }

    /// Deliver one queued webhook request, if any is ready. Signs the body
    /// with the installation signing key when the webhook asks for it,
    /// records every attempt in the tenant-scoped audit log, and retries
    /// failures with backoff.
    pub async fn process_next_webhook(&self) -> Result<Option<WebhookOutcome>, sqlx::Error> {
        let Some(request) = self.webhook_queue.dequeue(&self.worker_id).await? else {
            return Ok(None);
        };

        let mut http_request = self
            .http
            .post(&request.url)
            .header("content-type", "application/json")
            .header("X-CamelMailer-Event", &request.event)
            .header("X-CamelMailer-UUID", &request.uuid);
        if request.sign {
            if let Some(signer) = &self.signer {
                let signature = signer.sign_sha256(request.payload.as_bytes());
                http_request = http_request.header(
                    "X-CamelMailer-Signature",
                    base64::engine::general_purpose::STANDARD.encode(signature),
                );
            }
        }

        let attempt = request.attempts + 1;
        let result = http_request.body(request.payload.clone()).send().await;
        let (status_code, success, response_body) = match result {
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                (Some(status.as_u16() as i32), status.is_success(), body)
            }
            Err(error) => (None, false, error.to_string()),
        };
        self.webhook_queue
            .log_attempt(&request, attempt, status_code, success, &response_body)
            .await?;

        if success {
            self.webhook_queue.complete(request.id).await?;
            Ok(Some(WebhookOutcome::Delivered))
        } else if attempt >= WEBHOOK_MAX_ATTEMPTS {
            self.webhook_queue.complete(request.id).await?;
            tracing::warn!(url = %request.url, "webhook given up after {WEBHOOK_MAX_ATTEMPTS} attempts");
            Ok(Some(WebhookOutcome::GivenUp))
        } else {
            self.webhook_queue.retry(request.id, request.attempts).await?;
            Ok(Some(WebhookOutcome::Retrying))
        }
    }

    /// Test/ops helper: deliver every ready webhook request.
    pub async fn drain_webhooks(&self) -> Result<usize, sqlx::Error> {
        let mut processed = 0;
        while self.process_next_webhook().await?.is_some() {
            processed += 1;
        }
        Ok(processed)
    }

    /// The long-running worker loop: drain the queue, then poll.
    pub async fn run(&self) -> Result<(), sqlx::Error> {
        tracing::info!(worker_id = %self.worker_id, "camelmailer worker started");
        loop {
            let mut idle = true;
            match self.process_next().await {
                Ok(Some(outcome)) => {
                    idle = false;
                    tracing::debug!(?outcome, "processed queued message");
                }
                Ok(None) => {}
                Err(error) => tracing::error!(%error, "queue processing error"),
            }
            match self.process_next_webhook().await {
                Ok(Some(outcome)) => {
                    idle = false;
                    tracing::debug!(?outcome, "processed webhook request");
                }
                Ok(None) => {}
                Err(error) => tracing::error!(%error, "webhook processing error"),
            }
            if idle {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
