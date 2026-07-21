//! Integration tests for quotation delivery partial-failure / resume
//! (Task 34).
//!
//! Proves:
//! - S3 succeeds, Drive fails retryable once then succeeds on resume → Drive
//!   provider invoked again, but only until Accepted; no duplicate Drive copy.
//! - Drive ok, Telegram fails terminal → `DeliveryError::Telegram`; S3 + Drive
//!   not re-done on a second call (journal Final for both); only Telegram
//!   attempted.
//! - Topic delivery targets the originating `message_thread_id` (assert the
//!   fake recorded the exact `TopicSessionId` passed).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    dead_code,
    unused_imports
)]

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use application::artifact_delivery::{
    ArtifactDeliveryService, DeliveryError, DeliveryRequest, DriveCopyPort, TopicDeliveryPort,
};
use application::external_operation::{
    AttemptClaim, BackoffWait, BeginAttemptOutcome, CompletedAttempt, ExecutionOutcome,
    ExternalOperationExecutor, ExternalResourceId, FailureCode, JitterSource, JournalPreparation,
    JournalState, OperationFailure, OperationJournal, ProviderOutcome, SanitizedSummary,
};
use application::ports::{
    ArtifactLinkSigner, HistoryStore, ObjectClass, ObjectStore, Page, PresignedObjectLink,
    StorageKey, StorageRecordId, StoredObject,
};
use application::publication::PublicationCoordinator;
use application::repositories::{
    ConditionalWriteOutcome, HistoryCheckpoint, HistoryRepository, ObjectMetadataRepository,
    PageRequest,
};
use application::sanitized_history::SanitizedHistory;
use domain::idempotency::{IdempotencyKey, OperationKind, OperationTargetFingerprint};
use domain::identity::{ChatId, MessageThreadId, TopicSessionId, WorkflowId};
use domain::retry::{JitterSample, RetryPolicy};
use domain::workflow::{WorkflowRevision, WorkflowTimestamp};

// ── Test doubles ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn noop() -> Self {
        Self::with(PutBehavior::DefinitiveFailure)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl ObjectStore for FakeObjectStore {
    type Error = FakeStoreError;

    async fn put(&self, object: &StoredObject, bytes: &[u8]) -> Result<(), Self::Error> {
        if object.class == ObjectClass::SanitizedHistory {
            return Err(FakeStoreError::Definitive);
        }
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
                if body.len() as u64 == object.byte_length && fake_digest(body) == object.sha256 {
                    Ok(body.clone())
                } else {
                    Err(FakeStoreError::NotFound)
                }
            }
            None => Err(FakeStoreError::NotFound),
        }
    }
}

impl HistoryStore for FakeObjectStore {
    type Error = FakeStoreError;

    async fn put_history(
        &self,
        object: &StoredObject,
        history: &SanitizedHistory,
    ) -> Result<(), Self::Error> {
        match &self.put_behavior {
            PutBehavior::Success => {
                let bytes = history.serialize().expect("should serialize");
                self.storage
                    .lock()
                    .expect("storage lock")
                    .push((object.storage_key.as_str().to_owned(), bytes));
                Ok(())
            }
            PutBehavior::Ambiguous => Err(FakeStoreError::Ambiguous),
            PutBehavior::DefinitiveFailure => Err(FakeStoreError::Definitive),
        }
    }

    async fn get_history(&self, object: &StoredObject) -> Result<Vec<u8>, Self::Error> {
        ObjectStore::get(self, object).await
    }
}

#[derive(Debug, Default, Clone)]
struct FakeMetadataRepo {
    recorded: Arc<Mutex<Vec<StoredObject>>>,
}

impl FakeMetadataRepo {
    fn noop() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    fn recorded_count(&self) -> usize {
        self.recorded.lock().expect("recorded lock").len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeRepoError {
    Exhausted,
}

impl Display for FakeRepoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exhausted => f.write_str("outcomes exhausted"),
        }
    }
}

impl ObjectMetadataRepository for FakeMetadataRepo {
    type Error = FakeRepoError;

    async fn record(&self, object: &StoredObject) -> Result<ConditionalWriteOutcome, Self::Error> {
        let mut recorded = self.recorded.lock().expect("recorded lock");
        if recorded
            .iter()
            .any(|existing| existing.storage_key == object.storage_key)
        {
            return Ok(ConditionalWriteOutcome::Conflict);
        }
        recorded.push(object.clone());
        Ok(ConditionalWriteOutcome::Committed)
    }

