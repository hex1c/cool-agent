#![deny(unsafe_code)]

//! Artifact delivery handler (Task 34A).
//!
//! Deserializes a versioned [`DeliveryEvent`], rebuilds a validated
//! [`DeliveryRequest`], and runs it through an injected [`DeliveryRunner`]
//! (the [`ArtifactDeliveryService`] in production; a fake in tests). The
//! handler itself contains no retry, confirmation, or adapter logic — that
//! lives in the application layer. Outcomes are mapped to a serializable
//! [`DeliveryResultDto`] distinguishing accepted, retryable, ambiguous, and
//! terminal results.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};

use application::artifact_delivery::{
    ArtifactDeliveryService, DeliveryError, DeliveryRequest, DeliveryResult,
};
use application::external_operation::ExecutionOutcome;
use application::ports::{ObjectClass, StorageKey, StorageRecordId, StoredObject};
use domain::idempotency::IdempotencyKey;
use domain::identity::TopicSessionId;
use domain::retry::RetryPolicy;
use domain::workflow::WorkflowTimestamp;

use crate::EVENT_SCHEMA_VERSION;

/// Serializable artifact descriptor. Reconstructs a validated [`StoredObject`].
#[derive(Debug, Deserialize)]
pub struct ArtifactDto {
    #[serde(rename = "workflowId")]
    pub workflow_id: String,
    #[serde(rename = "objectId")]
    pub object_id: String,
    #[serde(rename = "storageKey")]
    pub storage_key: String,
    #[serde(rename = "byteLength")]
    pub byte_length: u64,
    #[serde(rename = "sha256Hex")]
    pub sha256_hex: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Serializable retry policy (the domain `RetryPolicy` is not `Serialize`).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct RetryPolicyDto {
    #[serde(rename = "maxAttempts")]
    pub max_attempts: u8,
    #[serde(rename = "baseDelayMs")]
    pub base_delay_ms: u32,
    #[serde(rename = "maximumDelayMs")]
    pub maximum_delay_ms: u32,
    #[serde(rename = "jitterBasisPoints")]
    pub jitter_basis_points: u16,
}

impl RetryPolicyDto {
    fn build(self) -> Result<RetryPolicy, DeliveryError> {
        RetryPolicy::new(
            self.max_attempts,
            self.base_delay_ms,
            self.maximum_delay_ms,
            self.jitter_basis_points,
        )
        .map_err(|_| DeliveryError::Execution {
            operation: "retry policy",
        })
    }
}

/// Versioned event consumed by the `delivery` Lambda.
#[derive(Debug, Deserialize)]
pub struct DeliveryEvent {
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    pub artifact: ArtifactDto,
    #[serde(rename = "pdfBytesBase64")]
    pub pdf_bytes_base64: String,
    #[serde(rename = "driveKey")]
    pub drive_key: IdempotencyKey,
    #[serde(rename = "telegramKey")]
    pub telegram_key: IdempotencyKey,
    #[serde(rename = "destinationName")]
    pub destination_name: String,
    pub topic: TopicSessionId,
    #[serde(rename = "retryPolicy")]
    pub retry_policy: RetryPolicyDto,
    #[serde(rename = "topicMessage")]
    pub topic_message: String,
}

/// Typed delivery outcome. Presigned links and raw provider strings are never
/// serialized — only stable labels and opaque resource ids.
#[derive(Debug, Clone, Serialize)]
pub struct DeliveryResultDto {
    #[serde(rename = "s3Outcome")]
    pub s3_outcome: &'static str,
    #[serde(rename = "driveResourceId")]
    pub drive_resource_id: Option<String>,
    #[serde(rename = "telegramOutcome")]
    pub telegram_outcome: &'static str,
    #[serde(rename = "outcome")]
    pub outcome: &'static str,
}

/// Abstraction over the delivery service so the handler is unit-testable
/// without AWS credentials. Production wires `ArtifactDeliveryService`; tests
/// inject a fake.
#[allow(async_fn_in_trait)]
pub trait DeliveryRunner {
    async fn run(&self, request: DeliveryRequest) -> Result<DeliveryResult, DeliveryError>;
}

impl<O, HS, M, H, J, Ji, W, D, T, S> DeliveryRunner
    for ArtifactDeliveryService<O, HS, M, H, J, Ji, W, D, T, S>
where
    O: application::ports::ObjectStore,
    HS: application::ports::HistoryStore,
    M: application::repositories::ObjectMetadataRepository,
    H: application::repositories::HistoryRepository,
    J: application::external_operation::OperationJournal,
    Ji: application::external_operation::JitterSource,
    W: application::external_operation::BackoffWait,
    D: application::artifact_delivery::DriveCopyPort,
    T: application::artifact_delivery::TopicDeliveryPort,
    S: application::ports::ArtifactLinkSigner,
    J::Error: std::fmt::Display,
    W::Error: std::fmt::Display,
{
    async fn run(&self, request: DeliveryRequest) -> Result<DeliveryResult, DeliveryError> {
        self.deliver(request).await
    }
}

/// Errors raised while deserializing/rebuilding the delivery request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryEventError {
    SchemaVersionMismatch,
    InvalidArtifact,
    InvalidBase64,
    InvalidRetryPolicy,
}

