//! The message dequeuer — the port of `app/lib/message_dequeuer` plus the
//! webhook dispatch of `app/models/webhook_request.rb`.
//!
//! The worker owns the cross-tenant queue but enters each message's tenant
//! context (via the RLS-aware accessors on `camelmailer-db`) to read
//! content, so tenant isolation never depends on worker code being careful.

use crate::dkim;
use crate::inspection::{ClamavInspector, RspamdInspector};
use crate::sender::SmtpSender;
use crate::signer::Signer;
use crate::smtp_client::SendOutcome;
use crate::ssrf::SsrfGuard;
use crate::tracking;
use base64::Engine;
use camelmailer_core::{AdminStore, RouteMode};
use camelmailer_db::webhook_payload::{
    postal_webhook_message_id, postal_webhook_subject, postal_webhook_timestamp,
};
use camelmailer_db::{
    BounceCorrelationOutcome, BounceOriginal, FeedbackComplaintOutcome, OutboundQueueAction,
    PgLifecycleQueue, PgMessageSink, PgQueue, PgStore, PgWebhookQueue, QueueFilter, StoredMessage,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Webhook deliveries are retried this many times before giving up
/// (mirrors Postal's webhook retry schedule length).
const WEBHOOK_MAX_ATTEMPTS: i32 = 10;

/// How often the worker loop runs housekeeping.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(3600);

const UNMATCHED_BOUNCE_DETAILS: &str = "This message was a bounce but we couldn't link it with any outgoing message and there was no route for it.";

/// Borrowed view of either side of Postal's `MessageBounced` payload. Keeping
/// the JSON contract here prevents the incoming and original shapes drifting.
struct BounceWebhookFields<'a> {
    id: i64,
    token: &'a str,
    direction: &'a str,
    message_id: Option<String>,
    to: &'a str,
    from_address: &'a str,
    subject: String,
    timestamp: f64,
    spam_status: &'a str,
    tag: Option<&'a str>,
}

impl<'a> From<&'a StoredMessage> for BounceWebhookFields<'a> {
    fn from(message: &'a StoredMessage) -> Self {
        Self {
            id: message.id,
            token: &message.token,
            direction: &message.scope,
            message_id: postal_webhook_message_id(message.message_id_header.as_deref()),
            to: &message.rcpt_to,
            from_address: &message.mail_from,
            subject: postal_webhook_subject(message.subject.as_deref()),
            timestamp: postal_webhook_timestamp(message.created_at),
            spam_status: &message.spam_status,
            tag: message.tag.as_deref(),
        }
    }
}

impl<'a> From<&'a BounceOriginal> for BounceWebhookFields<'a> {
    fn from(message: &'a BounceOriginal) -> Self {
        Self {
            id: message.id,
            token: &message.token,
            direction: &message.scope,
            message_id: postal_webhook_message_id(message.message_id_header.as_deref()),
            to: &message.rcpt_to,
            from_address: &message.mail_from,
            subject: postal_webhook_subject(message.subject.as_deref()),
            timestamp: postal_webhook_timestamp(message.created_at),
            spam_status: &message.spam_status,
            tag: message.tag.as_deref(),
        }
    }
}

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
    /// Incoming message parsed and stored as a DMARC aggregate report
    /// (route target `internal://dmarc-reports`).
    DmarcReportIngested,
    /// Incoming message recognised as an ARF spam-complaint (feedback loop)
    /// report and turned into a stream-scoped complaint for the recipient.
    FeedbackReportIngested,
    /// An inbound DSN was linked to its original outgoing message.
    BounceCorrelated,
    /// A terminal DSN already bounced this outgoing message. Its stale queue
    /// row was removed without another send or delivery event.
    AlreadyBounced,
    /// Nothing to deliver (an incoming message without an endpoint).
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
    /// The installation lifecycle-event outbox.
    lifecycle_queue: PgLifecycleQueue,
    /// `lifecycle_webhooks`: where lifecycle events go and the HMAC secret.
    lifecycle_webhooks: camelmailer_config::LifecycleWebhooks,
    sender: SmtpSender,
    signer: Option<Signer>,
    dkim_selector: String,
    rspamd: Option<RspamdInspector>,
    clamav: Option<ClamavInspector>,
    spam_threshold: f64,
    spam_failure_threshold: f64,
    /// Base URL for tracking links, e.g. `https://track.example.com`.
    tracking_base_url: String,
    /// `camelmailer.web_protocol` — scheme for per-server track domain URLs.
    web_protocol: String,
    http: reqwest::Client,
    /// Address guard for outbound webhook / route-endpoint requests (SSRF).
    ssrf_guard: SsrfGuard,
    max_attempts: i32,
    /// Installation retention defaults (`retention` group, with
    /// `camelmailer.message_retention_days` as the metadata alias).
    retention: camelmailer_core::RetentionPeriods,
    /// `retention.auth_event_days` (0 = keep forever).
    auth_event_days: u32,
    /// `retention.api_request_days` (0 = keep forever).
    api_request_days: u32,
    /// `retention.batch_size`.
    retention_batch: i64,
    /// Validated shared SMTP return-path domain. Invalid placeholders retain
    /// the submitted envelope sender.
    return_path_domain: Option<String>,
    worker_id: String,
    /// Delivery loops of this process (`worker.threads`).
    threads: u32,
}

