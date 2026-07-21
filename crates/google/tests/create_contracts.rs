#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented
)]

use std::convert::Infallible;
use std::sync::Mutex;

use application::config::DriveConfig;
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
use google::drive::{
    DriveDestination, FolderId, ResolvedDestination, SharedDriveId, resolve_destination,
};
use google::sheets_docs::{
    ConfirmedCreateProof, FileKind, GoogleCreateClient, GoogleCreateService, NewFileError,
    NewFileRequest,
};

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

fn config_with_default(folder_id: &str) -> DriveConfig {
    DriveConfig {
        default_folder_id: folder_id.to_string(),
        shared_drive_id: None,
    }
}

fn config_with_shared_drive(sd_id: &str) -> DriveConfig {
    DriveConfig {
        default_folder_id: String::new(),
        shared_drive_id: Some(sd_id.to_string()),
    }
}

fn empty_config() -> DriveConfig {
    DriveConfig {
        default_folder_id: String::new(),
        shared_drive_id: None,
    }
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

fn resolved_dest() -> ResolvedDestination {
    let folder = FolderId::new("folder1").expect("valid folder id");
    ResolvedDestination::new(Some(folder), None)
}

// ── mock client ──────────────────────────────────────────────────────────

struct RecordingCreateClient {
    calls: Mutex<u32>,
    fixed_id: String,
}

impl RecordingCreateClient {
    fn new(fixed_id: impl Into<String>) -> Self {
        Self {
            calls: Mutex::new(0),
            fixed_id: fixed_id.into(),
        }
    }
}

impl GoogleCreateClient for RecordingCreateClient {
    type Error = Infallible;

    async fn create_file(
        &self,
        _token: &GoogleAccessToken,
        _kind: FileKind,
        _title: &str,
        _destination: &ResolvedDestination,
    ) -> Result<ExternalResourceId, Self::Error> {
        *self.calls.lock().expect("lock poisoned") += 1;
        Ok(ExternalResourceId::new(self.fixed_id.clone()).expect("valid resource id"))
    }
}

// ── drive_destination tests ──────────────────────────────────────────────

mod drive_destination {
    use google::drive::DriveDestinationError;

    use super::*;

    #[test]
    fn company_default_uses_config_folder() {
        let config = config_with_default("root123");
        let resolved = resolve_destination(&DriveDestination::CompanyDefault, &config, None)
            .expect("resolve succeeded");
        let folder = resolved.folder_id().expect("folder_id present");
        assert_eq!(folder.as_str(), "root123");
        assert!(resolved.shared_drive().is_none());
    }

    #[test]
    fn employee_override_wins_over_company_default() {
        let config = config_with_default("root123");
        let emp = FolderId::new("emp1").expect("valid folder id");
        let resolved = resolve_destination(
            &DriveDestination::EmployeeOverride(emp.clone()),
            &config,
            None,
        )
        .expect("resolve succeeded");
        let folder = resolved.folder_id().expect("folder_id present");
        assert_eq!(folder.as_str(), "emp1");
    }

    #[test]
    fn workflow_override_wins_over_employee_and_company() {
        let config = config_with_default("root123");
        let emp = FolderId::new("emp1").expect("valid folder id");
        let wf = FolderId::new("wf1").expect("valid folder id");
        let resolved = resolve_destination(
            &DriveDestination::WorkflowOverride(wf.clone()),
            &config,
            Some(&emp),
        )
        .expect("resolve succeeded");
        let folder = resolved.folder_id().expect("folder_id present");
        assert_eq!(folder.as_str(), "wf1");
    }

    #[test]
    fn my_drive_has_no_folder_or_shared_drive() {
        let config = config_with_default("root123");
        let resolved = resolve_destination(&DriveDestination::MyDrive, &config, None)
            .expect("resolve succeeded");
        assert!(resolved.folder_id().is_none());
        assert!(resolved.shared_drive().is_none());
    }

    #[test]
    fn shared_drive_with_permission() {
        let config = config_with_shared_drive("sd1");
        let sd = SharedDriveId::new("sd1").expect("valid shared drive id");
        let resolved =
            resolve_destination(&DriveDestination::SharedDrive(sd.clone()), &config, None)
                .expect("resolve succeeded");
        let shared = resolved.shared_drive().expect("shared_drive present");
        assert_eq!(shared.as_str(), "sd1");
    }

    #[test]
    fn shared_drive_without_permission_rejected() {
        let config = empty_config();
        let sd = SharedDriveId::new("sd1").expect("valid shared drive id");
        let err = resolve_destination(&DriveDestination::SharedDrive(sd), &config, None)
            .expect_err("should be rejected");
        assert!(matches!(
            err,
            DriveDestinationError::SharedDriveNotPermitted
        ));
    }

    #[test]
    fn company_default_with_empty_config_no_override_rejected() {
        let config = empty_config();
        let err = resolve_destination(&DriveDestination::CompanyDefault, &config, None)
            .expect_err("should be rejected");
        assert!(matches!(err, DriveDestinationError::NoUsableDefault));
    }

    #[test]
    fn employee_default_used_for_company_default_selection() {
        let config = config_with_default("root123");
        let emp = FolderId::new("emp1").expect("valid folder id");
        let resolved = resolve_destination(&DriveDestination::CompanyDefault, &config, Some(&emp))
            .expect("resolve succeeded");
        let folder = resolved.folder_id().expect("folder_id present");
        assert_eq!(folder.as_str(), "emp1");
    }
}

// ── create_file tests ────────────────────────────────────────────────────

mod create_file {
    use super::*;

    #[test]
    fn from_consumed_with_correct_action_succeeds() {
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint([0u8; 32]),
        );
        let proof = ConfirmedCreateProof::from_consumed(&record).expect("proof extracted");
        assert_eq!(proof.owner(), participant(101));
        assert_eq!(proof.mutation_target().as_bytes(), &[0u8; 32]);
    }

    #[test]
    fn from_consumed_with_pending_record_rejected() {
        let record = ConfirmationRecord::Pending(pending_confirmation(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint([0u8; 32]),
        ));
        let err = ConfirmedCreateProof::from_consumed(&record).expect_err("should be unauthorized");
        assert!(matches!(err, NewFileError::Unauthorized));
    }

    #[test]
    fn from_consumed_with_wrong_action_rejected() {
        let record = consumed_record(
            ConfirmationAction::StartDirectPdfGeneration,
            fingerprint([0u8; 32]),
        );
        let err = ConfirmedCreateProof::from_consumed(&record).expect_err("should be unauthorized");
        assert!(matches!(err, NewFileError::Unauthorized));
    }

    #[tokio::test]
    async fn confirmed_create_returns_one_stable_resource_id() {
        let target_bytes = [42u8; 32];
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint(target_bytes),
        );
        let proof = ConfirmedCreateProof::from_consumed(&record).expect("proof extracted");

        let request = NewFileRequest::new(
            FileKind::Sheet,
            "Test Sheet".to_string(),
            resolved_dest(),
            op_fingerprint(target_bytes),
        )
        .expect("valid request");

        let client = RecordingCreateClient::new("res-abc123");
        let service = GoogleCreateService::new(client);
        let token = GoogleAccessToken::new("tok".to_string());

        let outcome = service
            .create_file(&token, &proof, &request)
            .await
            .expect("create succeeded");

        match outcome {
            ProviderOutcome::Accepted { resource_id } => {
                let id = resource_id.expect("resource id present");
                assert_eq!(id.as_str(), "res-abc123");
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_with_target_mismatch_rejected() {
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint([0u8; 32]),
        );
        let proof = ConfirmedCreateProof::from_consumed(&record).expect("proof extracted");

        let request = NewFileRequest::new(
            FileKind::Sheet,
            "Test Sheet".to_string(),
            resolved_dest(),
            op_fingerprint([1u8; 32]),
        )
        .expect("valid request");

        let client = RecordingCreateClient::new("res-abc123");
        let service = GoogleCreateService::new(client);
        let token = GoogleAccessToken::new("tok".to_string());

        let err = service
            .create_file(&token, &proof, &request)
            .await
            .expect_err("should be target mismatch");

        assert!(matches!(
            err,
            google::sheets_docs::CreateFileError::TargetMismatch
        ));
    }

    #[tokio::test]
    async fn retries_return_same_stable_resource_id() {
        // Provider-level idempotency is the journal's job; this test asserts
        // the provider outcome is deterministic and stable so the journal can
        // safely replay it.
        let target_bytes = [7u8; 32];
        let record = consumed_record(
            ConfirmationAction::StartSheetOrDocWrite,
            fingerprint(target_bytes),
        );
        let proof = ConfirmedCreateProof::from_consumed(&record).expect("proof extracted");

        let request = NewFileRequest::new(
            FileKind::Sheet,
            "Stable Sheet".to_string(),
            resolved_dest(),
            op_fingerprint(target_bytes),
        )
        .expect("valid request");

        let client = RecordingCreateClient::new("res-stable-1");
        let service = GoogleCreateService::new(client);
        let token = GoogleAccessToken::new("tok".to_string());

        let outcome1 = service
            .create_file(&token, &proof, &request)
            .await
            .expect("first create");

        let outcome2 = service
            .create_file(&token, &proof, &request)
            .await
            .expect("second create");

        let id1 = match outcome1 {
            ProviderOutcome::Accepted { resource_id } => resource_id.expect("resource id"),
            other => panic!("expected Accepted, got {other:?}"),
        };
        let id2 = match outcome2 {
            ProviderOutcome::Accepted { resource_id } => resource_id.expect("resource id"),
            other => panic!("expected Accepted, got {other:?}"),
        };

        // Both invocations return the same resource id (deterministic provider).
        assert_eq!(id1.as_str(), "res-stable-1");
        assert_eq!(id2.as_str(), "res-stable-1");
        // The mock is invoked twice; the journal prevents re-invoke at the
        // application layer (tested in Task 34).
    }
}
