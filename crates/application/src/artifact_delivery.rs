#![deny(unsafe_code)]

//! Artifact delivery orchestrator: S3 publication → presigned link → Drive copy → Telegram send.
//!
//! Every external operation has a stable [`IdempotencyKey`], so re-running
//! [`ArtifactDeliveryService::deliver`] from the top after a partial failure
//! is safe: S3 is write-once (re-confirm + conditional metadata), Drive and
//! Telegram executors replay the durable journal (Final → returned without
//! re-invoking the provider).
//!
//! Port traits are defined here so the `application` crate does not depend on
//! `pdf`, `google`, or `telegram`. Concrete adapters implement the ports.

use std::fmt::{Display, Formatter};

use domain::idempotency::IdempotencyKey;
use domain::identity::TopicSessionId;
use domain::retry::RetryPolicy;

use crate::external_operation::{
    BackoffWait, ExecutionOutcome, ExternalOperationExecutor, ExternalResourceId, JitterSource,
    OperationJournal, ProviderOutcome,
};
use crate::ports::HistoryStore;
use crate::ports::{ArtifactLinkSigner, ObjectStore, PresignedObjectLink, StoredObject};
use crate::publication::{PublicationCoordinator, PublicationError};
use crate::repositories::{ConditionalWriteOutcome, HistoryRepository, ObjectMetadataRepository};

// ── Port traits ───────────────────────────────────────────────────────

/// Single-attempt Google Drive file copy. The shared executor handles retry.
/// Implementations classify their own outcome as retryable, terminal, or
/// ambiguous and MUST be free of retry/backoff logic.
#[allow(async_fn_in_trait)]
pub trait DriveCopyPort {
    /// Copy the canonical S3/PDF artifact into the owner's Drive. Returns a
    /// [`ProviderOutcome`] so the executor can replay final states without
    /// re-invoking the provider. Must be idempotent: copying the same source
    /// to the same destination twice must not create a duplicate.
    async fn copy_artifact(
        &self,
        artifact: &StoredObject,
        destination_name: &str,
    ) -> ProviderOutcome;
}

/// Single-attempt topic message send. The shared executor handles retry.
/// Implementations classify their own outcome and MUST reject OAuth-bearing
/// content (defense in depth; the application layer also rejects before
/// calling).
#[allow(async_fn_in_trait)]
pub trait TopicDeliveryPort {
    /// Send a topic-safe message (text + presigned link) to the originating
    /// forum topic's `message_thread_id`.
    async fn send_topic(&self, topic: TopicSessionId, message: &str) -> ProviderOutcome;
}

// ── Request / response ────────────────────────────────────────────────

/// All inputs needed for one artifact delivery attempt.
pub struct DeliveryRequest {
    /// The validated artifact descriptor (class = Artifact).
    pub artifact: StoredObject,
    /// Rendered PDF bytes.
    pub pdf_bytes: Vec<u8>,
    /// Idempotency key for the Drive copy (GoogleWrite).
    pub drive_key: IdempotencyKey,
    /// Idempotency key for the Telegram send (TelegramSend).
    pub telegram_key: IdempotencyKey,
    /// Google Drive destination file name.
    pub destination_name: String,
    /// Originating forum topic.
    pub topic: TopicSessionId,
    /// Retry policy shared by Drive and Telegram operations.
    pub retry_policy: RetryPolicy,
    /// Topic message template. `{link}` is replaced with the presigned URL.
    pub topic_message: String,
}

/// Result of a successful delivery.
#[derive(Debug)]
pub struct DeliveryResult {
    /// S3 publication outcome (Committed or Conflict).
    pub s3_outcome: ConditionalWriteOutcome,
    /// Drive resource id produced by the accepted copy.
    pub drive_resource_id: Option<ExternalResourceId>,
    /// Presigned retrieval link for the artifact.
    pub presigned_link: PresignedObjectLink,
    /// Telegram delivery final outcome (should be Accepted).
    pub telegram_outcome: ExecutionOutcome,
}

// ── Errors ────────────────────────────────────────────────────────────

/// Stable delivery error — no provider details exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryError {
    /// S3 object could not be confirmed durable.
    S3NotConfirmed,
    /// S3 metadata write failed.
    S3Metadata,
    /// Drive copy did not reach an accepted outcome.
    Drive(DriveDeliveryError),
    /// Telegram send did not reach an accepted outcome.
    Telegram(TelegramDeliveryError),
    /// Presigned-link generation failed.
    LinkSigning,
    /// Executor persistence or wait layer failed.
    Execution { operation: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveDeliveryError {
    /// All retries exhausted; no accepted outcome.
    Exhausted,
    /// Terminal provider failure.
    Terminal,
    /// Ambiguous outcome; manual review required.
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramDeliveryError {
    Exhausted,
    Terminal,
    Ambiguous,
}

impl Display for DeliveryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::S3NotConfirmed => f.write_str("S3 object not confirmed durable"),
            Self::S3Metadata => f.write_str("S3 metadata write failed"),
            Self::Drive(d) => write!(f, "drive copy failed: {d}"),
            Self::Telegram(t) => write!(f, "telegram send failed: {t}"),
            Self::LinkSigning => f.write_str("presigned-link generation failed"),
            Self::Execution { operation } => {
                write!(f, "executor layer failed during {operation}")
            }
        }
    }
}

impl std::error::Error for DeliveryError {}

impl From<DriveDeliveryError> for DeliveryError {
    fn from(value: DriveDeliveryError) -> Self {
        Self::Drive(value)
    }
}