impl Worker {
    pub fn new(config: &camelmailer_config::Config, store: PgStore) -> Self {
        let stale_lock_days = config.camelmailer.queued_message_lock_stale_days as i32;
        let queue = PgQueue::with_stale_lock_days(store.pool().clone(), stale_lock_days);
        let sink = PgMessageSink::new(store.clone());
        let sender = SmtpSender::new(config);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        let signer =
            Signer::from_pem_file(&config.camelmailer.signing_key_path).unwrap_or_else(|error| {
                tracing::warn!(%error, "could not load signing key; webhook signing disabled");
                None
            });
        let webhook_queue =
            PgWebhookQueue::with_stale_lock_days(store.pool().clone(), stale_lock_days);
        let rspamd = config
            .rspamd
            .enabled
            .then(|| RspamdInspector::new(&config.rspamd));
        let clamav = config
            .clamav
            .enabled
            .then(|| ClamavInspector::new(&config.clamav));
        let return_path_domain = config
            .dns
            .return_path_envelope
            .then(|| config.dns.normalized_return_path_domain())
            .flatten();
        if config.dns.return_path_envelope && return_path_domain.is_none() {
            tracing::warn!(
                configured = %config.dns.return_path_domain,
                "dns.return_path_domain is empty, invalid, or reserved; preserving submitted envelope senders"
            );
        }
        let lifecycle_queue = PgLifecycleQueue::new(store.pool().clone(), stale_lock_days);
        Self {
            store,
            sink,
            queue,
            webhook_queue,
            lifecycle_queue,
            lifecycle_webhooks: config.lifecycle_webhooks.clone(),
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
            web_protocol: config.camelmailer.web_protocol.clone(),
            sender,
            http,
            ssrf_guard: SsrfGuard::from_config(config),
            max_attempts: config.camelmailer.default_maximum_delivery_attempts as i32,
            retention: camelmailer_core::RetentionPeriods::from_days(
                config.retention.message_content_days,
                config.message_metadata_retention_days(),
                config.retention.webhook_log_days,
                config.retention.dmarc_report_days,
            ),
            auth_event_days: config.retention.auth_event_days,
            api_request_days: config.retention.api_request_days,
            retention_batch: config.retention.batch_size.max(1) as i64,
            return_path_domain,
            worker_id: format!("worker-{}", camelmailer_core::token::generate_token(6)),
            threads: config.worker.threads.max(1),
        }
    }

    /// Scope the delivery to part of the queue and set how many messages
    /// this process delivers at once. The filter comes from `worker.servers`,
    /// `worker.exclude_servers` and `worker.deliver`, resolved to server ids.
    pub fn with_delivery(mut self, filter: QueueFilter, threads: u32) -> Self {
        self.queue = self.queue.with_filter(filter);
        self.threads = threads.max(1);
        self
    }

    /// Process one queued message, if any is ready. Returns `None` when the
    /// queue is empty.
    pub async fn process_next(&self) -> Result<Option<ProcessOutcome>, sqlx::Error> {
        let Some(queued) = self.queue.dequeue(&self.worker_id).await? else {
            return Ok(None);
        };

        let loaded = if self.return_path_domain.is_some() {
            self.sink
                .message_with_server_token(queued.server_id, queued.message_id)
                .await?
                .map(|(message, token)| (message, Some(token)))
        } else {
            self.sink
                .message_by_id(queued.server_id, queued.message_id)
                .await?
                .map(|message| (message, None))
        };
        let Some((mut message, server_token)) = loaded else {
            self.queue.complete(queued.id).await?;
            return Ok(Some(ProcessOutcome::MessageMissing));
        };

        let outcome = if message.scope == "outgoing" {
            self.process_outgoing(&queued, &message, server_token.as_deref())
                .await?
        } else {
            self.process_incoming(&queued, &mut message).await?
        };
        Ok(Some(outcome))
    }