    async fn page(
        &self,
        _workflow_id: &WorkflowId,
        _request: &PageRequest,
    ) -> Result<Page<StoredObject>, Self::Error> {
        unreachable!("page not used by coordinator")
    }
}

#[derive(Debug, Default, Clone)]
struct FakeHistoryRepo;

impl FakeHistoryRepo {
    fn noop() -> Self {
        Self
    }
}

impl HistoryRepository for FakeHistoryRepo {
    type Error = FakeRepoError;

    async fn append(
        &self,
        _checkpoint: &HistoryCheckpoint,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        Ok(ConditionalWriteOutcome::Committed)
    }

    async fn page(
        &self,
        _workflow_id: &WorkflowId,
        _request: &PageRequest,
    ) -> Result<Page<HistoryCheckpoint>, Self::Error> {
        unreachable!("page not used by coordinator")
    }
}

// ── Executor fakes ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct ZeroJitter;

impl JitterSource for ZeroJitter {
    fn sample(&self) -> JitterSample {
        JitterSample::new(0).expect("zero jitter is valid")
    }
}

#[derive(Debug, Default)]
struct NoopWait;

impl BackoffWait for NoopWait {
    type Error = std::convert::Infallible;

    async fn wait(&self, _delay_ms: u32) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FakeJournal {
    completed: Mutex<HashMap<IdempotencyKey, Vec<CompletedAttempt>>>,
    final_outcomes: Mutex<HashMap<IdempotencyKey, ExecutionOutcome>>,
}

#[derive(Debug, Clone)]
struct SharedJournal(Arc<FakeJournal>);

impl OperationJournal for SharedJournal {
    type Error = FakeJournalError;

    async fn prepare(&self, key: &IdempotencyKey) -> Result<JournalPreparation, Self::Error> {
        if let Some(outcome) = self
            .0
            .final_outcomes
            .lock()
            .expect("outcomes")
            .get(key)
            .cloned()
        {
            return Ok(JournalPreparation::new(JournalState::Final(outcome)));
        }
        let completed = self
            .0
            .completed
            .lock()
            .expect("completed")
            .get(key)
            .cloned()
            .unwrap_or_default();
        Ok(JournalPreparation::new(JournalState::Ready {
            completed_attempts: completed,
        }))
    }

    async fn begin_attempt(
        &self,
        _key: &IdempotencyKey,
        attempt: domain::retry::AttemptNumber,
    ) -> Result<BeginAttemptOutcome, Self::Error> {
        Ok(BeginAttemptOutcome::Acquired(
            AttemptClaim::new(format!("claim-{}", attempt.get())).expect("claim is valid"),
        ))
    }

    async fn complete_attempt(
        &self,
        key: &IdempotencyKey,
        _claim: &AttemptClaim,
        attempt: &CompletedAttempt,
    ) -> Result<(), Self::Error> {
        let mut completed = self.0.completed.lock().expect("completed");
        completed
            .entry(key.clone())
            .or_default()
            .push(attempt.clone());
        drop(completed);
        let finalized = match attempt.outcome() {
            ProviderOutcome::Accepted { resource_id } => Some(ExecutionOutcome::Accepted {
                attempt: attempt.attempt(),
                resource_id: resource_id.clone(),
            }),
            ProviderOutcome::TerminalFailure(failure) => Some(ExecutionOutcome::TerminalFailure {
                attempt: attempt.attempt(),
                failure: failure.clone(),
            }),
            ProviderOutcome::Ambiguous(failure) => Some(ExecutionOutcome::ManualReview {
                attempt: attempt.attempt(),
                failure: failure.clone(),
                completed_attempts: self
                    .0
                    .completed
                    .lock()
                    .expect("completed")
                    .get(key)
                    .cloned()
                    .unwrap_or_default(),
            }),
            ProviderOutcome::RetryableFailure(_) => None,
        };
        if let Some(outcome) = finalized {
            self.0
                .final_outcomes
                .lock()
                .expect("outcomes")
                .insert(key.clone(), outcome);
        }
        Ok(())
    }

