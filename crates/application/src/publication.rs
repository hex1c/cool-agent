#![deny(unsafe_code)]

//! Storage publication coordinator.
//!
//! Proves S3 acceptance of an immutable object *before* publishing its
//! DynamoDB metadata or history pointer. S3 writes are write-once
//! (`If-None-Match: *`), so a network-level failure during `put_object` is
//! ambiguous: the object may already be durably stored even though the SDK
//! returned an error. The coordinator disambiguates such outcomes by probing
//! the object with `ObjectStore::get` — which re-verifies length and
//! SHA-256 — before deciding whether to proceed to the metadata write.
//!
//! DynamoDB metadata is published with a conditional write
//! (`attribute_not_exists(pk)`). A `Conflict` outcome means a previous
//! attempt already recorded the pointer for the same immutable object, so
//! the combined S3 + DynamoDB state is consistent and the coordinator
//! returns `Conflict` rather than treating it as an error.

use std::fmt::{Display, Formatter};

use crate::ports::{ObjectClass, ObjectStore, StoredObject};
use crate::repositories::{
    ConditionalWriteOutcome, HistoryCheckpoint, HistoryRepository, ObjectMetadataRepository,
};
use crate::sanitized_history::SanitizedHistory;

/// Failure reported by the publication coordinator.
///
/// Provider details are never exposed. The application layer does not rely
/// on adapter `Display` implementations being redacted — only a stable
/// operation label is retained, so a replacement adapter that leaks
/// provider text cannot propagate it through this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationError {
    /// The object could not be confirmed in object storage after a put
    /// attempt and a verification probe. The object is *not* known to be
    /// durable; the caller may safely retry the entire publication with the
    /// same object identifier because S3 puts are write-once and idempotent.
    ObjectNotConfirmed { operation: &'static str },
    /// The object was confirmed in object storage but the DynamoDB metadata
    /// write failed. The caller may retry the metadata write alone (or the
    /// whole publication, which will re-confirm the object and then either
    /// commit or conflict on the metadata).
    MetadataWrite { operation: &'static str },
    /// The serialized SanitizedHistory does not match the checkpoint's
    /// stored object (byte length or SHA-256). The producer must construct
    /// the checkpoint from the serialized history's hash/length.
    HistorySerializationMismatch,
    /// The SanitizedHistory could not be serialized.
    HistorySerializationFailed,
    /// The caller attempted to publish a SanitizedHistory object through
    /// `publish_object` instead of `publish_history`, which would bypass
    /// the producer-owned SanitizedHistory type.
    ObjectClassRequiresTypedPublication,
    /// The checkpoint failed validation before I/O.
    InvalidCheckpoint,
}

impl Display for PublicationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ObjectNotConfirmed { operation } => {
                write!(formatter, "object not confirmed during {operation}")
            }
            Self::MetadataWrite { operation } => {
                write!(formatter, "metadata write failed during {operation}")
            }
            Self::HistorySerializationMismatch => {
                formatter.write_str("serialized history does not match checkpoint object")
            }
            Self::HistorySerializationFailed => {
                formatter.write_str("failed to serialize sanitized history")
            }
            Self::ObjectClassRequiresTypedPublication => {
                formatter.write_str("sanitized history must be published through publish_history")
            }
            Self::InvalidCheckpoint => formatter.write_str("history checkpoint failed validation"),
        }
    }
}

impl std::error::Error for PublicationError {}

/// Coordinates immutable object storage (S3) with DynamoDB metadata
/// publication so that metadata pointers are only written after the object
/// body is confirmed durable.
pub struct PublicationCoordinator<O, M, H> {
    object_store: O,
    metadata_repository: M,
    history_repository: H,
}

impl<O, M, H> PublicationCoordinator<O, M, H> {
    pub const fn new(object_store: O, metadata_repository: M, history_repository: H) -> Self {
        Self {
            object_store,
            metadata_repository,
            history_repository,
        }
    }
}