    async fn process_outgoing(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
        server_token: Option<&str>,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        if message.status == "Bounced" {
            self.queue.complete(queued.id).await?;
            tracing::info!(
                message_id = message.id,
                "completed stale queue row for already bounced message"
            );
            return Ok(ProcessOutcome::AlreadyBounced);
        }

        // Suppression list check (tenant-scoped, RLS-protected). Stream-aware:
        // a server-wide (stream_id NULL) or this-stream suppression blocks it,
        // so a broadcast opt-out never holds transactional mail.
        if camelmailer_core::ServerStore::address_suppressed(
            &self.store,
            message.server_id,
            &message.rcpt_to,
            message.stream_id,
        )
        .await
        .map_err(|e| sqlx::Error::Protocol(e.to_string()))?
        {
            let details = "recipient is on the suppression list";
            let payload = self.message_payload(message, details);
            let recorded = self
                .sink
                .record_outbound_delivery(
                    message.server_id,
                    message.id,
                    queued.id,
                    "Held",
                    details,
                    "",
                    false,
                    None,
                    OutboundQueueAction::Complete,
                    "MessageHeld",
                    &payload,
                )
                .await?;
            return Ok(if recorded.is_some() {
                ProcessOutcome::Held
            } else {
                ProcessOutcome::AlreadyBounced
            });
        }

        // Rewrite HTML links for click tracking and append an open pixel
        // before signing, so the DKIM signature covers the final body.
        let tracked = self.apply_tracking(message).await?;

        // DKIM-sign at delivery time when the message carries an
        // authenticated domain: with the domain's own key when it has one,
        // with the installation key otherwise. The stored message stays
        // unsigned, matching the Ruby behaviour.
        let identified =
            camelmailer_core::bounce::add_message_token_header(&tracked, &message.token);
        let raw_message = match message.domain_id {
            Some(domain_id) => match self.store.domain_by_id(domain_id).await {
                Ok(Some(domain)) => match dkim::signer_for_domain(
                    domain.dkim_private_key.as_deref(),
                    self.signer.as_ref(),
                ) {
                    Some(signer) => dkim::sign_and_prepend(
                        &identified,
                        &domain.name,
                        &self.dkim_selector,
                        &signer,
                        chrono::Utc::now().timestamp(),
                    ),
                    None => identified,
                },
                _ => identified,
            },
            None => identified,
        };

        let return_path = if let Some(domain) = &self.return_path_domain {
            let token = server_token.ok_or(sqlx::Error::RowNotFound)?;
            format!("{token}@{domain}")
        } else {
            message.mail_from.clone()
        };

        // Source-address selection: send from the message stream's IP pool if
        // it sets one, else the server's pool (highest-priority IPv4). A
        // stream without a pool resolves to the server pool exactly as before.
        let source_ip = camelmailer_core::ServerStore::source_ip_for(
            &self.store,
            message.server_id,
            message.stream_id,
        )
        .await
        .ok()
        .flatten()
        .and_then(|ip| ip.parse().ok());

        let outcome = self
            .sender
            .send(
                &queued.domain,
                &return_path,
                &message.rcpt_to,
                &raw_message,
                source_ip,
            )
            .await;

        match outcome {
            SendOutcome::Sent { response, tls } => {
                let payload = self.message_payload(message, &response);
                let recorded = self
                    .sink
                    .record_outbound_delivery(
                        message.server_id,
                        message.id,
                        queued.id,
                        "Sent",
                        "message accepted by the remote server",
                        &response,
                        tls,
                        None,
                        OutboundQueueAction::Complete,
                        "MessageSent",
                        &payload,
                    )
                    .await?;
                if recorded.is_none() {
                    tracing::info!(
                        message_id = message.id,
                        "preserved newer bounced state after outbound SMTP success"
                    );
                    return Ok(ProcessOutcome::AlreadyBounced);
                }
                Ok(ProcessOutcome::Delivered { response })
            }
            SendOutcome::SoftFail { response } => {
                if queued.attempts + 1 >= self.max_attempts {
                    // Terminal failure: classify the bounce from the last
                    // SMTP response (5xx -> hard, 4xx -> soft, otherwise
                    // undetermined — see camelmailer_core::bounce).
                    let category = camelmailer_core::bounce::classify_response(&response);
                    let payload = self.message_payload(message, &response);
                    let recorded = self
                        .sink
                        .record_outbound_delivery(
                            message.server_id,
                            message.id,
                            queued.id,
                            "HardFail",
                            "delivery attempts exhausted",
                            &response,
                            false,
                            Some(category.as_str()),
                            OutboundQueueAction::Complete,
                            "MessageDeliveryFailed",
                            &payload,
                        )
                        .await?;
                    Ok(if recorded.is_some() {
                        ProcessOutcome::Failed { response }
                    } else {
                        ProcessOutcome::AlreadyBounced
                    })
                } else {
                    let payload = self.message_payload(message, &response);
                    let recorded = self
                        .sink
                        .record_outbound_delivery(
                            message.server_id,
                            message.id,
                            queued.id,
                            "SoftFail",
                            "temporary delivery failure",
                            &response,
                            false,
                            // transient — the message may still deliver, so
                            // no bounce category is persisted yet
                            None,
                            OutboundQueueAction::Retry {
                                attempts: queued.attempts,
                            },
                            "MessageDelayed",
                            &payload,
                        )
                        .await?;
                    Ok(if recorded.is_some() {
                        ProcessOutcome::Delayed { response }
                    } else {
                        ProcessOutcome::AlreadyBounced
                    })
                }
            }
            SendOutcome::HardFail { response } => {
                let category = camelmailer_core::bounce::classify_response(&response);
                let payload = self.message_payload(message, &response);
                let recorded = self
                    .sink
                    .record_outbound_delivery(
                        message.server_id,
                        message.id,
                        queued.id,
                        "HardFail",
                        "message rejected by the remote server",
                        &response,
                        false,
                        Some(category.as_str()),
                        OutboundQueueAction::Complete,
                        "MessageDeliveryFailed",
                        &payload,
                    )
                    .await?;
                Ok(if recorded.is_some() {
                    ProcessOutcome::Failed { response }
                } else {
                    ProcessOutcome::AlreadyBounced
                })
            }
        }
    }

