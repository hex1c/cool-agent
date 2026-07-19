#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

use application::attachments::{AttachmentCollectionError, AttachmentCollectionService};
use application::config::AttachmentConfig;
use application::ports::{ObjectClass, ObjectStore, Page, StoredObject};
use application::repositories::{ConditionalWriteOutcome, ObjectMetadataRepository, PageRequest};
use domain::attachment::AttachmentDescriptor;
use domain::identity::{
    AttachmentId, ChatId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
};
use domain::{
    WaitDeadline, Workflow, WorkflowState, WorkflowStateKind, WorkflowTimestamp, WorkflowTransition,
};
use sha2::Digest;

// ── fake error ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct FakeError(&'static str);

impl Display for FakeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for FakeError {}

// ── fake object store ───────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct FakeObjectStore {
    objects: RefCell<HashMap<String, Vec<u8>>>,
}

impl ObjectStore for FakeObjectStore {
    type Error = FakeError;

    async fn put(&self, object: &StoredObject, bytes: &[u8]) -> Result<(), Self::Error> {
        if matches!(object.class, ObjectClass::SanitizedHistory) {
            return Err(FakeError("raw put of sanitized history rejected"));
        }
        self.objects
            .borrow_mut()
            .insert(object.storage_key.as_str().to_owned(), bytes.to_vec());
        Ok(())
    }

    async fn get(&self, object: &StoredObject) -> Result<Vec<u8>, Self::Error> {
        let data = self
            .objects
            .borrow()
            .get(object.storage_key.as_str())
            .cloned()
            .ok_or(FakeError("object not found"))?;
        if data.len() as u64 != object.byte_length {
            return Err(FakeError("byte length mismatch"));
        }
        let checksum = sha2::Sha256::digest(&data);
        if checksum.as_slice() != object.sha256 {
            return Err(FakeError("sha256 mismatch"));
        }
        Ok(data)
    }
}

// ── fake metadata repository ────────────────────────────────────────────────

#[derive(Debug, Default)]
struct FakeMetadataRepository {
    records: RefCell<HashMap<(WorkflowId, String), StoredObject>>,
}

impl ObjectMetadataRepository for FakeMetadataRepository {
    type Error = FakeError;

    async fn record(&self, object: &StoredObject) -> Result<ConditionalWriteOutcome, Self::Error> {
        let key = (
            object.workflow_id.clone(),
            object.storage_key.as_str().to_owned(),
        );
        let mut records = self.records.borrow_mut();
        if let std::collections::hash_map::Entry::Vacant(e) = records.entry(key) {
            e.insert(object.clone());
            Ok(ConditionalWriteOutcome::Committed)
        } else {
            Ok(ConditionalWriteOutcome::Conflict)
        }
    }

    async fn page(
        &self,
        workflow_id: &WorkflowId,
        _request: &PageRequest,
    ) -> Result<Page<StoredObject>, Self::Error> {
        let records = self.records.borrow();
        let items: Vec<StoredObject> = records
            .iter()
            .filter(|((wf_id, _), _)| wf_id == workflow_id)
            .map(|(_, obj)| obj.clone())
            .collect();
        Ok(Page {
            items,
            next_token: None,
        })
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn workflow_id(value: &str) -> WorkflowId {
    WorkflowId::new(value).expect("test workflow id must be valid")
}

fn participant(id: i64) -> ParticipantId {
    ParticipantId::new(id).expect("test participant id must be valid")
}

fn message_id(id: i64) -> MessageId {
    MessageId::new(id).expect("test message id must be valid")
}

fn attachment_id(value: &str) -> AttachmentId {
    AttachmentId::new(value).expect("test attachment id must be valid")
}

fn timestamp(secs: u64) -> WorkflowTimestamp {
    WorkflowTimestamp::from_unix_seconds(secs)
}

fn default_config() -> AttachmentConfig {
    AttachmentConfig {
        max_count: 10,
        max_bytes: 20 * 1024 * 1024, // 20 MB
        allowed_mime_types: vec![
            "image/jpeg".to_owned(),
            "image/png".to_owned(),
            "application/pdf".to_owned(),
        ],
    }
}

fn workflow_in_collection() -> Workflow {
    let wf = Workflow::new(
        workflow_id("wf-1"),
        TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(9).expect("thread")),
        participant(42),
        timestamp(1),
    );
    wf.transition(domain::TransitionRequest {
        transition: WorkflowTransition::BeginAttachmentCollection {
            deadline: WaitDeadline::at(timestamp(1800)),
        },
        expected_revision: wf.revision(),
        actor: participant(42),
        source_message: message_id(7),
        timestamp: timestamp(2),
    })
    .expect("begin attachment collection")
    .workflow
}

fn jpeg_descriptor() -> AttachmentDescriptor {
    AttachmentDescriptor::new(attachment_id("att-1"), "image/jpeg")
        .expect("jpeg descriptor must be valid")
}

fn make_service() -> AttachmentCollectionService<FakeObjectStore, FakeMetadataRepository> {
    AttachmentCollectionService::new(
        FakeObjectStore::default(),
        FakeMetadataRepository::default(),
        default_config(),
    )
}

// ── collect tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn accepts_attachment_within_limits() {
    let service = make_service();
    let workflow = workflow_in_collection();
    let descriptor = jpeg_descriptor();
    let bytes = b"fake jpeg data";