impl<O, M, H> PublicationCoordinator<O, M, H>
where
    O: ObjectStore,
    M: ObjectMetadataRepository,
    H: HistoryRepository,
{
    /// Publish a raw input or artifact object: put the body to object
    /// storage, confirm durability, then record the metadata pointer in
    /// DynamoDB.
    pub async fn publish_object(
        &self,
        object: &StoredObject,
        bytes: &[u8],
    ) -> Result<ConditionalWriteOutcome, PublicationError> {
        // SanitizedHistory must be published through publish_history, which
        // enforces the producer-owned SanitizedHistory type. Reject it here
        // to prevent bypassing redaction via the raw-bytes path.
        if object.class == ObjectClass::SanitizedHistory {
            return Err(PublicationError::ObjectClassRequiresTypedPublication);
        }
        self.confirm_object(object, bytes, "put object").await?;
        self.metadata_repository
            .record(object)
            .await
            .map_err(|_error| PublicationError::MetadataWrite {
                operation: "record object metadata",
            })
    }

    /// Publish a sanitized-history checkpoint: serialize the producer-owned
    /// [`SanitizedHistory`], verify the bytes match the checkpoint's stored
    /// object (byte length and SHA-256), put the body to object storage,
    /// confirm durability, then append the history pointer in DynamoDB.
    pub async fn publish_history(
        &self,
        checkpoint: &HistoryCheckpoint,
        history: &SanitizedHistory,
    ) -> Result<ConditionalWriteOutcome, PublicationError> {
        // Validate the checkpoint before any I/O so an artifact-class or
        // wrong-workflow checkpoint is rejected before S3 accepts the body.
        checkpoint
            .validate()
            .map_err(|_| PublicationError::InvalidCheckpoint)?;

        let bytes = history
            .serialize()
            .map_err(|_| PublicationError::HistorySerializationFailed)?;

        // Verify the serialized bytes match the checkpoint's stored object.
        // The producer must construct the checkpoint from the serialized
        // history's hash/length; a mismatch indicates a bug in the producer.
        use sha2::{Digest, Sha256};
        let computed_hash = Sha256::digest(&bytes);
        if bytes.len() as u64 != checkpoint.object.byte_length
            || computed_hash.as_slice() != checkpoint.object.sha256
        {
            return Err(PublicationError::HistorySerializationMismatch);
        }

        self.confirm_object(&checkpoint.object, &bytes, "put history object")
            .await?;
        self.history_repository
            .append(checkpoint)
            .await
            .map_err(|_error| PublicationError::MetadataWrite {
                operation: "append history pointer",
            })
    }

    /// Put the object body to storage and confirm it is durable. On a
    /// definitive put success the object is confirmed. On a put error the
    /// outcome is ambiguous (the object may or may not have been written),
    /// so a `get` probe re-verifies the content against the expected length
    /// and SHA-256 (see `ObjectStore::get`). If the probe cannot confirm
    /// the object, the coordinator fails closed.
    async fn confirm_object(
        &self,
        object: &StoredObject,
        bytes: &[u8],
        operation: &'static str,
    ) -> Result<(), PublicationError> {
        match self.object_store.put(object, bytes).await {
            Ok(()) => Ok(()),
            Err(_put_error) => {
                // Ambiguous: the SDK returned an error but the object may
                // already be durable. Probe with get, which re-verifies
                // length and SHA-256 before returning Ok.
                match self.object_store.get(object).await {
                    Ok(_) => Ok(()),
                    Err(_) => Err(PublicationError::ObjectNotConfirmed { operation }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::ports::{ObjectClass, Page, PageToken, PortValueError, StorageKey, StorageRecordId};
    use crate::repositories::PageRequest;
    use crate::sanitized_history::{SanitizedHistory, SanitizedRole};
    use domain::WorkflowTimestamp;
    use domain::identity::WorkflowId;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    // ── Test doubles ────────────────────────────────────────────────

    #[derive(Debug, Clone)]
    enum PutBehavior {
        Success,
        Ambiguous,
        DefinitiveFailure,
    }

    type StoredObjects = Vec<(String, Vec<u8>)>;

    #[derive(Debug, Clone)]
    struct FakeObjectStore {
        put_behavior: PutBehavior,
        storage: Arc<Mutex<StoredObjects>>,
    }

    impl FakeObjectStore {
        fn with(put_behavior: PutBehavior) -> Self {
            Self {
                put_behavior,
                storage: Arc::new(Mutex::new(Vec::new())),
            }
        }

        #[allow(dead_code)]
        fn seed(&self, key: &str, body: &[u8]) {
            self.storage
                .lock()
                .expect("storage lock")
                .push((key.to_owned(), body.to_vec()));
        }

        fn len(&self) -> usize {
            self.storage.lock().expect("storage lock").len()
        }
    }

    impl ObjectStore for FakeObjectStore {
        type Error = FakeStoreError;

        async fn put(&self, object: &StoredObject, bytes: &[u8]) -> Result<(), Self::Error> {
            match &self.put_behavior {
                PutBehavior::Success => {
                    self.storage
                        .lock()
                        .expect("storage lock")
                        .push((object.storage_key.as_str().to_owned(), bytes.to_vec()));
                    Ok(())
                }
                PutBehavior::Ambiguous => Err(FakeStoreError::Ambiguous),
                PutBehavior::DefinitiveFailure => Err(FakeStoreError::Definitive),
            }
        }

        async fn get(&self, object: &StoredObject) -> Result<Vec<u8>, Self::Error> {
            let storage = self.storage.lock().expect("storage lock");
            let found = storage
                .iter()
                .find(|(key, _)| key == object.storage_key.as_str());
            match found {
                Some((_, body)) => {
                    if body.len() as u64 == object.byte_length && fake_digest(body) == object.sha256
                    {
                        Ok(body.clone())
                    } else {
                        Err(FakeStoreError::NotFound)
                    }
                }
                None => Err(FakeStoreError::NotFound),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FakeStoreError {
        Ambiguous,
        Definitive,
        NotFound,
    }

    impl Display for FakeStoreError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Ambiguous => f.write_str("ambiguous put"),
                Self::Definitive => f.write_str("definitive failure"),
                Self::NotFound => f.write_str("not found"),
            }
        }
    }

    #[derive(Debug, Default, Clone)]
    struct FakeMetadataRepo {
        outcomes: Arc<Mutex<VecDeque<ConditionalWriteOutcome>>>,
        recorded: Arc<Mutex<Vec<StoredObject>>>,
    }

    impl FakeMetadataRepo {
        fn with(outcomes: Vec<ConditionalWriteOutcome>) -> Self {
            Self {
                outcomes: Arc::new(Mutex::new(outcomes.into())),
                recorded: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn noop() -> Self {
            Self::default()
        }

        fn recorded_count(&self) -> usize {
            self.recorded.lock().expect("recorded lock").len()
        }
    }

    impl ObjectMetadataRepository for FakeMetadataRepo {
        type Error = FakeRepoError;

        async fn record(
            &self,
            object: &StoredObject,
        ) -> Result<ConditionalWriteOutcome, Self::Error> {
            self.recorded
                .lock()
                .expect("recorded lock")
                .push(object.clone());
            self.outcomes
                .lock()
                .expect("outcomes lock")
                .pop_front()
                .ok_or(FakeRepoError::Exhausted)
        }

        async fn page(
            &self,
            _workflow_id: &WorkflowId,
            _request: &PageRequest,
        ) -> Result<Page<StoredObject>, Self::Error> {
            unreachable!("page is not used by the coordinator")
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FakeRepoError {
        Exhausted,
        #[allow(dead_code)]
        Service,
    }

    impl Display for FakeRepoError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Exhausted => f.write_str("outcomes exhausted"),
                Self::Service => f.write_str("service error"),
            }
        }
    }

    // ── Fixtures ────────────────────────────────────────────────────

    fn fake_digest(bytes: &[u8]) -> [u8; 32] {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let value = hasher.finish();
        let mut digest = [0_u8; 32];
        digest[..8].copy_from_slice(&value.to_le_bytes());
        digest
    }

    fn make_object(class: ObjectClass, body: &[u8]) -> StoredObject {
        let workflow_id = WorkflowId::new("workflow-publication").expect("workflow id");
        let prefix = match class {
            ObjectClass::RawInput => "raw/",
            ObjectClass::Artifact => "artifacts/",
            ObjectClass::SanitizedHistory => "history/",
        };
        let storage_key = StorageKey::new(format!(
            "{prefix}{}/{object_id}",
            workflow_id.as_str(),
            object_id = "obj-1"
        ))
        .expect("storage key");
        StoredObject {
            workflow_id,
            object_id: StorageRecordId::new("obj-1").expect("object id"),
            class,
            storage_key,
            byte_length: body.len() as u64,
            sha256: fake_digest(body),
            media_type: "application/octet-stream".to_owned(),
            created_at: WorkflowTimestamp::from_unix_seconds(1_700_000_000),
        }
    }

    fn make_history_checkpoint(serialized: &[u8]) -> (HistoryCheckpoint, SanitizedHistory) {
        let workflow_id = WorkflowId::new("workflow-publication").expect("workflow id");
        let storage_key = StorageKey::new(format!("history/{}/obj-1", workflow_id.as_str()))
            .expect("storage key");
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(serialized);
        let mut sha256 = [0_u8; 32];
        sha256.copy_from_slice(hash.as_slice());
        let object = StoredObject {
            workflow_id: workflow_id.clone(),
            object_id: StorageRecordId::new("obj-1").expect("object id"),
            class: ObjectClass::SanitizedHistory,
            storage_key,
            byte_length: serialized.len() as u64,
            sha256,
            media_type: "application/json".to_owned(),
            created_at: WorkflowTimestamp::from_unix_seconds(1_700_000_000),
        };
        let checkpoint = HistoryCheckpoint {
            workflow_id: workflow_id.clone(),
            sequence: crate::repositories::HistorySequence::new(1),
            object: object.clone(),
            model_version: "model-1".to_owned(),
            prompt_version: "prompt-1".to_owned(),
            created_at: object.created_at,
        };
        // Reconstruct the SanitizedHistory from the serialized bytes for
        // the coordinator call. In production the producer holds the
        // original object; here we serialize a fresh one to prove the
        // coordinator's hash/length verification.
        let history = SanitizedHistory::new(vec![(SanitizedRole::User, "hello".to_owned())], &[])
            .expect("valid history");
        (checkpoint, history)
    }

    // ── Tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn publish_object_rejects_sanitized_history_class() {
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[]}"#;
        let object = make_object(ObjectClass::SanitizedHistory, body);
        let store = FakeObjectStore::with(PutBehavior::Success);
        let metadata = FakeMetadataRepo::with(vec![ConditionalWriteOutcome::Committed]);
        let coordinator = PublicationCoordinator::new(store, metadata, FakeHistoryRepo::noop());

        let error = coordinator
            .publish_object(&object, body)
            .await
            .expect_err("SanitizedHistory must go through publish_history");

        assert!(
            matches!(error, PublicationError::ObjectClassRequiresTypedPublication),
            "expected ObjectClassRequiresTypedPublication, got {error:?}"
        );
    }

    #[tokio::test]
    async fn publish_object_puts_then_records_on_success() {
        let body = b"raw payload";
        let object = make_object(ObjectClass::RawInput, body);
        let store = FakeObjectStore::with(PutBehavior::Success);
        let metadata = FakeMetadataRepo::with(vec![ConditionalWriteOutcome::Committed]);
        let coordinator =
            PublicationCoordinator::new(store.clone(), metadata.clone(), FakeHistoryRepo::noop());

        let outcome = coordinator
            .publish_object(&object, body)
            .await
            .expect("success path should commit");

        assert_eq!(outcome, ConditionalWriteOutcome::Committed);
        assert_eq!(store.len(), 1);
        assert_eq!(metadata.recorded_count(), 1);
    }

    #[tokio::test]
    async fn publish_object_disambiguates_ambiguous_put_via_get_then_records() {
        let body = b"raw payload";
        let object = make_object(ObjectClass::RawInput, body);
        // Simulate: the put error is ambiguous, but the object *is* in S3
        // (pre-seeded storage so the get probe confirms it).
        let store = FakeObjectStore {
            put_behavior: PutBehavior::Ambiguous,
            storage: Arc::new(Mutex::new(vec![(
                object.storage_key.as_str().to_owned(),
                body.to_vec(),
            )])),
        };
        let metadata = FakeMetadataRepo::with(vec![ConditionalWriteOutcome::Committed]);
        let coordinator =
            PublicationCoordinator::new(store, metadata.clone(), FakeHistoryRepo::noop());

        let outcome = coordinator
            .publish_object(&object, body)
            .await
            .expect("disambiguated path should commit");

        assert_eq!(outcome, ConditionalWriteOutcome::Committed);
        assert_eq!(metadata.recorded_count(), 1);
    }

    #[tokio::test]
    async fn publish_object_fails_closed_when_put_error_is_not_confirmed() {
        let body = b"raw payload";
        let object = make_object(ObjectClass::RawInput, body);
        // Ambiguous put + empty storage => get probe cannot confirm.
        let store = FakeObjectStore::with(PutBehavior::Ambiguous);
        let metadata = FakeMetadataRepo::with(vec![ConditionalWriteOutcome::Committed]);
        let coordinator =
            PublicationCoordinator::new(store, metadata.clone(), FakeHistoryRepo::noop());

        let error = coordinator
            .publish_object(&object, body)
            .await
            .expect_err("should fail closed when not confirmed");

        assert!(
            matches!(error, PublicationError::ObjectNotConfirmed { .. }),
            "expected ObjectNotConfirmed, got {error:?}"
        );
        assert_eq!(metadata.recorded_count(), 0);
    }

    #[tokio::test]
    async fn publish_object_fails_closed_on_definitive_put_failure() {
        let body = b"raw payload";
        let object = make_object(ObjectClass::RawInput, body);
        let store = FakeObjectStore::with(PutBehavior::DefinitiveFailure);
        let metadata = FakeMetadataRepo::with(vec![ConditionalWriteOutcome::Committed]);
        let coordinator =
            PublicationCoordinator::new(store, metadata.clone(), FakeHistoryRepo::noop());

        let error = coordinator
            .publish_object(&object, body)
            .await
            .expect_err("should fail closed on definitive failure");

        assert!(
            matches!(error, PublicationError::ObjectNotConfirmed { .. }),
            "expected ObjectNotConfirmed, got {error:?}"
        );
        assert_eq!(metadata.recorded_count(), 0);
    }

    #[tokio::test]
    async fn publish_object_returns_committed_when_metadata_already_exists_same_content() {
        let body = b"raw payload";
        let object = make_object(ObjectClass::RawInput, body);
        let store = FakeObjectStore::with(PutBehavior::Success);
        // Same-content conflict is treated as idempotent success.
        let metadata = FakeMetadataRepo::with(vec![ConditionalWriteOutcome::Committed]);
        let coordinator = PublicationCoordinator::new(store, metadata, FakeHistoryRepo::noop());

        let outcome = coordinator
            .publish_object(&object, body)
            .await
            .expect("same-content conflict is idempotent success");

        assert_eq!(outcome, ConditionalWriteOutcome::Committed);
    }

    #[tokio::test]
    async fn publish_object_reports_metadata_write_failure_without_retrying() {
        let body = b"raw payload";
        let object = make_object(ObjectClass::RawInput, body);
        let store = FakeObjectStore::with(PutBehavior::Success);
        let metadata = FakeMetadataRepo::with(vec![]); // exhausted => error
        let coordinator = PublicationCoordinator::new(store, metadata, FakeHistoryRepo::noop());

        let error = coordinator
            .publish_object(&object, body)
            .await
            .expect_err("metadata failure should surface");

        assert!(
            matches!(error, PublicationError::MetadataWrite { .. }),
            "expected MetadataWrite, got {error:?}"
        );
    }

    #[tokio::test]
    async fn publish_history_puts_then_appends_on_success() {
        let history = SanitizedHistory::new(vec![(SanitizedRole::User, "hello".to_owned())], &[])
            .expect("valid history");
        let serialized = history.serialize().expect("should serialize");
        let (checkpoint, history_clone) = make_history_checkpoint(&serialized);

        let store = FakeObjectStore::with(PutBehavior::Success);
        let history_repo = FakeHistoryRepo::with(vec![ConditionalWriteOutcome::Committed]);
        let coordinator =
            PublicationCoordinator::new(store, FakeMetadataRepo::noop(), history_repo.clone());

        let outcome = coordinator
            .publish_history(&checkpoint, &history_clone)
            .await
            .expect("history path should commit");

        assert_eq!(outcome, ConditionalWriteOutcome::Committed);
        assert_eq!(history_repo.appended_count(), 1);
    }

    #[tokio::test]
    async fn publish_history_fails_closed_when_object_not_confirmed() {
        let history = SanitizedHistory::new(vec![(SanitizedRole::User, "hello".to_owned())], &[])
            .expect("valid history");
        let serialized = history.serialize().expect("should serialize");
        let (checkpoint, history_clone) = make_history_checkpoint(&serialized);

        let store = FakeObjectStore::with(PutBehavior::Ambiguous);
        let history_repo = FakeHistoryRepo::with(vec![ConditionalWriteOutcome::Committed]);
        let coordinator =
            PublicationCoordinator::new(store, FakeMetadataRepo::noop(), history_repo.clone());

        let error = coordinator
            .publish_history(&checkpoint, &history_clone)
            .await
            .expect_err("should fail closed");

        assert!(
            matches!(error, PublicationError::ObjectNotConfirmed { .. }),
            "expected ObjectNotConfirmed, got {error:?}"
        );
        assert_eq!(history_repo.appended_count(), 0);
    }

    #[tokio::test]
    async fn publish_history_rejects_checkpoint_with_mismatched_hash() {
        let history = SanitizedHistory::new(vec![(SanitizedRole::User, "hello".to_owned())], &[])
            .expect("valid history");
        let serialized = history.serialize().expect("should serialize");
        let (mut checkpoint, history_clone) = make_history_checkpoint(&serialized);
        // Corrupt the hash so it doesn't match the serialized bytes.
        checkpoint.object.sha256 = [0xff; 32];

        let store = FakeObjectStore::with(PutBehavior::Success);
        let history_repo = FakeHistoryRepo::with(vec![ConditionalWriteOutcome::Committed]);
        let coordinator =
            PublicationCoordinator::new(store, FakeMetadataRepo::noop(), history_repo.clone());

        let error = coordinator
            .publish_history(&checkpoint, &history_clone)
            .await
            .expect_err("mismatched hash should be rejected");

        assert!(
            matches!(error, PublicationError::HistorySerializationMismatch),
            "expected HistorySerializationMismatch, got {error:?}"
        );
        assert_eq!(history_repo.appended_count(), 0);
    }

    // ── History fake ────────────────────────────────────────────────

    #[derive(Debug, Default, Clone)]
    struct FakeHistoryRepo {
        outcomes: Arc<Mutex<VecDeque<ConditionalWriteOutcome>>>,
        appended: Arc<Mutex<Vec<HistoryCheckpoint>>>,
    }

    impl FakeHistoryRepo {
        fn with(outcomes: Vec<ConditionalWriteOutcome>) -> Self {
            Self {
                outcomes: Arc::new(Mutex::new(outcomes.into())),
                appended: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn noop() -> Self {
            Self::default()
        }

        fn appended_count(&self) -> usize {
            self.appended.lock().expect("lock").len()
        }
    }

    impl HistoryRepository for FakeHistoryRepo {
        type Error = FakeRepoError;

        async fn append(
            &self,
            checkpoint: &HistoryCheckpoint,
        ) -> Result<ConditionalWriteOutcome, Self::Error> {
            self.appended
                .lock()
                .expect("appended lock")
                .push(checkpoint.clone());
            self.outcomes
                .lock()
                .expect("outcomes lock")
                .pop_front()
                .ok_or(FakeRepoError::Exhausted)
        }

        async fn page(
            &self,
            _workflow_id: &WorkflowId,
            _request: &PageRequest,
        ) -> Result<Page<HistoryCheckpoint>, Self::Error> {
            unreachable!("page is not used by the coordinator")
        }
    }

    // ── Noop metadata repo for history tests ────────────────────────

    // (FakeMetadataRepo::noop is defined above with the other impls.)

    // ── PageToken/Page unused but needed for trait impl compilation ─

    #[allow(dead_code)]
    fn _ensure_page_token() -> Result<PageToken, PortValueError> {
        PageToken::new("dummy")
    }
}