    /// Inspect an incoming message with rspamd/clamav (when enabled) and
    /// record the verdict. Returns true when the message is a virus threat
    /// or exceeds the spam-failure threshold and should be held.
    async fn inspect(&self, message: &mut StoredMessage) -> Result<bool, sqlx::Error> {
        if self.rspamd.is_none() && self.clamav.is_none() {
            return Ok(false);
        }
        if message.inspected {
            return Ok(message.threat || message.spam_status == "SpamFailure");
        }

        let mut spam_status = "NotChecked".to_string();
        let mut spam_score = 0.0;
        if let Some(rspamd) = &self.rspamd {
            match rspamd
                .check(&message.raw_message, self.spam_threshold)
                .await
            {
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

        // Later inbound handlers build payloads from this value. Keep it in
        // sync with the verdict just persisted instead of using the row that
        // was loaded before inspection.
        message.spam_status = spam_status.clone();
        message.spam_score = spam_score;
        message.threat = threat;
        message.threat_details = threat_details;
        message.inspected = true;

        Ok(threat || spam_status == "SpamFailure")
    }

    async fn process_incoming(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &mut StoredMessage,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        // Inspect every incoming message before any content-specific handler;
        // a virus or spam-failure message is held (stored, not delivered).
        if self.inspect(message).await? {
            self.sink
                .record_delivery(
                    message.server_id,
                    message.id,
                    "Held",
                    "message failed inspection (spam or virus)",
                    "",
                    false,
                    None,
                )
                .await?;
            self.queue.complete(queued.id).await?;
            return Ok(ProcessOutcome::Held);
        }

        // Feedback-loop (ARF) reports: an ISP delivers a spam complaint as a
        // multipart/report; report-type=feedback-report. Recognise it by its
        // envelope (content-based, independent of routing) and record a
        // stream-scoped complaint for the recipient who complained.
        if crate::arf::is_feedback_report(&message.raw_message) {
            return self.ingest_feedback_report(queued, message).await;
        }

        // Correlate only machine-readable delivery-status notifications.
        // Auto-replies and other messages addressed to the return path retain
        // the existing route/fallback behaviour below.
        if let Some(outcome) = self.correlate_bounce(queued, message).await? {
            return Ok(outcome);
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
            (r.mode == RouteMode::Endpoint)
                .then(|| r.endpoint_url.clone())
                .flatten()
        });

        let Some(endpoint_url) = endpoint_url else {
            // Accept/Hold routes and bounces: the message is stored; there is
            // nothing to deliver.
            self.queue.complete(queued.id).await?;
            return Ok(ProcessOutcome::NothingToDo);
        };

        // The internal DMARC target: parse the message as an aggregate
        // report instead of POSTing it anywhere.
        if endpoint_url == camelmailer_core::DMARC_REPORTS_ENDPOINT {
            return self.ingest_dmarc_report(queued, message).await;
        }

        // SSRF guard: refuse to POST to a route endpoint that resolves to a
        // non-global (loopback / private / link-local / …) address unless the
        // operator explicitly allowed it. Treat a blocked endpoint like any
        // other failed delivery (retry with backoff, then terminal fail).
        if let Err(error) = self.ssrf_guard.check_url(&endpoint_url).await {
            tracing::warn!(%error, endpoint = %endpoint_url, "route endpoint blocked by SSRF guard");
            let response = format!("route endpoint blocked by address guard: {error}");
            return if queued.attempts + 1 >= self.max_attempts {
                self.queue.complete(queued.id).await?;
                Ok(ProcessOutcome::Failed { response })
            } else {
                self.queue.retry(queued.id, queued.attempts).await?;
                Ok(ProcessOutcome::Delayed { response })
            };
        }

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

    async fn correlate_bounce(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
    ) -> Result<Option<ProcessOutcome>, sqlx::Error> {
        if !message.bounce
            || !camelmailer_core::bounce::is_failed_delivery_status_notification(
                &message.raw_message,
            )
        {
            return Ok(None);
        }
        let category = camelmailer_core::bounce::classify_dsn(&message.raw_message);
        let tokens = camelmailer_core::bounce::original_message_tokens(&message.raw_message);
        let correlation = self
            .sink
            .correlate_bounce(
                message.server_id,
                message.id,
                &tokens,
                category.as_str(),
                |original| {
                    json!({
                        "original_message": self.bounce_webhook_payload(original.into()),
                        "bounce": self.bounce_webhook_payload(message.into()),
                    })
                },
            )
            .await?;
        match correlation {
            BounceCorrelationOutcome::Correlated(_) => {}
            BounceCorrelationOutcome::AlreadyCorrelated => {
                self.queue.complete(queued.id).await?;
                return Ok(Some(ProcessOutcome::BounceCorrelated));
            }
            BounceCorrelationOutcome::NotMatched if message.route_id.is_none() => {
                self.sink
                    .record_unmatched_bounce(
                        message.server_id,
                        message.id,
                        queued.id,
                        category.as_str(),
                        UNMATCHED_BOUNCE_DETAILS,
                    )
                    .await?;
                return Ok(Some(ProcessOutcome::Failed {
                    response: UNMATCHED_BOUNCE_DETAILS.to_string(),
                }));
            }
            BounceCorrelationOutcome::NotMatched => return Ok(None),
        }
        self.queue.complete(queued.id).await?;
        Ok(Some(ProcessOutcome::BounceCorrelated))
    }

    /// Parse an inbound message as a DMARC aggregate report and store it
    /// in the tenant's report tables. Parse failures hold the message
    /// (like any undeliverable inbound mail); storage failures retry with
    /// backoff. Never panics — a malformed report must not take the
    /// worker down.
    async fn ingest_dmarc_report(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        let report = match crate::dmarc::extract_report(&message.raw_message) {
            Ok(report) => report,
            Err(error) => {
                tracing::warn!(%error, message_id = message.id, "unparseable DMARC report held");
                self.queue.complete(queued.id).await?;
                self.sink
                    .record_delivery(
                        message.server_id,
                        message.id,
                        "Held",
                        "message could not be parsed as a DMARC aggregate report",
                        &error.to_string(),
                        false,
                        None,
                    )
                    .await?;
                return Ok(ProcessOutcome::Held);
            }
        };

        let new = camelmailer_core::NewDmarcReport {
            server_id: message.server_id,
            domain: report.domain,
            org_name: report.org_name,
            org_email: report.org_email,
            report_id: report.report_id,
            date_range_begin: report.date_range_begin,
            date_range_end: report.date_range_end,
            records: report
                .records
                .into_iter()
                .map(|record| camelmailer_core::NewDmarcRecord {
                    source_ip: record.source_ip,
                    count: record.count,
                    disposition: record.disposition,
                    dkim_result: record.dkim_result,
                    spf_result: record.spf_result,
                    dkim_aligned: record.dkim_aligned,
                    spf_aligned: record.spf_aligned,
                    header_from: record.header_from,
                    envelope_from: record.envelope_from,
                })
                .collect(),
        };
        match camelmailer_core::ServerStore::store_dmarc_report(&self.store, new).await {
            Ok(stored) => {
                self.queue.complete(queued.id).await?;
                self.sink
                    .record_delivery(
                        message.server_id,
                        message.id,
                        "Processed",
                        &format!(
                            "DMARC aggregate report for {} stored (#{}, {} records)",
                            stored.domain, stored.id, stored.record_count
                        ),
                        "",
                        false,
                        None,
                    )
                    .await?;
                Ok(ProcessOutcome::DmarcReportIngested)
            }
            Err(error) => {
                // storage trouble is transient — retry like a failing
                // endpoint instead of losing the report
                tracing::warn!(%error, message_id = message.id, "could not store DMARC report");
                let response = format!("could not store the DMARC report: {error}");
                if queued.attempts + 1 >= self.max_attempts {
                    self.queue.complete(queued.id).await?;
                    Ok(ProcessOutcome::Failed { response })
                } else {
                    self.queue.retry(queued.id, queued.attempts).await?;
                    Ok(ProcessOutcome::Delayed { response })
                }
            }
        }
    }

    /// Parse an inbound message as an ARF feedback-loop (spam-complaint)
    /// report and, when it is an abuse report, record the complaint and queue
    /// `MessageComplained`. A broadcast recipient (resolved via the
    /// List-Unsubscribe token) also gets a stream-scoped complaint
    /// suppression and opt-out. Parse failures hold the message (like any
    /// undeliverable inbound mail); storage failures retry with backoff.
    /// Mirrors [`Self::ingest_dmarc_report`]; never panics.
    async fn ingest_feedback_report(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        let report = match crate::arf::extract_feedback(&message.raw_message) {
            Ok(report) => report,
            Err(error) => {
                tracing::warn!(%error, message_id = message.id, "unparseable ARF report held");
                self.queue.complete(queued.id).await?;
                self.sink
                    .record_delivery(
                        message.server_id,
                        message.id,
                        "Held",
                        "message could not be parsed as an ARF feedback report",
                        &error.to_string(),
                        false,
                        None,
                    )
                    .await?;
                return Ok(ProcessOutcome::Held);
            }
        };

        // Abuse reports become complaints. A broadcast original resolves via
        // its List-Unsubscribe token (and is suppressed for that stream); any
        // other original resolves via the engine message token, Message-ID,
        // or recipient. Either way subscribed webhooks get
        // `MessageComplained`. Non-abuse feedback types are stored but
        // actioned no further, like an accepted inbound with nothing to
        // deliver.
        if report.feedback_type == "abuse" {
            let complaint = camelmailer_db::FeedbackComplaint {
                feedback_type: report.feedback_type.clone(),
                user_agent: report.user_agent.clone(),
                unsubscribe_token: report.unsubscribe_token.clone(),
                message_tokens: report.message_tokens.clone(),
                message_id: report.original_message_id.clone(),
                recipient: report.recipient(),
            };
            match self
                .sink
                .record_feedback_complaint(message.server_id, &complaint)
                .await
            {
                Ok(outcome) => {
                    self.queue.complete(queued.id).await?;
                    let detail = match outcome {
                        FeedbackComplaintOutcome::Recorded {
                            suppressed: true, ..
                        } => "spam complaint recorded for the broadcast recipient".to_string(),
                        FeedbackComplaintOutcome::Recorded {
                            message_id: Some(id),
                            ..
                        } => format!("spam complaint recorded for <msg:{id}>"),
                        FeedbackComplaintOutcome::Recorded { address, .. } => {
                            format!("spam complaint recorded for {address}; no matching message")
                        }
                        FeedbackComplaintOutcome::NotMatched => {
                            "feedback report parsed; no matching message or recipient".to_string()
                        }
                    };
                    self.sink
                        .record_delivery(
                            message.server_id,
                            message.id,
                            "Processed",
                            &detail,
                            "",
                            false,
                            None,
                        )
                        .await?;
                    Ok(ProcessOutcome::FeedbackReportIngested)
                }
                Err(error) => {
                    // storage trouble is transient — retry like a failing
                    // endpoint instead of losing the complaint
                    tracing::warn!(%error, message_id = message.id, "could not record feedback complaint");
                    let response = format!("could not record the feedback complaint: {error}");
                    if queued.attempts + 1 >= self.max_attempts {
                        self.queue.complete(queued.id).await?;
                        Ok(ProcessOutcome::Failed { response })
                    } else {
                        self.queue.retry(queued.id, queued.attempts).await?;
                        Ok(ProcessOutcome::Delayed { response })
                    }
                }
            }
        } else {
            // Recognised ARF of a non-abuse type. Store and move on.
            self.queue.complete(queued.id).await?;
            self.sink
                .record_delivery(
                    message.server_id,
                    message.id,
                    "Processed",
                    &format!(
                        "feedback report ({}) recorded; no complaint actioned",
                        report.feedback_type
                    ),
                    "",
                    false,
                    None,
                )
                .await?;
            Ok(ProcessOutcome::FeedbackReportIngested)
        }
    }

    /// Register tracking tokens and rewrite the HTML body of an outgoing
    /// message. No-op for non-HTML messages. Returns the (possibly
    /// rewritten) raw message.
    async fn apply_tracking(&self, message: &StoredMessage) -> Result<Vec<u8>, sqlx::Error> {
        // Never rewrite a cryptographically signed message: injecting the open
        // pixel or rewriting a link would invalidate an S/MIME, PGP/MIME or
        // inline-PGP signature over the body.
        if tracking::is_signed(&message.raw_message) {
            return Ok(message.raw_message.clone());
        }

        // The decoded HTML of the first text/html part; a message without one
        // (or in a charset that cannot be rewritten safely) goes as it came.
        let Some(body) = tracking::html_of(&message.raw_message) else {
            return Ok(message.raw_message.clone());
        };

        // Per-server track domains take precedence over the installation-wide
        // `dns.track_domain`; a lookup failure falls back to the global base
        // rather than failing the delivery.
        let tracking_base_url =
            camelmailer_core::AdminStore::effective_track_domain(&self.store, message.server_id)
                .await
                .ok()
                .flatten()
                .map(|host| format!("{}://{host}", self.web_protocol))
                .unwrap_or_else(|| self.tracking_base_url.clone());

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
                &format!("{tracking_base_url}/track/c/{token}"),
            );
        }

        // open-tracking pixel
        let open_token = self
            .store
            .create_open_token(message.server_id, message.id)
            .await?;
        let pixel_url = format!("{tracking_base_url}/track/o/{open_token}.gif");
        let rewritten = tracking::inject_open_pixel(&rewritten, &pixel_url);

        Ok(tracking::with_html(&message.raw_message, &rewritten)
            .unwrap_or_else(|| message.raw_message.clone()))
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

    /// Postal's `MessageBounced` contract uses `direction`, `to` and `from`.
    /// Keep those names so Postal webhook consumers can migrate unchanged.
    fn bounce_webhook_payload(&self, message: BounceWebhookFields<'_>) -> serde_json::Value {
        json!({
            "id": message.id,
            "token": message.token,
            "direction": message.direction,
            "message_id": message.message_id,
            "to": message.to,
            "from": message.from_address,
            "subject": message.subject,
            "timestamp": message.timestamp,
            "spam_status": message.spam_status,
            "tag": message.tag,
        })
    }

    /// Deliver one queued webhook request, if any is ready. Signs the body
    /// with the installation signing key when the webhook asks for it,
    /// records every attempt in the tenant-scoped audit log, and retries
    /// failures with backoff.
    pub async fn process_next_webhook(&self) -> Result<Option<WebhookOutcome>, sqlx::Error> {
        let Some(request) = self.webhook_queue.dequeue(&self.worker_id).await? else {
            return Ok(None);
        };

        // SSRF guard: never fetch a webhook URL that resolves to a non-global
        // address (would otherwise leak up to 2 KB of the reply into the
        // tenant-visible audit log). Record the blocked attempt and retry /
        // give up like any other failure — but never make the request.
        let attempt = request.attempts + 1;
        if let Err(error) = self.ssrf_guard.check_url(&request.url).await {
            tracing::warn!(%error, url = %request.url, "webhook blocked by SSRF guard");
            let detail = format!("blocked by address guard: {error}");
            self.webhook_queue
                .log_attempt(&request, attempt, None, false, &detail)
                .await?;
            return if attempt >= WEBHOOK_MAX_ATTEMPTS {
                self.webhook_queue.complete(request.id).await?;
                Ok(Some(WebhookOutcome::GivenUp))
            } else {
                self.webhook_queue
                    .retry(request.id, request.attempts)
                    .await?;
                Ok(Some(WebhookOutcome::Retrying))
            };
        }

        let mut http_request = self
            .http
            .post(&request.url)
            .header("content-type", "application/json");
        // custom webhook headers first (values are secrets — never logged),
        // then the platform headers, so the latter always win
        for (name, value) in &request.headers {
            use reqwest::header::{HeaderName, HeaderValue};
            match (
                HeaderName::try_from(name.as_str()),
                HeaderValue::try_from(value.as_str()),
            ) {
                (Ok(name), Ok(value)) => {
                    http_request = http_request.header(name, value);
                }
                _ => tracing::warn!(header = %name, "skipping invalid webhook header"),
            }
        }
        // `CamelMailer` here is frozen wire vocabulary, see the note in
        // `camelmailer_api::webhook_send`.
        http_request = http_request
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
            self.webhook_queue
                .retry(request.id, request.attempts)
                .await?;
            Ok(Some(WebhookOutcome::Retrying))
        }
    }

