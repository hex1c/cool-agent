#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented
)]

use std::sync::Mutex;

use application::external_operation::{ExternalResourceId, ProviderOutcome};
use domain::confirmation::{
    ConfirmationAction, ConfirmationRecord, ConsumedConfirmation, MutationTargetFingerprint,
    PendingConfirmation, PreviewDigest, TopicMessageReference,
};
use domain::idempotency::OperationTargetFingerprint;
use domain::identity::{
    ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
};
use domain::workflow::{WaitDeadline, WorkflowRevision, WorkflowTimestamp};

use google::auth::GoogleAccessToken;
use google::existing_files::{
    ConfirmedMutationProof, ExistingFileError, FileResourceId, MutationPayload, TabOrSection,
};
use google::mutations::{
    GoogleMutationClient, GoogleMutationService, MutationError, MutationProviderOutcome,
};

use google::FieldUpdate;

// ── helpers ──────────────────────────────────────────────────────────────

fn topic() -> TopicSessionId {
    TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77).unwrap())
}

fn participant(value: i64) -> ParticipantId {
    ParticipantId::new(value).unwrap()
}

fn time(seconds: u64) -> WorkflowTimestamp {
    WorkflowTimestamp::from_unix_seconds(seconds)
}

fn fingerprint(bytes: [u8; 32]) -> MutationTargetFingerprint {
    MutationTargetFingerprint::new(bytes)
}

fn op_fingerprint(bytes: [u8; 32]) -> OperationTargetFingerprint {
    OperationTargetFingerprint::new(bytes)
}

fn digest(bytes: [u8; 32]) -> PreviewDigest {
    PreviewDigest::new(bytes)
}

fn pending_confirmation(
    action: ConfirmationAction,
    target: MutationTargetFingerprint,
) -> PendingConfirmation {
    PendingConfirmation::new(
        ConfirmationId::new("conf-1").unwrap(),
        WorkflowId::new("wf-1").unwrap(),
        WorkflowRevision::new(1),
        participant(101),
        topic(),
        digest([0u8; 32]),
        target,
        action,
        WaitDeadline::at(time(86_400)),
    )
}

fn consumed_record(
    action: ConfirmationAction,
    target: MutationTargetFingerprint,
) -> ConfirmationRecord {
    let pending = pending_confirmation(action, target);
    let consumed = ConsumedConfirmation::new(
        pending,
        participant(202),
        domain::authorization::MembershipAuthorizationSource::Live,
        TopicMessageReference::new(topic(), MessageId::new(8).unwrap()),
        time(8),
        WorkflowRevision::new(2),
    );
    ConfirmationRecord::Consumed(consumed)
}

fn file_id() -> FileResourceId {
    FileResourceId::new("abc123").expect("valid file id")
}

fn tab() -> TabOrSection {
    TabOrSection::new("Sheet1").expect("valid tab")
}

fn field_updates() -> Vec<FieldUpdate> {
    vec![FieldUpdate::new("Status", "Complete").expect("valid field update")]
}

fn mutation_payload(target_bytes: [u8; 32]) -> MutationPayload {
    MutationPayload::new(
        file_id(),
        tab(),
        field_updates(),
        op_fingerprint(target_bytes),
    )
    .expect("valid payload")
}

fn resource_id(value: &str) -> ExternalResourceId {
    ExternalResourceId::new(value).expect("valid resource id")
}

// ── mock clients ─────────────────────────────────────────────────────────

struct RecordingMutationClient {
    calls: Mutex<u32>,
    outcome: MutationProviderOutcome,
}

impl RecordingMutationClient {
    fn new(outcome: MutationProviderOutcome) -> Self {
        Self {
            calls: Mutex::new(0),
            outcome,
        }
    }

    fn call_count(&self) -> u32 {
        *self.calls.lock().expect("lock poisoned")
    }
}

impl GoogleMutationClient for RecordingMutationClient {
    type Error = String;

    async fn apply_mutation(
        &self,
        _token: &GoogleAccessToken,
        _payload: &MutationPayload,
    ) -> Result<MutationProviderOutcome, Self::Error> {
        *self.calls.lock().expect("lock poisoned") += 1;
        Ok(self.outcome.clone())
    }
}

struct FailingMutationClient;

impl GoogleMutationClient for FailingMutationClient {
    type Error = String;

    async fn apply_mutation(
        &self,
        _token: &GoogleAccessToken,
        _payload: &MutationPayload,
    ) -> Result<MutationProviderOutcome, Self::Error> {
        Err("test failure".to_string())
    }
}

// ── existing_file_mutation tests ─────────────────────────────────────────

mod existing_file_mutation {
    use super::*;