impl From<TelegramDeliveryError> for DeliveryError {
    fn from(value: TelegramDeliveryError) -> Self {
        Self::Telegram(value)
    }
}

impl Display for DriveDeliveryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exhausted => f.write_str("retries exhausted"),
            Self::Terminal => f.write_str("terminal failure"),
            Self::Ambiguous => f.write_str("ambiguous outcome"),
        }
    }
}

impl Display for TelegramDeliveryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exhausted => f.write_str("retries exhausted"),
            Self::Terminal => f.write_str("terminal failure"),
            Self::Ambiguous => f.write_str("ambiguous outcome"),
        }
    }
}

fn map_execution_outcome_to_drive(
    outcome: ExecutionOutcome,
) -> Result<Option<ExternalResourceId>, DriveDeliveryError> {
    match outcome {
        ExecutionOutcome::Accepted { resource_id, .. } => Ok(resource_id),
        ExecutionOutcome::TerminalFailure { .. } => Err(DriveDeliveryError::Terminal),
        ExecutionOutcome::ManualReview { .. } => Err(DriveDeliveryError::Ambiguous),
        ExecutionOutcome::Exhausted { .. } => Err(DriveDeliveryError::Exhausted),
    }
}

fn map_execution_outcome_to_telegram(
    outcome: ExecutionOutcome,
) -> Result<ExecutionOutcome, TelegramDeliveryError> {
    match &outcome {
        ExecutionOutcome::Accepted { .. } => Ok(outcome),
        ExecutionOutcome::TerminalFailure { .. } => Err(TelegramDeliveryError::Terminal),
        ExecutionOutcome::ManualReview { .. } => Err(TelegramDeliveryError::Ambiguous),
        ExecutionOutcome::Exhausted { .. } => Err(TelegramDeliveryError::Exhausted),
    }
}

// ── Service ───────────────────────────────────────────────────────────

/// Orchestrates artifact delivery: S3 publish → presign → Drive copy → Telegram.
///
/// A single shared [`ExternalOperationExecutor`] is used for both Drive and
/// Telegram operations because the journal is keyed by [`IdempotencyKey`].
pub struct ArtifactDeliveryService<O, HS, M, H, J, Ji, W, D, T, S> {
    publication: PublicationCoordinator<O, HS, M, H>,
    executor: ExternalOperationExecutor<J, Ji, W>,
    drive_port: D,
    telegram_port: T,
    link_signer: S,
}

impl<O, HS, M, H, J, Ji, W, D, T, S> ArtifactDeliveryService<O, HS, M, H, J, Ji, W, D, T, S> {
    pub const fn new(
        publication: PublicationCoordinator<O, HS, M, H>,
        executor: ExternalOperationExecutor<J, Ji, W>,
        drive_port: D,
        telegram_port: T,
        link_signer: S,
    ) -> Self {
        Self {
            publication,
            executor,
            drive_port,
            telegram_port,
            link_signer,
        }
    }
}

impl<O, HS, M, H, J, Ji, W, D, T, S> ArtifactDeliveryService<O, HS, M, H, J, Ji, W, D, T, S>
where
    O: ObjectStore,
    HS: HistoryStore,
    M: ObjectMetadataRepository,
    H: HistoryRepository,
    J: OperationJournal,
    Ji: JitterSource,
    W: BackoffWait,
    D: DriveCopyPort,
    T: TopicDeliveryPort,
    S: ArtifactLinkSigner,
    J::Error: Display,
    W::Error: Display,
{
    /// Execute the full delivery pipeline.
    ///
    /// # Idempotency
    ///
    /// Re-running after a partial failure is safe:
    /// - S3 is write-once: the publication coordinator re-confirms the object
    ///   on ambiguous puts and conflicts on duplicate metadata.
    /// - Drive and Telegram executors replay the durable journal: Final
    ///   states are returned without re-invoking the provider.
    pub async fn deliver(&self, request: DeliveryRequest) -> Result<DeliveryResult, DeliveryError> {
        // 1. Publish artifact to S3.
        let s3_outcome = self
            .publication
            .publish_object(&request.artifact, &request.pdf_bytes)
            .await
            .map_err(|err| match err {
                PublicationError::ObjectNotConfirmed { .. } => DeliveryError::S3NotConfirmed,
                PublicationError::MetadataWrite { .. } => DeliveryError::S3Metadata,
                _ => DeliveryError::S3NotConfirmed,
            })?;

        // 2. Generate presigned link.
        let presigned_link = self
            .link_signer
            .presign_artifact(&request.artifact)
            .await
            .map_err(|_| DeliveryError::LinkSigning)?;

        // 3. Compose final topic message.
        let message = request
            .topic_message
            .replace("{link}", presigned_link.as_str());

        // 4. Drive copy via executor.
        let drive_outcome = self
            .executor
            .execute(&request.drive_key, request.retry_policy, || {
                self.drive_port
                    .copy_artifact(&request.artifact, &request.destination_name)
            })
            .await
            .map_err(|_err| DeliveryError::Execution { operation: "drive" })?;
        let drive_resource_id = map_execution_outcome_to_drive(drive_outcome)?;

        // 5. Telegram send via executor.
        let telegram_outcome = self
            .executor
            .execute(&request.telegram_key, request.retry_policy, || {
                self.telegram_port.send_topic(request.topic, &message)
            })
            .await
            .map_err(|_err| DeliveryError::Execution {
                operation: "telegram",
            })?;
        let telegram_outcome = map_execution_outcome_to_telegram(telegram_outcome)?;

        Ok(DeliveryResult {
            s3_outcome,
            drive_resource_id,
            presigned_link,
            telegram_outcome,
        })
    }
}