    /// Deliver one queued lifecycle event to every configured URL, if one is
    /// ready. The body was rendered at enqueue time; each request is signed
    /// with HMAC-SHA256 over `"{timestamp}.{body}"`. The event counts as
    /// delivered when every URL answered 2xx; otherwise it is retried with
    /// backoff (all URLs again, so receivers dedupe on the event id) and
    /// dropped after the last attempt. Lifecycle URLs are operator
    /// configuration, so the tenant SSRF guard does not apply to them.
    pub async fn process_next_lifecycle_event(
        &self,
    ) -> Result<Option<WebhookOutcome>, sqlx::Error> {
        if !self.lifecycle_webhooks.enabled() {
            return Ok(None);
        }
        let Some(event) = self.lifecycle_queue.dequeue(&self.worker_id).await? else {
            return Ok(None);
        };
        let secret = self.lifecycle_webhooks.secret.clone().unwrap_or_default();
        let mut failures = Vec::new();
        for url in &self.lifecycle_webhooks.urls {
            let timestamp = chrono::Utc::now().timestamp();
            let signature =
                camelmailer_core::lifecycle::signature_header(&secret, timestamp, &event.payload);
            let result = self
                .http
                .post(url)
                .header("content-type", "application/json")
                .header(camelmailer_core::lifecycle::EVENT_HEADER, &event.event)
                .header(camelmailer_core::lifecycle::EVENT_ID_HEADER, &event.uuid)
                .header(camelmailer_core::lifecycle::SIGNATURE_HEADER, signature)
                .body(event.payload.clone())
                .send()
                .await;
            match result {
                Ok(response) if response.status().is_success() => {}
                Ok(response) => failures.push(format!("{url}: HTTP {}", response.status())),
                Err(error) => failures.push(format!("{url}: {error}")),
            }
        }
        if failures.is_empty() {
            self.lifecycle_queue.complete(event.id).await?;
            return Ok(Some(WebhookOutcome::Delivered));
        }
        let detail = failures.join("; ");
        if event.attempts + 1 >= WEBHOOK_MAX_ATTEMPTS {
            tracing::warn!(event = %event.event, id = %event.uuid, %detail,
                "lifecycle event given up after {WEBHOOK_MAX_ATTEMPTS} attempts");
            self.lifecycle_queue.complete(event.id).await?;
            Ok(Some(WebhookOutcome::GivenUp))
        } else {
            tracing::info!(event = %event.event, id = %event.uuid, %detail,
                "lifecycle event delivery failed, retrying");
            self.lifecycle_queue
                .retry(event.id, event.attempts, &detail)
                .await?;
            Ok(Some(WebhookOutcome::Retrying))
        }
    }

