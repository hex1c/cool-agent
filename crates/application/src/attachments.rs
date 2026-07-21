#![deny(unsafe_code)]

//! Attachment collection service.
//!
//! Validates incoming Telegram attachments against [`AttachmentConfig`],
//! durably stores raw bytes in the object store before normalization,
//! and records immutable metadata with content-based deduplication so
//! duplicate/concurrent uploads produce exactly one stored object.

use std::fmt::{Display, Formatter};

use domain::attachment::{AttachmentDescriptor, AttachmentError};
use domain::identity::{MessageId, ParticipantId, WorkflowId};
use domain::{
    TransitionError, TransitionOutcome, TransitionRequest, WaitDeadline, Workflow, WorkflowState,
    WorkflowTimestamp, WorkflowTransition,
};
use sha2::{Digest, Sha256};

use crate::config::AttachmentConfig;
use crate::ports::{
    ObjectClass, ObjectStore, PortValueError, StorageKey, StorageRecordId, StoredObject,
};
use crate::repositories::{ConditionalWriteOutcome, ObjectMetadataRepository, PageRequest};

/// Errors produced by the attachment collection service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentCollectionError {
    /// The workflow is not in [`WorkflowState::CollectingAttachments`].
    NotCollectingAttachments,
    /// The total attachment count for this workflow has reached the configured maximum.
    CountExceeded { max: u8, current: u8 },
    /// The raw byte size of a single attachment exceeds the configured maximum.
    SizeExceeded { max_bytes: u64, actual: u64 },
    /// The attachment's media type is not in the configured allow-list.
    DisallowedMimeType { media_type: String },
    /// Port-level validation failed while constructing a storage value.
    PortValue(PortValueError),
    /// The attachment descriptor is invalid (e.g. unsupported media type).
    InvalidAttachment(AttachmentError),
    /// The object store rejected the put.
    Store,
    /// The metadata repository rejected the record.
    MetadataStore,
    /// The transition was rejected by the workflow state machine.
    WorkflowTransition(TransitionError),
}

impl Display for AttachmentCollectionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCollectingAttachments => {
                formatter.write_str("workflow is not in attachment collection state")
            }
            Self::CountExceeded { max, current } => {
                write!(
                    formatter,
                    "attachment count exceeded: {current} already collected (max {max})"
                )
            }
            Self::SizeExceeded { max_bytes, actual } => {
                write!(
                    formatter,
                    "attachment size {actual} bytes exceeds {max_bytes} byte maximum"
                )
            }
            Self::DisallowedMimeType { media_type } => {
                write!(
                    formatter,
                    "attachment media type {media_type:?} is not allowed"
                )
            }
            Self::PortValue(error) => write!(formatter, "port value error: {error}"),
            Self::InvalidAttachment(error) => write!(formatter, "invalid attachment: {error}"),
            Self::Store => formatter.write_str("object store error"),
            Self::MetadataStore => formatter.write_str("metadata store error"),
            Self::WorkflowTransition(error) => write!(formatter, "workflow transition: {error}"),
        }
    }
}

impl std::error::Error for AttachmentCollectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PortValue(error) => Some(error),
            Self::InvalidAttachment(error) => Some(error),
            Self::WorkflowTransition(error) => Some(error),
            _ => None,
        }
    }
}

impl From<PortValueError> for AttachmentCollectionError {
    fn from(value: PortValueError) -> Self {
        Self::PortValue(value)
    }
}

impl From<TransitionError> for AttachmentCollectionError {
    fn from(value: TransitionError) -> Self {
        Self::WorkflowTransition(value)
    }
}

/// The attachment collection service.
///
/// Owns the object store and metadata repository adapters along with
/// the deployment-level [`AttachmentConfig`]. All I/O is behind the
/// ports so the service remains testable with in-memory fakes.
pub struct AttachmentCollectionService<O, M> {
    object_store: O,
    metadata_repository: M,
    config: AttachmentConfig,
}