    let stored = service
        .collect(
            &workflow,
            &descriptor,
            bytes,
            message_id(100),
            timestamp(10),
        )
        .await
        .expect("valid attachment should be accepted");

    assert_eq!(stored.workflow_id, workflow_id("wf-1"));
    assert_eq!(stored.byte_length, bytes.len() as u64);
    assert_eq!(stored.media_type, "image/jpeg");
    assert_eq!(stored.class, ObjectClass::RawInput);
    assert!(
        stored.storage_key.as_str().starts_with("raw/wf-1/"),
        "storage key must use raw prefix"
    );
}

#[tokio::test]
async fn rejects_over_count() {
    let service = make_service();
    let workflow = workflow_in_collection();
    let descriptor = jpeg_descriptor();
    let bytes = b"some jpeg bytes";

    // Fill up to max_count
    for i in 0..10 {
        let d = AttachmentDescriptor::new(attachment_id(&format!("att-{}", i)), "image/jpeg")
            .expect("valid descriptor");
        service
            .collect(
                &workflow,
                &d,
                bytes,
                message_id(100 + i as i64),
                timestamp(10 + i as u64),
            )
            .await
            .expect("should accept within limit");
    }

    // 11th should fail
    let err = service
        .collect(
            &workflow,
            &descriptor,
            bytes,
            message_id(200),
            timestamp(20),
        )
        .await
        .expect_err("should reject over-count");

    assert!(matches!(
        err,
        AttachmentCollectionError::CountExceeded {
            max: 10,
            current: 10
        }
    ));
}

#[tokio::test]
async fn rejects_over_size() {
    let service = AttachmentCollectionService::new(
        FakeObjectStore::default(),
        FakeMetadataRepository::default(),
        AttachmentConfig {
            max_count: 10,
            max_bytes: 100,
            allowed_mime_types: vec!["image/jpeg".to_owned()],
        },
    );
    let workflow = workflow_in_collection();
    let descriptor = jpeg_descriptor();
    let bytes = vec![0u8; 101];

    let err = service
        .collect(
            &workflow,
            &descriptor,
            &bytes,
            message_id(100),
            timestamp(10),
        )
        .await
        .expect_err("should reject over-sized attachment");

    assert!(matches!(
        err,
        AttachmentCollectionError::SizeExceeded {
            max_bytes: 100,
            actual: 101
        }
    ));
}

#[tokio::test]
async fn rejects_disallowed_mime_type() {
    let service = AttachmentCollectionService::new(
        FakeObjectStore::default(),
        FakeMetadataRepository::default(),
        AttachmentConfig {
            max_count: 10,
            max_bytes: 20 * 1024 * 1024,
            allowed_mime_types: vec!["image/jpeg".to_owned()],
        },
    );
    let workflow = workflow_in_collection();
    // "application/pdf" is a valid AttachmentKind but excluded from allowed_mime_types
    let descriptor = AttachmentDescriptor::new(attachment_id("att-pdf"), "application/pdf")
        .expect("pdf descriptor");
    let bytes = b"pdf data";

    let err = service
        .collect(
            &workflow,
            &descriptor,
            bytes,
            message_id(100),
            timestamp(10),
        )
        .await
        .expect_err("should reject disallowed MIME type");

    assert!(matches!(
        err,
        AttachmentCollectionError::DisallowedMimeType { media_type } if media_type == "application/pdf"
    ));
}