    /// Test/ops helper: deliver every ready lifecycle event.
    pub async fn drain_lifecycle_events(&self) -> Result<usize, sqlx::Error> {
        let mut processed = 0;
        while self.process_next_lifecycle_event().await?.is_some() {
            processed += 1;
        }
        Ok(processed)
    }

    /// Test/ops helper: deliver every ready webhook request.
    pub async fn drain_webhooks(&self) -> Result<usize, sqlx::Error> {
        let mut processed = 0;
        while self.process_next_webhook().await?.is_some() {
            processed += 1;
        }
        Ok(processed)
    }

    /// Apply data retention once: per server (with its organization's
    /// overrides) message content, message metadata, the webhook log and
    /// DMARC reports; installation-wide the auth audit log and the API
    /// request log; and, always, expired authentication tokens. Every class
    /// is deleted in batches of `retention.batch_size`. A failing class is
    /// logged and the pass moves on.
    pub async fn apply_retention(&self) -> Result<camelmailer_db::RetentionReport, sqlx::Error> {
        let now = chrono::Utc::now();
        let batch = self.retention_batch;
        let mut report = camelmailer_db::RetentionReport::default();
        for target in self.store.retention_targets().await? {
            let periods = self.retention.with_overrides(&target.overrides);
            let server = target.server_id;
            if let Some(cutoff) =
                camelmailer_core::retention::cutoff(now, periods.message_metadata_days)
            {
                match self
                    .store
                    .prune_message_metadata(server, cutoff, batch)
                    .await
                {
                    Ok(n) => report.messages_deleted += n,
                    Err(error) => tracing::error!(%error, server, "message retention failed"),
                }
            }
            if let Some(cutoff) =
                camelmailer_core::retention::cutoff(now, periods.message_content_days)
            {
                match self
                    .store
                    .purge_message_content(server, cutoff, batch)
                    .await
                {
                    Ok(n) => report.message_content_purged += n,
                    Err(error) => tracing::error!(%error, server, "content retention failed"),
                }
            }
            if let Some(cutoff) = camelmailer_core::retention::cutoff(now, periods.webhook_log_days)
            {
                match self.store.prune_webhook_log(server, cutoff, batch).await {
                    Ok(n) => report.webhook_log_deleted += n,
                    Err(error) => tracing::error!(%error, server, "webhook log retention failed"),
                }
            }
            if let Some(cutoff) =
                camelmailer_core::retention::cutoff(now, periods.dmarc_report_days)
            {
                match self.store.prune_dmarc_reports(server, cutoff, batch).await {
                    Ok(n) => report.dmarc_reports_deleted += n,
                    Err(error) => tracing::error!(%error, server, "DMARC retention failed"),
                }
            }
        }
        if self.auth_event_days > 0 {
            let cutoff = now - chrono::Duration::days(self.auth_event_days as i64);
            report.auth_events_deleted = self.store.prune_auth_events(cutoff, batch).await?;
        }
        if self.api_request_days > 0 {
            let cutoff = now - chrono::Duration::days(self.api_request_days as i64);
            report.api_requests_deleted = self.store.prune_api_request_log(cutoff, batch).await?;
        }
        report.expired_tokens_deleted = self.store.prune_expired_auth_tokens(now, batch).await?;
        Ok(report)
    }