impl std::fmt::Display for DeliveryEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaVersionMismatch => f.write_str("delivery event: schema version mismatch"),
            Self::InvalidArtifact => f.write_str("delivery event: invalid artifact descriptor"),
            Self::InvalidBase64 => f.write_str("delivery event: invalid base64 pdf bytes"),
            Self::InvalidRetryPolicy => f.write_str("delivery event: invalid retry policy"),
        }
    }
}

impl std::error::Error for DeliveryEventError {}

impl DeliveryEvent {
    /// Rebuild a validated [`DeliveryRequest`] from the serialized event.
    pub fn build(self) -> Result<DeliveryRequest, DeliveryEventError> {
        if self.schema_version != EVENT_SCHEMA_VERSION {
            return Err(DeliveryEventError::SchemaVersionMismatch);
        }
        let workflow_id = domain::identity::WorkflowId::new(self.artifact.workflow_id)
            .map_err(|_| DeliveryEventError::InvalidArtifact)?;
        let object_id = StorageRecordId::new(self.artifact.object_id)
            .map_err(|_| DeliveryEventError::InvalidArtifact)?;
        let storage_key = StorageKey::new(self.artifact.storage_key)
            .map_err(|_| DeliveryEventError::InvalidArtifact)?;
        let sha256 = hex::decode(&self.artifact.sha256_hex)
            .map_err(|_| DeliveryEventError::InvalidArtifact)
            .and_then(|bytes| {
                <[u8; 32]>::try_from(bytes.as_slice())
                    .map_err(|_| DeliveryEventError::InvalidArtifact)
            })?;
        let created_at = WorkflowTimestamp::from_unix_seconds(self.artifact.created_at);
        let artifact = StoredObject {
            workflow_id,
            object_id,
            class: ObjectClass::Artifact,
            storage_key,
            byte_length: self.artifact.byte_length,
            sha256,
            media_type: self.artifact.media_type,
            created_at,
        };
        artifact
            .validate()
            .map_err(|_| DeliveryEventError::InvalidArtifact)?;

        let pdf_bytes = BASE64
            .decode(&self.pdf_bytes_base64)
            .map_err(|_| DeliveryEventError::InvalidBase64)?;
        if pdf_bytes.len() as u64 != artifact.byte_length {
            return Err(DeliveryEventError::InvalidBase64);
        }

        let retry_policy = self
            .retry_policy
            .build()
            .map_err(|_| DeliveryEventError::InvalidRetryPolicy)?;

        Ok(DeliveryRequest {
            artifact,
            pdf_bytes,
            drive_key: self.drive_key,
            telegram_key: self.telegram_key,
            destination_name: self.destination_name,
            topic: self.topic,
            retry_policy,
            topic_message: self.topic_message,
        })
    }
}

/// Pure preprocessing: validate the event and rebuild the request. The actual
/// delivery is performed by the injected [`DeliveryRunner`].
pub async fn process_delivery<R: DeliveryRunner>(
    event: DeliveryEvent,
    runner: &R,
) -> Result<DeliveryResultDto, DeliveryProcessError> {
    let request = event.build().map_err(DeliveryProcessError::from)?;
    let result = runner
        .run(request)
        .await
        .map_err(DeliveryProcessError::Delivery)?;
    Ok(DeliveryResultDto::from(result))
}

/// Combined handler error.
#[derive(Debug)]
pub enum DeliveryProcessError {
    Event(DeliveryEventError),
    Delivery(DeliveryError),
}