    #[test]
    fn proof_from_consumed_with_correct_action_succeeds() {
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint([0u8; 32]),
        );
        let proof = ConfirmedMutationProof::from_consumed(&record).expect("proof extracted");
        assert_eq!(proof.owner(), participant(101));
        assert_eq!(proof.mutation_target().as_bytes(), &[0u8; 32]);
    }

    #[test]
    fn proof_from_pending_record_rejected() {
        let record = ConfirmationRecord::Pending(pending_confirmation(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint([0u8; 32]),
        ));
        let err =
            ConfirmedMutationProof::from_consumed(&record).expect_err("should be unauthorized");
        assert!(matches!(err, ExistingFileError::Unauthorized));
    }

    #[test]
    fn proof_from_consumed_with_wrong_action_rejected() {
        let record = consumed_record(
            ConfirmationAction::StartDirectPdfGeneration,
            fingerprint([0u8; 32]),
        );
        let err =
            ConfirmedMutationProof::from_consumed(&record).expect_err("should be unauthorized");
        assert!(matches!(err, ExistingFileError::Unauthorized));
    }

    #[tokio::test]
    async fn target_mismatch_rejected_without_client_call() {
        let proof_target = fingerprint([1u8; 32]);
        let payload_target = [2u8; 32];

        let record = consumed_record(ConfirmationAction::StartSheetOrDocWrite, proof_target);
        let proof = ConfirmedMutationProof::from_consumed(&record).expect("proof extracted");
        let payload = mutation_payload(payload_target);

        let client = RecordingMutationClient::new(MutationProviderOutcome::Ambiguous);
        let service = GoogleMutationService::new(client);
        let token = GoogleAccessToken::new("tok".to_string());

        let err = service
            .apply_mutation(&token, &proof, &payload)
            .await
            .expect_err("should be target mismatch");

        assert!(matches!(err, MutationError::TargetMismatch));
        assert_eq!(service.client().call_count(), 0);
    }

    #[tokio::test]
    async fn applied_mutation_returns_accepted_with_resource_id() {
        let target_bytes = [42u8; 32];
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint(target_bytes),
        );
        let proof = ConfirmedMutationProof::from_consumed(&record).expect("proof extracted");
        let payload = mutation_payload(target_bytes);

        let expected_id = resource_id("file-xyz");
        let client =
            RecordingMutationClient::new(MutationProviderOutcome::Applied(expected_id.clone()));
        let service = GoogleMutationService::new(client);
        let token = GoogleAccessToken::new("tok".to_string());

        let outcome = service
            .apply_mutation(&token, &proof, &payload)
            .await
            .expect("mutation succeeded");

        match outcome {
            ProviderOutcome::Accepted { resource_id } => {
                let id = resource_id.expect("resource id present");
                assert_eq!(id, expected_id);
            }
            other => panic!("expected Accepted, got {other:?}"),
        }

        assert_eq!(service.client().call_count(), 1);
    }

    #[tokio::test]
    async fn ambiguous_client_outcome_maps_to_ambiguous_provider_outcome() {
        let target_bytes = [5u8; 32];
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint(target_bytes),
        );
        let proof = ConfirmedMutationProof::from_consumed(&record).expect("proof extracted");
        let payload = mutation_payload(target_bytes);

        let client = RecordingMutationClient::new(MutationProviderOutcome::Ambiguous);
        let service = GoogleMutationService::new(client);
        let token = GoogleAccessToken::new("tok".to_string());

        let outcome = service
            .apply_mutation(&token, &proof, &payload)
            .await
            .expect("mutation succeeded");

        assert!(matches!(outcome, ProviderOutcome::Ambiguous(_)));
    }

    #[tokio::test]
    async fn terminal_client_outcome_maps_to_terminal_failure() {
        let target_bytes = [6u8; 32];
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint(target_bytes),
        );
        let proof = ConfirmedMutationProof::from_consumed(&record).expect("proof extracted");
        let payload = mutation_payload(target_bytes);

        let client = RecordingMutationClient::new(MutationProviderOutcome::Terminal);
        let service = GoogleMutationService::new(client);
        let token = GoogleAccessToken::new("tok".to_string());

        let outcome = service
            .apply_mutation(&token, &proof, &payload)
            .await
            .expect("mutation succeeded");

        assert!(matches!(outcome, ProviderOutcome::TerminalFailure(_)));
    }

    #[tokio::test]
    async fn client_error_maps_to_retryable_failure() {
        let target_bytes = [7u8; 32];
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint(target_bytes),
        );
        let proof = ConfirmedMutationProof::from_consumed(&record).expect("proof extracted");
        let payload = mutation_payload(target_bytes);

        let client = FailingMutationClient;
        let service = GoogleMutationService::new(client);
        let token = GoogleAccessToken::new("tok".to_string());

        let outcome = service
            .apply_mutation(&token, &proof, &payload)
            .await
            .expect("mutation succeeded");

        assert!(matches!(outcome, ProviderOutcome::RetryableFailure(_)));
    }

    #[tokio::test]
    async fn retries_return_same_stable_outcome() {
        // Provider-level idempotency is the journal's job; this test asserts
        // the provider outcome is deterministic and stable so the journal can
        // safely replay it.
        let target_bytes = [9u8; 32];
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint(target_bytes),
        );
        let proof = ConfirmedMutationProof::from_consumed(&record).expect("proof extracted");
        let payload = mutation_payload(target_bytes);

        let expected_id = resource_id("res-stable-2");
        let client =
            RecordingMutationClient::new(MutationProviderOutcome::Applied(expected_id.clone()));
        let service = GoogleMutationService::new(client);
        let token = GoogleAccessToken::new("tok".to_string());

        let outcome1 = service
            .apply_mutation(&token, &proof, &payload)
            .await
            .expect("first mutation");

        let outcome2 = service
            .apply_mutation(&token, &proof, &payload)
            .await
            .expect("second mutation");

        match (&outcome1, &outcome2) {
            (
                ProviderOutcome::Accepted { resource_id: id1 },
                ProviderOutcome::Accepted { resource_id: id2 },
            ) => {
                let rid1 = id1.as_ref().expect("resource id 1");
                let rid2 = id2.as_ref().expect("resource id 2");
                assert_eq!(rid1, &expected_id);
                assert_eq!(rid2, &expected_id);
            }
            _ => panic!("expected both Accepted, got {outcome1:?} / {outcome2:?}"),
        }
    }
}