impl<O, M> AttachmentCollectionService<O, M>
where
    O: ObjectStore,
    M: ObjectMetadataRepository,
{
    /// Create a new service wired to the given adapters.
    pub fn new(object_store: O, metadata_repository: M, config: AttachmentConfig) -> Self {
        Self {
            object_store,
            metadata_repository,
            config,
        }
    }

    /// Collect one attachment.
    ///
    /// Validates the MIME type and byte size against the configured limits,
    /// checks that the workflow state is [`WorkflowState::CollectingAttachments`],
    /// computes a SHA-256 checksum, builds a stable storage key, writes the
    /// raw bytes to the object store, and conditionally records the metadata.
    ///
    /// Duplicate uploads (same source message + same content checksum) produce
    /// exactly one stored object: the [`record`][ObjectMetadataRepository::record]
    /// call returns [`ConditionalWriteOutcome::Conflict`] and the existing
    /// object is returned.
    pub async fn collect(
        &self,
        workflow: &Workflow,
        descriptor: &AttachmentDescriptor,
        bytes: &[u8],
        source_message: MessageId,
        timestamp: WorkflowTimestamp,
    ) -> Result<StoredObject, AttachmentCollectionError> {
        // State guard
        if !matches!(
            workflow.state(),
            WorkflowState::CollectingAttachments { .. }
        ) {
            return Err(AttachmentCollectionError::NotCollectingAttachments);
        }

        // MIME validation
        let media_type = descriptor.media_type().to_owned();
        if !self
            .config
            .allowed_mime_types
            .iter()
            .any(|allowed| allowed == &media_type)
        {
            return Err(AttachmentCollectionError::DisallowedMimeType { media_type });
        }

        // Size validation
        let byte_length = bytes.len() as u64;
        if byte_length > self.config.max_bytes {
            return Err(AttachmentCollectionError::SizeExceeded {
                max_bytes: self.config.max_bytes,
                actual: byte_length,
            });
        }

        // Count validation (best-effort; guard via existing metadata records)
        let current_count = self.current_attachment_count(workflow.id()).await?;
        if current_count >= self.config.max_count {
            return Err(AttachmentCollectionError::CountExceeded {
                max: self.config.max_count,
                current: current_count,
            });
        }

        // Checksum
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let checksum: [u8; 32] = hasher.finalize().into();

        // Stable object identity: {source_message}_{sha256_prefix}
        let checksum_prefix = hex::encode(&checksum[..8]);
        let object_id_value = format!("{}_{}", source_message.get(), checksum_prefix);
        let object_id = StorageRecordId::new(object_id_value)?;

        // Storage key
        let storage_key_value = format!("raw/{}/{}", workflow.id().as_str(), object_id.as_str());
        let storage_key = StorageKey::new(storage_key_value)?;

        let stored = StoredObject {
            workflow_id: workflow.id().clone(),
            object_id,
            class: ObjectClass::RawInput,
            storage_key,
            byte_length,
            sha256: checksum,
            media_type,
            created_at: timestamp,
        };
        stored.validate()?;

        // Idempotent put (same key → same content; S3 write-once)
        self.object_store
            .put(&stored, bytes)
            .await
            .map_err(|_| AttachmentCollectionError::Store)?;

        // Conditional metadata record for deduplication
        match self.metadata_repository.record(&stored).await {
            Ok(ConditionalWriteOutcome::Committed) | Ok(ConditionalWriteOutcome::Conflict) => {
                Ok(stored)
            }
            Err(_) => Err(AttachmentCollectionError::MetadataStore),
        }
    }

    /// Finish collection: transition from `CollectingAttachments` to
    /// `ExtractionStarted`. This corresponds to the `/done` command.
    pub fn finish(
        &self,
        workflow: &Workflow,
        actor: ParticipantId,
        source_message: MessageId,
        timestamp: WorkflowTimestamp,
    ) -> Result<TransitionOutcome, AttachmentCollectionError> {
        let request = TransitionRequest {
            transition: WorkflowTransition::FinishAttachmentCollection,
            expected_revision: workflow.revision(),
            actor,
            source_message,
            timestamp,
        };
        Ok(workflow.transition(request)?)
    }

    /// Handle collection timeout: transition from `CollectingAttachments` to
    /// `WaitingForClarification` with `resume: AttachmentCollection`.
    pub fn handle_timeout(
        &self,
        workflow: &Workflow,
        actor: ParticipantId,
        source_message: MessageId,
        timestamp: WorkflowTimestamp,
        clarification_deadline: WaitDeadline,
    ) -> Result<TransitionOutcome, AttachmentCollectionError> {
        let request = TransitionRequest {
            transition: WorkflowTransition::AttachmentCollectionTimedOut {
                clarification_deadline,
            },
            expected_revision: workflow.revision(),
            actor,
            source_message,
            timestamp,
        };
        Ok(workflow.transition(request)?)
    }

    /// Query the number of objects already recorded for the given workflow.
    async fn current_attachment_count(
        &self,
        workflow_id: &WorkflowId,
    ) -> Result<u8, AttachmentCollectionError> {
        let request = PageRequest::new(u16::from(self.config.max_count) + 1, None)
            .map_err(|_| AttachmentCollectionError::MetadataStore)?;
        let page = self
            .metadata_repository
            .page(workflow_id, &request)
            .await
            .map_err(|_| AttachmentCollectionError::MetadataStore)?;
        Ok(page.items.len() as u8)
    }
}
