//! Router regressions with a task-local gate after a real completed-claim lookup.
//! Only the retry is gated; the winner uses the same MemoryStore normally.
use super::*;
use axum::{body::Body, http::Request};
use camelmailer_core::{
    AdminStore, CredentialType, Domain, DomainOwner, Id, MemoryStore, NewCredential,
    NewOrganization, NewServer, ServerMode, ServerStore, StoreError,
};
use http_body_util::BodyExt;
use std::{
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::Notify;
use tower::ServiceExt;

#[test]
fn json_object_keys_are_sorted_for_idempotency_fingerprints() {
    // Request fingerprints rely on serde_json's default sorted object keys.
    // Enabling insertion-order preservation would make equivalent requests hash
    // differently and break replay compatibility, so this configuration change
    // must fail this test.
    let value: Value = serde_json::from_str(r#"{"z":{"b":2,"a":1},"a":[{"d":4,"c":3}]}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#"{"a":[{"c":3,"d":4}],"z":{"a":1,"b":2}}"#
    );
}

tokio::task_local! {
    static LOOKUP_GATE: Arc<LookupGate>;
}

#[derive(Default)]
struct LookupGate {
    calls: AtomicUsize,
    missed: Notify,
    resume: Notify,
    fail_on: Option<usize>,
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("idempotency test timed out")
}

pub(super) async fn after_lookup(
    result: Result<IdempotencyLookup, StoreError>,
) -> Result<IdempotencyLookup, StoreError> {
    let Ok(gate) = LOOKUP_GATE.try_with(Arc::clone) else {
        return result;
    };
    let call = gate.calls.fetch_add(1, Ordering::SeqCst) + 1;
    if gate.fail_on == Some(call) {
        return Err(StoreError::Other("injected lookup failure".into()));
    }
    if call == 1 {
        assert!(matches!(result, Ok(IdempotencyLookup::New)));
        gate.missed.notify_one();
        bounded(gate.resume.notified()).await;
    }
    result
}

struct Fixture {
    app: Router,
    store: Arc<MemoryStore>,
    server_id: Id,
}

impl Fixture {
    async fn new() -> Self {
        let store = Arc::new(MemoryStore::new());
        let org = store
            .create_organization(NewOrganization {
                name: "Org".into(),
                permalink: "org".into(),
            })
            .await
            .unwrap();
        let mut server = store
            .create_server(NewServer {
                organization_id: org.id,
                name: "Server".into(),
                permalink: "server".into(),
                mode: ServerMode::Live,
            })
            .await
            .unwrap();
        server.send_limit = Some(2);
        let server_id = server.id;
        store.update_server(server).await.unwrap();
        store.insert_domain(Domain {
            id: store.next_id(),
            uuid: "domain".into(),
            owner: DomainOwner::Server(server_id),
            name: "example.org".into(),
            verified: true,
            verification_token: "test".into(),
            check_dmarc: true,
            check_spf: true,
            dkim_private_key: None,
        });
        store
            .create_credential_record(NewCredential {
                server_id,
                credential_type: CredentialType::Api,
                name: "api".into(),
                key: Some("test-idempotency-token".into()),
            })
            .await
            .unwrap();
        store
            .create_template(NewTemplate {
                server_id,
                name: "Receipt".into(),
                permalink: "receipt".into(),
                subject: Some("Receipt".into()),
                text_body: Some("Thanks".into()),
                html_body: None,
                layout_id: None,
            })
            .await
            .unwrap();
        let app = build_server_router(ApiState::with_server_store(
            store.clone(),
            store.clone(),
            None,
        ));
        Self {
            app,
            store,
            server_id,
        }
    }

    async fn send(&self, templated: bool, key: Option<&str>, subject: &str) -> (StatusCode, Value) {
        let path = if templated {
            "/api/v2/server/messages/with_template"
        } else {
            "/api/v2/server/messages"
        };
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header("X-Server-API-Key", "test-idempotency-token")
            .header("Content-Type", "application/json");
        if let Some(key) = key {
            request = request.header("Idempotency-Key", key);
        }
        let mut body = json!({
            "from": "sender@example.org", "to": ["a@example.net", "b@example.net"],
            "subject": subject, "text_body": "Thanks"
        });
        if templated {
            body["template"] = json!("receipt");
        }
        let response = bounded(
            self.app
                .clone()
                .oneshot(request.body(Body::from(body.to_string())).unwrap()),
        )
        .await
        .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn remove_template_permalink(&self) {
        let mut template = self
            .store
            .template_by_permalink(self.server_id, "receipt")
            .await
            .unwrap()
            .unwrap();
        template.permalink = "renamed".into();
        self.store.update_template(template).await.unwrap();
    }
}

#[derive(Clone, Copy, Debug)]
enum Preparation {
    OrdinaryQuota,
    TemplateQuota,
    TemplateMissing,
}

impl Preparation {
    fn templated(self) -> bool {
        !matches!(self, Self::OrdinaryQuota)
    }

    fn error(self) -> (StatusCode, &'static str) {
        if matches!(self, Self::TemplateMissing) {
            (StatusCode::UNPROCESSABLE_ENTITY, "ValidationError")
        } else {
            (StatusCode::TOO_MANY_REQUESTS, "SendLimitExceeded")
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Claim {
    Matching,
    PayloadConflict,
    OperationConflict,
    Absent,
    LookupFailure,
}

async fn preparation_race(preparation: Preparation, claim: Claim) {
    let fixture = Fixture::new().await;
    let gate = Arc::new(LookupGate {
        fail_on: matches!(claim, Claim::LookupFailure).then_some(2),
        ..Default::default()
    });
    let retry = LOOKUP_GATE.scope(
        gate.clone(),
        fixture.send(preparation.templated(), Some("race"), "Receipt"),
    );
    let winner = async {
        bounded(gate.missed.notified()).await;
        let key = if matches!(claim, Claim::Absent) {
            "other"
        } else {
            "race"
        };
        let subject = if matches!(claim, Claim::PayloadConflict) {
            "Changed"
        } else {
            "Receipt"
        };
        let templated = preparation.templated() ^ matches!(claim, Claim::OperationConflict);
        let response = fixture.send(templated, Some(key), subject).await;
        assert_eq!(response.0, StatusCode::CREATED, "{response:?}");
        if matches!(preparation, Preparation::TemplateMissing) {
            fixture.remove_template_permalink().await;
        }
        gate.resume.notify_one();
        response
    };
    let (retry, winner) = bounded(async { tokio::join!(retry, winner) }).await;
    let expected = match claim {
        Claim::Matching => {
            assert_eq!(retry.1["data"], winner.1["data"]);
            assert_eq!(retry.1["data"]["recipients"].as_array().unwrap().len(), 2);
            (StatusCode::CREATED, None)
        }
        Claim::PayloadConflict | Claim::OperationConflict => {
            (StatusCode::CONFLICT, Some("InvalidIdempotentRequest"))
        }
        Claim::Absent => {
            let (status, code) = preparation.error();
            (status, Some(code))
        }
        Claim::LookupFailure => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("InternalServerError"),
        ),
    };
    assert_eq!(retry.0, expected.0, "{preparation:?}, {claim:?}: {retry:?}");
    if let Some(code) = expected.1 {
        assert_eq!(retry.1["error"]["code"], code);
    }
    assert_eq!(
        gate.calls.load(Ordering::SeqCst),
        2,
        "must reconcile after the preparation error"
    );
    assert_eq!(fixture.store.messages_for(fixture.server_id).len(), 2);
    assert_eq!(
        ServerStore::send_usage(fixture.store.as_ref(), fixture.server_id)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn ordinary_quota_errors_reconcile_completed_claims() {
    for claim in [
        Claim::Matching,
        Claim::PayloadConflict,
        Claim::OperationConflict,
        Claim::Absent,
        Claim::LookupFailure,
    ] {
        preparation_race(Preparation::OrdinaryQuota, claim).await;
    }
}

#[tokio::test]
async fn template_quota_errors_reconcile_completed_claims() {
    for claim in [
        Claim::Matching,
        Claim::PayloadConflict,
        Claim::OperationConflict,
        Claim::Absent,
        Claim::LookupFailure,
    ] {
        preparation_race(Preparation::TemplateQuota, claim).await;
    }
}

#[tokio::test]
async fn template_build_errors_reconcile_completed_claims() {
    for claim in [
        Claim::Matching,
        Claim::PayloadConflict,
        Claim::OperationConflict,
        Claim::Absent,
        Claim::LookupFailure,
    ] {
        preparation_race(Preparation::TemplateMissing, claim).await;
    }
}

#[tokio::test]
async fn initial_lookup_failure_preserves_internal_error() {
    for templated in [false, true] {
        let fixture = Fixture::new().await;
        let gate = Arc::new(LookupGate {
            fail_on: Some(1),
            ..Default::default()
        });
        let (status, body) = LOOKUP_GATE
            .scope(
                gate.clone(),
                fixture.send(templated, Some("race"), "Receipt"),
            )
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "InternalServerError");
        assert_eq!(body["error"]["message"], "An internal error occurred");
        assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
        assert!(fixture.store.messages_for(fixture.server_id).is_empty());
    }
}

#[tokio::test]
async fn unkeyed_preparation_errors_skip_claim_lookup() {
    for preparation in [
        Preparation::OrdinaryQuota,
        Preparation::TemplateQuota,
        Preparation::TemplateMissing,
    ] {
        let fixture = Fixture::new().await;
        assert_eq!(
            fixture
                .send(preparation.templated(), None, "Receipt")
                .await
                .0,
            StatusCode::CREATED
        );
        if matches!(preparation, Preparation::TemplateMissing) {
            fixture.remove_template_permalink().await;
        }
        let gate = Arc::new(LookupGate::default());
        let (status, body) = LOOKUP_GATE
            .scope(
                gate.clone(),
                fixture.send(preparation.templated(), None, "Receipt"),
            )
            .await;
        let expected = preparation.error();
        assert_eq!(status, expected.0);
        assert_eq!(body["error"]["code"], expected.1);
        assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.store.messages_for(fixture.server_id).len(), 2);
    }
}