    /// Housekeeping: data retention ([`Worker::apply_retention`]), expired
    /// send-idempotency responses, send-limit counter buckets that have left
    /// every window, and the lifecycle outbox when nothing can deliver it.
    /// Returns how many API-request rows were removed. Runs periodically
    /// from [`Worker::run`].
    pub async fn housekeep(&self) -> Result<u64, camelmailer_core::StoreError> {
        let now = chrono::Utc::now();
        let expired_idempotency =
            camelmailer_core::ServerStore::prune_idempotency_requests(&self.store, now).await?;
        if expired_idempotency > 0 {
            tracing::info!(
                pruned = expired_idempotency,
                "pruned expired send-idempotency responses"
            );
        }

        let removed = match self.apply_retention().await {
            Ok(report) => {
                if !report.is_empty() {
                    tracing::info!(?report, "data retention applied");
                }
                report.api_requests_deleted
            }
            Err(error) => {
                tracing::error!(%error, "data retention error");
                0
            }
        };

        // Send-limit counter buckets that have left every 30-day window.
        // Failing to prune them is a housekeeping annoyance rather than a
        // reason to abandon the rest of the pass, so it only logs.
        match self.sink.prune_send_counters().await {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "pruned expired send-limit counters"),
            Err(error) => tracing::error!(%error, "send-counter pruning error"),
        }

