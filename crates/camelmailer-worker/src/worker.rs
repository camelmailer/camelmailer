//! The message dequeuer — the port of `app/lib/message_dequeuer` plus the
//! webhook dispatch of `app/models/webhook_request.rb`.
//!
//! The worker owns the cross-tenant queue but enters each message's tenant
//! context (via the RLS-aware accessors on `camelmailer-db`) to read
//! content, so tenant isolation never depends on worker code being careful.

use crate::sender::SmtpSender;
use crate::smtp_client::SendOutcome;
use base64::Engine;
use camelmailer_core::{AdminStore, Id, RouteMode};
use camelmailer_db::{PgMessageSink, PgQueue, PgStore, StoredMessage};
use serde_json::json;
use std::time::Duration;

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

pub struct Worker {
    store: PgStore,
    sink: PgMessageSink,
    queue: PgQueue,
    sender: SmtpSender,
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
        Self {
            store,
            sink,
            queue,
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
            self.send_webhooks(
                message.server_id,
                "MessageHeld",
                self.message_payload(message, "recipient is on the suppression list"),
            )
            .await;
            return Ok(ProcessOutcome::Held);
        }

        let outcome = self
            .sender
            .send(
                &queued.domain,
                &message.mail_from,
                &message.rcpt_to,
                &message.raw_message,
            )
            .await;

        match outcome {
            SendOutcome::Sent { response } => {
                self.queue.complete(queued.id).await?;
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
                    self.send_webhooks(
                        message.server_id,
                        "MessageDeliveryFailed",
                        self.message_payload(message, &response),
                    )
                    .await;
                    Ok(ProcessOutcome::Failed { response })
                } else {
                    self.queue.retry(queued.id, queued.attempts).await?;
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

    async fn process_incoming(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
    ) -> Result<ProcessOutcome, sqlx::Error> {
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

    /// POST an event to every enabled webhook of the server. Failures are
    /// logged, not retried (webhook request retrying is a later phase).
    async fn send_webhooks(&self, server_id: Id, event: &str, payload: serde_json::Value) {
        let webhooks = match self.store.list_webhooks(server_id).await {
            Ok(webhooks) => webhooks,
            Err(error) => {
                tracing::warn!(%error, server_id, "could not load webhooks");
                return;
            }
        };
        for webhook in webhooks.into_iter().filter(|w| w.enabled) {
            let body = json!({
                "event": event,
                "timestamp": chrono::Utc::now().timestamp(),
                "uuid": camelmailer_core::token::generate_uuid(),
                "payload": payload,
            });
            let result = self
                .http
                .post(&webhook.url)
                .header("X-CamelMailer-Event", event)
                .json(&body)
                .send()
                .await;
            match result {
                Ok(response) if response.status().is_success() => {}
                Ok(response) => {
                    tracing::warn!(webhook = %webhook.url, status = %response.status(), "webhook rejected");
                }
                Err(error) => {
                    tracing::warn!(webhook = %webhook.url, %error, "webhook delivery failed");
                }
            }
        }
    }

    /// The long-running worker loop: drain the queue, then poll.
    pub async fn run(&self) -> Result<(), sqlx::Error> {
        tracing::info!(worker_id = %self.worker_id, "camelmailer worker started");
        loop {
            match self.process_next().await {
                Ok(Some(outcome)) => {
                    tracing::debug!(?outcome, "processed queued message");
                }
                Ok(None) => tokio::time::sleep(Duration::from_secs(5)).await,
                Err(error) => {
                    tracing::error!(%error, "queue processing error");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}