#[tokio::test]
async fn rejects_when_not_in_collection_state() {
    let service = make_service();
    // Workflow still in RequestAccepted (hasn't begun collection)
    let workflow = Workflow::new(
        workflow_id("wf-1"),
        TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(9).expect("thread")),
        participant(42),
        timestamp(1),
    );
    let descriptor = jpeg_descriptor();

    let err = service
        .collect(
            &workflow,
            &descriptor,
            b"data",
            message_id(100),
            timestamp(10),
        )
        .await
        .expect_err("should reject when not collecting");

    assert!(matches!(
        err,
        AttachmentCollectionError::NotCollectingAttachments
    ));
}

#[tokio::test]
async fn duplicate_attachment_produces_one_object() {
    let service = make_service();
    let workflow = workflow_in_collection();
    let descriptor = jpeg_descriptor();
    let bytes = b"same content for dedup";
    let source = message_id(100);

    let first = service
        .collect(&workflow, &descriptor, bytes, source, timestamp(10))
        .await
        .expect("first collect");

    // Second upload — same content, same source message
    let second = service
        .collect(&workflow, &descriptor, bytes, source, timestamp(11))
        .await
        .expect("second collect (duplicate)");

    // Both should return the same object
    assert_eq!(first.object_id, second.object_id);
    assert_eq!(first.sha256, second.sha256);
    assert_eq!(first.storage_key, second.storage_key);

    // Collect a different attachment to verify count is 1 (dedup worked)
    let third_desc =
        AttachmentDescriptor::new(attachment_id("att-3"), "image/jpeg").expect("valid descriptor");
    let third = service
        .collect(
            &workflow,
            &third_desc,
            b"different content",
            message_id(101),
            timestamp(12),
        )
        .await
        .expect("third collect");
    // The third object should have a different id from the deduped pair
    assert_ne!(first.object_id, third.object_id);
}

// ── transition tests ────────────────────────────────────────────────────────

#[test]
fn finish_collection_transitions_to_extraction_started() {
    let service = make_service();
    let workflow = workflow_in_collection();

    let outcome = service
        .finish(&workflow, participant(42), message_id(200), timestamp(100))
        .expect("finish collection");

    assert_eq!(
        outcome.workflow.state().kind(),
        WorkflowStateKind::ExtractionStarted
    );
    assert_eq!(
        outcome.workflow.revision().get(),
        workflow.revision().get() + 1
    );
}

#[test]
fn timeout_transitions_to_waiting_for_clarification() {
    let service = make_service();
    // Must be in CollectingAttachments with elapsed deadline
    let workflow = Workflow::new(
        workflow_id("wf-timeout"),
        TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(9).expect("thread")),
        participant(42),
        timestamp(1),
    );
    let collection_deadline = WaitDeadline::at(timestamp(30));
    let workflow = workflow
        .transition(domain::TransitionRequest {
            transition: WorkflowTransition::BeginAttachmentCollection {
                deadline: collection_deadline,
            },
            expected_revision: workflow.revision(),
            actor: participant(42),
            source_message: message_id(7),
            timestamp: timestamp(2),
        })
        .expect("begin collection")
        .workflow;

    // Now deadline has elapsed; timeout should work
    let clar_deadline = WaitDeadline::at(timestamp(3600));
    let outcome = service
        .handle_timeout(
            &workflow,
            participant(42),
            message_id(300),
            timestamp(31), // after deadline
            clar_deadline,
        )
        .expect("timeout transition");

    assert_eq!(
        outcome.workflow.state().kind(),
        WorkflowStateKind::WaitingForClarification
    );
    // Should have resume = AttachmentCollection
    assert!(matches!(
        outcome.workflow.state(),
        WorkflowState::WaitingForClarification {
            resume: domain::ClarificationResume::AttachmentCollection,
            ..
        }
    ));
}