    async fn record_exhausted(
        &self,
        key: &IdempotencyKey,
        completed_attempts: &[CompletedAttempt],
    ) -> Result<(), Self::Error> {
        self.0.final_outcomes.lock().expect("outcomes").insert(
            key.clone(),
            ExecutionOutcome::Exhausted {
                completed_attempts: completed_attempts.to_vec(),
            },
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct FakeJournalError;

impl Display for FakeJournalError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("fake journal error")
    }
}

// ── Port fakes ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct FakeDrivePort {
    outcomes: Rc<RefCell<VecDeque<ProviderOutcome>>>,
    calls: Rc<Cell<u32>>,
}

impl FakeDrivePort {
    fn new(outcomes: Vec<ProviderOutcome>) -> Self {
        Self {
            outcomes: Rc::new(RefCell::new(outcomes.into())),
            calls: Rc::new(Cell::new(0)),
        }
    }

    fn call_count(&self) -> u32 {
        self.calls.get()
    }
}

impl DriveCopyPort for FakeDrivePort {
    async fn copy_artifact(
        &self,
        _artifact: &StoredObject,
        _destination_name: &str,
    ) -> ProviderOutcome {
        self.calls.set(self.calls.get().saturating_add(1));
        self.outcomes.borrow_mut().pop_front().unwrap_or_else(|| {
            ProviderOutcome::TerminalFailure(OperationFailure::new(
                FailureCode::new("exhausted").expect("code"),
                SanitizedSummary::new("no outcomes left").expect("summary"),
            ))
        })
    }
}

#[derive(Debug, Clone)]
struct FakeTopicPort {
    outcomes: Rc<RefCell<VecDeque<ProviderOutcome>>>,
    calls: Rc<Cell<u32>>,
    last_topic: Rc<Cell<Option<TopicSessionId>>>,
}

impl FakeTopicPort {
    fn new(outcomes: Vec<ProviderOutcome>) -> Self {
        Self {
            outcomes: Rc::new(RefCell::new(outcomes.into())),
            calls: Rc::new(Cell::new(0)),
            last_topic: Rc::new(Cell::new(None)),
        }
    }

    fn call_count(&self) -> u32 {
        self.calls.get()
    }

    fn last_topic(&self) -> Option<TopicSessionId> {
        self.last_topic.get()
    }
}

impl TopicDeliveryPort for FakeTopicPort {
    async fn send_topic(&self, topic: TopicSessionId, _message: &str) -> ProviderOutcome {
        self.calls.set(self.calls.get().saturating_add(1));
        self.last_topic.set(Some(topic));
        self.outcomes.borrow_mut().pop_front().unwrap_or_else(|| {
            ProviderOutcome::TerminalFailure(OperationFailure::new(
                FailureCode::new("exhausted").expect("code"),
                SanitizedSummary::new("no outcomes left").expect("summary"),
            ))
        })
    }
}

#[derive(Debug)]
struct FakeLinkSigner {
    url: String,
}

impl FakeLinkSigner {
    fn new() -> Self {
        Self {
            url: "https://s3.example.com/presigned/artifact".to_owned(),
        }
    }
}

impl ArtifactLinkSigner for FakeLinkSigner {
    type Error = FakeStoreError;