impl std::fmt::Display for DeliveryProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Event(e) => write!(f, "{e}"),
            Self::Delivery(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DeliveryProcessError {}

impl From<DeliveryEventError> for DeliveryProcessError {
    fn from(value: DeliveryEventError) -> Self {
        Self::Event(value)
    }
}

impl From<DeliveryError> for DeliveryProcessError {
    fn from(value: DeliveryError) -> Self {
        Self::Delivery(value)
    }
}

impl From<DeliveryResult> for DeliveryResultDto {
    fn from(value: DeliveryResult) -> Self {
        let s3_outcome = match value.s3_outcome {
            application::repositories::ConditionalWriteOutcome::Committed => "committed",
            application::repositories::ConditionalWriteOutcome::Conflict => "conflict",
        };
        let drive_resource_id = value
            .drive_resource_id
            .as_ref()
            .map(|id| id.as_str().to_owned());
        let (telegram_outcome, outcome) = execution_outcome_labels(&value.telegram_outcome);
        Self {
            s3_outcome,
            drive_resource_id,
            telegram_outcome,
            outcome,
        }
    }
}

fn execution_outcome_labels(outcome: &ExecutionOutcome) -> (&'static str, &'static str) {
    match outcome {
        ExecutionOutcome::Accepted { .. } => ("accepted", "accepted"),
        ExecutionOutcome::TerminalFailure { .. } => ("terminal", "terminal"),
        ExecutionOutcome::ManualReview { .. } => ("ambiguous", "ambiguous"),
        ExecutionOutcome::Exhausted { .. } => ("exhausted", "exhausted"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic
    )]

    use super::*;
    use application::external_operation::ExternalResourceId;
    use application::ports::PresignedObjectLink;
    use application::repositories::ConditionalWriteOutcome;
    use domain::idempotency::{OperationKind, OperationTargetFingerprint};
    use domain::identity::{ChatId, MessageThreadId, WorkflowId};
    use domain::workflow::WorkflowRevision;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn valid_event() -> DeliveryEvent {
        let pdf_bytes = b"%PDF-1.4 handler test" as &[u8];
        use sha2::Digest;
        let hash: [u8; 32] = sha2::Sha256::digest(pdf_bytes).into();
        let workflow_id = WorkflowId::new("wf-handler-delivery").expect("wid");
        let storage_key =
            StorageKey::new(format!("artifacts/{}/obj-1", workflow_id.as_str())).expect("key");
        let target = OperationTargetFingerprint::new([3u8; 32]);
        let drive_key = IdempotencyKey::new(
            workflow_id.clone(),
            WorkflowRevision::new(8),
            OperationKind::GoogleWrite,
            target,
        );
        let telegram_key = IdempotencyKey::new(
            workflow_id.clone(),
            WorkflowRevision::new(8),
            OperationKind::TelegramSend,
            target,
        );
        DeliveryEvent {
            schema_version: EVENT_SCHEMA_VERSION.to_owned(),
            artifact: ArtifactDto {
                workflow_id: workflow_id.as_str().to_owned(),
                object_id: "obj-1".to_owned(),
                storage_key: storage_key.as_str().to_owned(),
                byte_length: pdf_bytes.len() as u64,
                sha256_hex: hex::encode(hash),
                media_type: "application/pdf".to_owned(),
                created_at: 1_700_000_000,
            },
            pdf_bytes_base64: BASE64.encode(pdf_bytes),
            drive_key,
            telegram_key,
            destination_name: "quote.pdf".to_owned(),
            topic: TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77).expect("tid")),
            retry_policy: RetryPolicyDto {
                max_attempts: 3,
                base_delay_ms: 500,
                maximum_delay_ms: 10_000,
                jitter_basis_points: 2_000,
            },
            topic_message: "Your quotation is ready: {link}".to_owned(),
        }
    }

    /// Fake runner that returns a programmatic result and counts calls.
    struct FakeRunner {
        result: Result<DeliveryResult, DeliveryError>,
        calls: Arc<AtomicU32>,
    }

    impl DeliveryRunner for FakeRunner {
        async fn run(&self, _request: DeliveryRequest) -> Result<DeliveryResult, DeliveryError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.result {
                Ok(r) => Ok(clone_result(r)),
                Err(e) => Err(clone_error(e)),
            }
        }
    }

    fn clone_result(r: &DeliveryResult) -> DeliveryResult {
        let link = PresignedObjectLink::new("https://s3.example.com/presigned/x").expect("link");
        DeliveryResult {
            s3_outcome: r.s3_outcome,
            drive_resource_id: r.drive_resource_id.clone(),
            presigned_link: link,
            telegram_outcome: clone_outcome(&r.telegram_outcome),
        }
    }

    fn clone_outcome(o: &ExecutionOutcome) -> ExecutionOutcome {
        match o {
            ExecutionOutcome::Accepted {
                attempt,
                resource_id,
            } => ExecutionOutcome::Accepted {
                attempt: *attempt,
                resource_id: resource_id.clone(),
            },
            ExecutionOutcome::TerminalFailure { attempt, failure } => {
                ExecutionOutcome::TerminalFailure {
                    attempt: *attempt,
                    failure: failure.clone(),
                }
            }
            ExecutionOutcome::ManualReview {
                attempt,
                failure,
                completed_attempts,
            } => ExecutionOutcome::ManualReview {
                attempt: *attempt,
                failure: failure.clone(),
                completed_attempts: completed_attempts.clone(),
            },
            ExecutionOutcome::Exhausted { completed_attempts } => ExecutionOutcome::Exhausted {
                completed_attempts: completed_attempts.clone(),
            },
        }
    }

    fn clone_error(e: &DeliveryError) -> DeliveryError {
        e.clone()
    }

    fn accepted_result() -> DeliveryResult {
        DeliveryResult {
            s3_outcome: ConditionalWriteOutcome::Committed,
            drive_resource_id: Some(ExternalResourceId::new("drive-abc").expect("id")),
            presigned_link: PresignedObjectLink::new("https://s3.example.com/presigned/x")
                .expect("link"),
            telegram_outcome: ExecutionOutcome::Accepted {
                attempt: domain::retry::AttemptNumber::new(1).expect("a"),
                resource_id: None,
            },
        }
    }

    #[tokio::test]
    async fn success_path_returns_accepted_dto() {
        let event = valid_event();
        let calls = Arc::new(AtomicU32::new(0));
        let runner = FakeRunner {
            result: Ok(accepted_result()),
            calls: Arc::clone(&calls),
        };
        let dto = process_delivery(event, &runner).await.expect("ok");
        assert_eq!(dto.s3_outcome, "committed");
        assert_eq!(dto.drive_resource_id.as_deref(), Some("drive-abc"));
        assert_eq!(dto.telegram_outcome, "accepted");
        assert_eq!(dto.outcome, "accepted");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retryable_drive_failure_maps_to_delivery_error() {
        let event = valid_event();
        let runner = FakeRunner {
            result: Err(DeliveryError::Drive(
                application::artifact_delivery::DriveDeliveryError::Exhausted,
            )),
            calls: Arc::new(AtomicU32::new(0)),
        };
        let err = process_delivery(event, &runner)
            .await
            .expect_err("drive exhausted");
        assert!(matches!(
            err,
            DeliveryProcessError::Delivery(DeliveryError::Drive(
                application::artifact_delivery::DriveDeliveryError::Exhausted
            ))
        ));
    }

    #[tokio::test]
    async fn terminal_telegram_failure_maps_to_delivery_error() {
        let event = valid_event();
        let runner = FakeRunner {
            result: Err(DeliveryError::Telegram(
                application::artifact_delivery::TelegramDeliveryError::Terminal,
            )),
            calls: Arc::new(AtomicU32::new(0)),
        };
        let err = process_delivery(event, &runner)
            .await
            .expect_err("telegram terminal");
        assert!(matches!(
            err,
            DeliveryProcessError::Delivery(DeliveryError::Telegram(
                application::artifact_delivery::TelegramDeliveryError::Terminal
            ))
        ));
    }

    #[tokio::test]
    async fn schema_version_mismatch_rejected_before_runner() {
        let mut event = valid_event();
        event.schema_version = "novus.workflow-actions.v0".to_owned();
        let runner = FakeRunner {
            result: Ok(accepted_result()),
            calls: Arc::new(AtomicU32::new(0)),
        };
        let err = process_delivery(event, &runner)
            .await
            .expect_err("mismatch");
        assert!(matches!(
            err,
            DeliveryProcessError::Event(DeliveryEventError::SchemaVersionMismatch)
        ));
        assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn invalid_base64_length_rejected() {
        let mut event = valid_event();
        event.pdf_bytes_base64 = BASE64.encode(b"wrong length");
        let runner = FakeRunner {
            result: Ok(accepted_result()),
            calls: Arc::new(AtomicU32::new(0)),
        };
        let err = process_delivery(event, &runner)
            .await
            .expect_err("bad pdf bytes");
        assert!(matches!(
            err,
            DeliveryProcessError::Event(DeliveryEventError::InvalidBase64)
        ));
    }

    #[tokio::test]
    async fn invalid_retry_policy_rejected() {
        let mut event = valid_event();
        event.retry_policy.max_attempts = 0;
        let runner = FakeRunner {
            result: Ok(accepted_result()),
            calls: Arc::new(AtomicU32::new(0)),
        };
        let err = process_delivery(event, &runner)
            .await
            .expect_err("bad policy");
        assert!(matches!(
            err,
            DeliveryProcessError::Event(DeliveryEventError::InvalidRetryPolicy)
        ));
    }
}