        // Without a lifecycle URL nothing can deliver the outbox (usage
        // threshold events are written by the insert transaction whether or
        // not anyone listens), so keep it from growing.
        if !self.lifecycle_webhooks.enabled() {
            match self.lifecycle_queue.discard_all().await {
                Ok(0) => {}
                Ok(dropped) => tracing::info!(
                    dropped,
                    "dropped lifecycle events: no lifecycle_webhooks.urls configured"
                ),
                Err(error) => tracing::error!(%error, "lifecycle outbox pruning error"),
            }
        }
        Ok(removed)
    }

    /// An extra delivery loop: deliver queued messages, poll when there are
    /// none. Never returns.
    async fn deliver_loop(&self) {
        loop {
            match self.process_next().await {
                Ok(Some(outcome)) => tracing::debug!(?outcome, "processed queued message"),
                Ok(None) => tokio::time::sleep(Duration::from_secs(1)).await,
                Err(error) => {
                    tracing::error!(%error, "queue processing error");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// The long-running worker loop: drain the queue, then poll. Runs
    /// housekeeping once at startup and then hourly. With `threads` above 1,
    /// further loops deliver queued messages alongside; webhooks, lifecycle
    /// events and housekeeping stay on this one.
    pub async fn run(self: Arc<Self>) -> Result<(), sqlx::Error> {
        let mut delivery = tokio::task::JoinSet::new();
        for _ in 1..self.threads {
            let worker = Arc::clone(&self);
            delivery.spawn(async move { worker.deliver_loop().await });
        }
        tracing::info!(worker_id = %self.worker_id, threads = self.threads, "camelmailer worker started");
        let mut last_housekeeping: Option<std::time::Instant> = None;
        loop {
            if last_housekeeping.is_none_or(|at| at.elapsed() >= HOUSEKEEPING_INTERVAL) {
                last_housekeeping = Some(std::time::Instant::now());
                match self.housekeep().await {
                    Ok(0) => {}
                    Ok(removed) => {
                        tracing::info!(removed, "pruned expired API request-log entries")
                    }
                    Err(error) => tracing::error!(%error, "housekeeping error"),
                }
            }
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
            match self.process_next_lifecycle_event().await {
                Ok(Some(outcome)) => {
                    idle = false;
                    tracing::debug!(?outcome, "processed lifecycle event");
                }
                Ok(None) => {}
                Err(error) => tracing::error!(%error, "lifecycle event processing error"),
            }
            if idle {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