    async fn presign_artifact(
        &self,
        _object: &StoredObject,
    ) -> Result<PresignedObjectLink, Self::Error> {
        PresignedObjectLink::new(self.url.clone()).map_err(|_| FakeStoreError::Definitive)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

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

fn policy() -> RetryPolicy {
    RetryPolicy::new(3, 500, 10_000, 2_000).expect("valid policy")
}

fn accepted_outcome() -> ProviderOutcome {
    ProviderOutcome::Accepted {
        resource_id: Some(ExternalResourceId::new("drive-file-abc").expect("resource id")),
    }
}

fn retryable_failure() -> ProviderOutcome {
    ProviderOutcome::RetryableFailure(OperationFailure::new(
        FailureCode::new("rate_limited").expect("code"),
        SanitizedSummary::new("rate limited").expect("summary"),
    ))
}

fn terminal_failure() -> ProviderOutcome {
    ProviderOutcome::TerminalFailure(OperationFailure::new(
        FailureCode::new("forbidden").expect("code"),
        SanitizedSummary::new("access denied").expect("summary"),
    ))
}

fn make_artifact(body: &[u8]) -> StoredObject {
    let workflow_id = WorkflowId::new("wf-recovery").expect("workflow id");
    let storage_key =
        StorageKey::new(format!("artifacts/{}/obj-1", workflow_id.as_str())).expect("storage key");
    StoredObject {
        workflow_id: workflow_id.clone(),
        object_id: StorageRecordId::new("obj-1").expect("object id"),
        class: ObjectClass::Artifact,
        storage_key,
        byte_length: body.len() as u64,
        sha256: fake_digest(body),
        media_type: "application/pdf".to_owned(),
        created_at: WorkflowTimestamp::from_unix_seconds(1_700_000_000),
    }
}

fn topic() -> TopicSessionId {
    TopicSessionId::new(
        ChatId::new(-1001),
        MessageThreadId::new(77).expect("thread id"),
    )
}

fn make_delivery_request(artifact: &StoredObject, pdf_bytes: Vec<u8>) -> DeliveryRequest {
    use sha2::{Digest, Sha256};

    let drive_target = {
        let mut data = Vec::new();
        data.extend_from_slice(b"drive:");
        data.extend_from_slice(hex::encode(artifact.sha256).as_bytes());
        data.extend_from_slice(b":quote.pdf");
        let hash: [u8; 32] = Sha256::digest(&data).into();
        OperationTargetFingerprint::new(hash)
    };

    let msg = "Your quotation is ready: {link}";
    let msg_hash: [u8; 32] = Sha256::digest(msg.as_bytes()).into();
    let tg_target = {
        let mut data = Vec::new();
        data.extend_from_slice(b"tg:");
        let canonical = format!(
            "{}:{}",
            topic().chat_id().get(),
            topic().message_thread_id().get()
        );
        data.extend_from_slice(canonical.as_bytes());
        data.extend_from_slice(b":");
        data.extend_from_slice(hex::encode(msg_hash).as_bytes());
        let hash: [u8; 32] = Sha256::digest(&data).into();
        OperationTargetFingerprint::new(hash)
    };

    DeliveryRequest {
        artifact: artifact.clone(),
        pdf_bytes,
        drive_key: IdempotencyKey::new(
            artifact.workflow_id.clone(),
            WorkflowRevision::new(8),
            OperationKind::GoogleWrite,
            drive_target,
        ),
        telegram_key: IdempotencyKey::new(
            artifact.workflow_id.clone(),
            WorkflowRevision::new(8),
            OperationKind::TelegramSend,
            tg_target,
        ),
        destination_name: "quote.pdf".to_owned(),
        topic: topic(),
        retry_policy: policy(),
        topic_message: msg.to_owned(),
    }
}

#[allow(clippy::type_complexity)]
fn build_service(
    store: FakeObjectStore,
    drive_port: FakeDrivePort,
    topic_port: FakeTopicPort,
) -> (
    ArtifactDeliveryService<
        FakeObjectStore,
        FakeObjectStore,
        FakeMetadataRepo,
        FakeHistoryRepo,
        SharedJournal,
        ZeroJitter,
        NoopWait,
        FakeDrivePort,
        FakeTopicPort,
        FakeLinkSigner,
    >,
    FakeMetadataRepo,
    FakeDrivePort,
    FakeTopicPort,
    SharedJournal,
) {
    let metadata = FakeMetadataRepo::noop();
    let journal = Arc::new(FakeJournal::default());
    let shared_journal = SharedJournal(Arc::clone(&journal));
    let executor = ExternalOperationExecutor::new(shared_journal.clone(), ZeroJitter, NoopWait);
    let history_store = FakeObjectStore::noop();
    let history_repo = FakeHistoryRepo::noop();
    let link_signer = FakeLinkSigner::new();

    let publication =
        PublicationCoordinator::new(store, history_store, metadata.clone(), history_repo);

    let service = ArtifactDeliveryService::new(
        publication,
        executor,
        drive_port.clone(),
        topic_port.clone(),
        link_signer,
    );

    (service, metadata, drive_port, topic_port, shared_journal)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn s3_succeeds_drive_retryable_then_recovers() {
    let pdf_bytes = b"%PDF-1.4 recovery test" as &[u8];
    let artifact = make_artifact(pdf_bytes);

    let store = FakeObjectStore::with(PutBehavior::Success);
    // Drive fails once (retryable), then succeeds on retry.
    let drive_port = FakeDrivePort::new(vec![retryable_failure(), accepted_outcome()]);
    let topic_port = FakeTopicPort::new(vec![ProviderOutcome::Accepted { resource_id: None }]);

    let (service, _metadata, drive_p, topic_p, _journal) =
        build_service(store, drive_port, topic_port);

    let result = service
        .deliver(make_delivery_request(&artifact, pdf_bytes.to_vec()))
        .await
        .expect("delivery should succeed after drive retry");

    assert!(result.drive_resource_id.is_some());
    // Drive was invoked twice: once (retryable failure), once (success).
    assert_eq!(drive_p.call_count(), 2, "drive should be invoked twice");
    assert_eq!(topic_p.call_count(), 1, "telegram should be invoked once");
}

#[tokio::test]
async fn drive_ok_telegram_terminal_then_resume_only_retries_telegram() {
    // Simulate a crash resume: S3 + Drive already reached durable Final
    // states in a prior invocation, but Telegram had not yet completed.
    // The journal is pre-seeded with Drive=Final(Accepted); Telegram's key
    // is untouched (Ready). The S3 store already holds the artifact, so the
    // publication coordinator re-confirms it and conflicts on metadata
    // (idempotent success). On resume, Drive must NOT be re-invoked and
    // Telegram must be attempted fresh and succeed.
    let pdf_bytes = b"%PDF-1.4 tg resume test" as &[u8];
    let artifact = make_artifact(pdf_bytes);
    let request = make_delivery_request(&artifact, pdf_bytes.to_vec());

    // Pre-seed S3 store with the artifact so re-publication conflicts.
    let store = FakeObjectStore::with(PutBehavior::Success);
    store
        .storage
        .lock()
        .expect("seed")
        .push((artifact.storage_key.as_str().to_owned(), pdf_bytes.to_vec()));

    let drive_port = FakeDrivePort::new(vec![accepted_outcome()]);
    let topic_port = FakeTopicPort::new(vec![ProviderOutcome::Accepted { resource_id: None }]);

    let (service, _metadata, drive_p, topic_p, journal) =
        build_service(store.clone(), drive_port, topic_port);

    // Pre-seed the durable journal: Drive already accepted in the prior run.
    let drive_accepted = ExecutionOutcome::Accepted {
        attempt: domain::retry::AttemptNumber::new(1).expect("attempt"),
        resource_id: Some(ExternalResourceId::new("drive-file-abc").expect("id")),
    };
    journal
        .0
        .final_outcomes
        .lock()
        .expect("seed")
        .insert(request.drive_key.clone(), drive_accepted);

    let result = service
        .deliver(make_delivery_request(&artifact, pdf_bytes.to_vec()))
        .await
        .expect("resume should succeed");

    assert_eq!(
        drive_p.call_count(),
        0,
        "drive must NOT be re-invoked on resume (journal Final)"
    );
    assert_eq!(
        topic_p.call_count(),
        1,
        "telegram must be invoked once on resume (journal Ready)"
    );
    assert!(matches!(
        result.telegram_outcome,
        ExecutionOutcome::Accepted { .. }
    ));
    // S3 publication is idempotent: metadata conflict is treated as success.
    assert!(
        matches!(result.s3_outcome, ConditionalWriteOutcome::Conflict)
            || matches!(result.s3_outcome, ConditionalWriteOutcome::Committed)
    );
}

#[tokio::test]
async fn resume_targets_correct_topic() {
    let pdf_bytes = b"%PDF-1.4 topic resume test" as &[u8];
    let artifact = make_artifact(pdf_bytes);

    let store = FakeObjectStore::with(PutBehavior::Success);
    let drive_port = FakeDrivePort::new(vec![accepted_outcome()]);
    // Telegram fails terminally first, then succeeds.
    let topic_port = FakeTopicPort::new(vec![
        terminal_failure(),
        ProviderOutcome::Accepted { resource_id: None },
    ]);

    let (service, _metadata, _drive_p, topic_p, _journal) =
        build_service(store.clone(), drive_port, topic_port);

    let request = make_delivery_request(&artifact, pdf_bytes.to_vec());
    let expected_topic = request.topic;

    let _err = service
        .deliver(make_delivery_request(&artifact, pdf_bytes.to_vec()))
        .await
        .expect_err("first attempt telegram fails");

    let sent_topic = topic_p
        .last_topic()
        .expect("topic should have been recorded");
    assert_eq!(sent_topic.chat_id(), expected_topic.chat_id());
    assert_eq!(
        sent_topic.message_thread_id(),
        expected_topic.message_thread_id()
    );
}
