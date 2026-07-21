#![deny(unsafe_code)]

//! Quotation (PDF) artifact delivery service.
//!
//! Builds a [`DeliveryRequest`] from a consumed confirmation + rendered PDF +
//! topic + destination name. Pure construction (no I/O).
//!
//! The confirmation must have been consumed with
//! [`StartDirectPdfGeneration`](domain::confirmation::ConfirmationAction::StartDirectPdfGeneration).

use std::fmt::{Display, Formatter};

use sha2::{Digest, Sha256};

use domain::confirmation::{ConfirmationAction, ConfirmationRecord, ConfirmationStatus};
use domain::confirmation::{MutationTargetFingerprint, PreviewDigest};
use domain::idempotency::{IdempotencyKey, OperationKind, OperationTargetFingerprint};
use domain::identity::{ParticipantId, TopicSessionId};
use domain::retry::RetryPolicy;
use domain::workflow::{Workflow, WorkflowRevision, WorkflowTimestamp};

use crate::artifact_delivery::DeliveryRequest;
use crate::ports::{ObjectClass, PortValueError, StorageKey, StorageRecordId, StoredObject};

// ── Proof ─────────────────────────────────────────────────────────────

/// Proof that a confirmation was consumed with `StartDirectPdfGeneration`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedPdfProof {
    /// The workflow owner.
    pub owner: ParticipantId,
    /// The confirmation's mutation-target fingerprint.
    pub mutation_target: MutationTargetFingerprint,
    /// Preview digest from the confirmation.
    pub preview_digest: PreviewDigest,
    /// Resulting workflow revision after consumption.
    pub workflow_revision: WorkflowRevision,
}

impl ConfirmedPdfProof {
    /// Extract a proof from a consumed [`ConfirmationRecord`].
    ///
    /// Returns an error if the record is not consumed or the action is not
    /// `StartDirectPdfGeneration`.
    pub fn from_consumed(record: &ConfirmationRecord) -> Result<Self, QuotationDeliveryError> {
        if record.status() != ConfirmationStatus::Consumed {
            return Err(QuotationDeliveryError::InvalidAction);
        }
        if record.action() != ConfirmationAction::StartDirectPdfGeneration {
            return Err(QuotationDeliveryError::InvalidAction);
        }
        Ok(Self {
            owner: record.owner(),
            mutation_target: record.mutation_target(),
            preview_digest: record.preview_digest(),
            workflow_revision: record
                .resulting_workflow_revision()
                .ok_or(QuotationDeliveryError::InvalidAction)?,
        })
    }
}

// ── Error ─────────────────────────────────────────────────────────────

/// Error building a quotation delivery request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotationDeliveryError {
    /// The confirmation was not consumed with `StartDirectPdfGeneration`.
    InvalidAction,
    /// The destination name is empty or invalid.
    InvalidDestinationName,
    /// The constructed [`StoredObject`] failed validation.
    Port(PortValueError),
}

impl Display for QuotationDeliveryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAction => {
                f.write_str("confirmation not consumed with StartDirectPdfGeneration")
            }
            Self::InvalidDestinationName => f.write_str("destination name is empty or invalid"),
            Self::Port(e) => write!(f, "stored object validation failed: {e}"),
        }
    }
}

impl std::error::Error for QuotationDeliveryError {}

impl From<PortValueError> for QuotationDeliveryError {
    fn from(value: PortValueError) -> Self {
        Self::Port(value)
    }
}

// ── Service ───────────────────────────────────────────────────────────

/// Builds a [`DeliveryRequest`] from a consumed confirmation + rendered PDF.
pub struct QuotationDeliveryService;

impl QuotationDeliveryService {
    /// Build the delivery request from a consumed confirmation proof.
    ///
    /// `workflow` must be post-consume in `PdfGenerationStarted` state.
    /// `proof` must have been extracted from the same confirmation record
    /// that produced this workflow revision.
    #[allow(clippy::too_many_arguments)]
    pub fn build_request(
        workflow: &Workflow,
        _proof: &ConfirmedPdfProof,
        pdf_bytes: Vec<u8>,
        destination_name: String,
        topic: TopicSessionId,
        retry_policy: RetryPolicy,
        timestamp: WorkflowTimestamp,
    ) -> Result<DeliveryRequest, QuotationDeliveryError> {
        if destination_name.trim().is_empty() {
            return Err(QuotationDeliveryError::InvalidDestinationName);
        }

        let resulting_revision = workflow.revision();

        // Compute artifact checksum.
        let sha256: [u8; 32] = Sha256::digest(&pdf_bytes).into();
        let hex_sha = hex::encode(sha256);
        let sha_prefix = &hex_sha[..16.min(hex_sha.len())];

        // Build storage identifiers.
        let object_id =
            StorageRecordId::new(format!("pdf_{}_{}", resulting_revision.get(), sha_prefix))?;
        let storage_key = StorageKey::new(format!(
            "artifacts/{}/{}",
            workflow.id().as_str(),
            object_id.as_str()
        ))?;

        // Build and validate StoredObject.
        let artifact = StoredObject {
            workflow_id: workflow.id().clone(),
            object_id,
            class: ObjectClass::Artifact,
            storage_key,
            byte_length: pdf_bytes.len() as u64,
            sha256,
            media_type: "application/pdf".to_owned(),
            created_at: timestamp,
        };
        artifact.validate()?;

        // Compose topic message (caller's template; link placeholder is
        // replaced by the delivery orchestrator after presigning).
        let topic_message = "Your quotation is ready: {link}".to_string();

        // Derive Drive idempotency key.
        let drive_target = {
            let mut data = Vec::new();
            data.extend_from_slice(b"drive:");
            data.extend_from_slice(hex::encode(sha256).as_bytes());
            data.extend_from_slice(b":");
            data.extend_from_slice(destination_name.as_bytes());
            let hash: [u8; 32] = Sha256::digest(&data).into();
            OperationTargetFingerprint::new(hash)
        };
        let drive_key = IdempotencyKey::new(
            workflow.id().clone(),
            resulting_revision,
            OperationKind::GoogleWrite,
            drive_target,
        );

        // Derive Telegram idempotency key.
        let message_hash: [u8; 32] = Sha256::digest(topic_message.as_bytes()).into();
        let tg_target = {
            let mut data = Vec::new();
            data.extend_from_slice(b"tg:");
            let canonical = format!(
                "{}:{}",
                topic.chat_id().get(),
                topic.message_thread_id().get()
            );
            data.extend_from_slice(canonical.as_bytes());
            data.extend_from_slice(b":");
            data.extend_from_slice(hex::encode(message_hash).as_bytes());
            let hash: [u8; 32] = Sha256::digest(&data).into();
            OperationTargetFingerprint::new(hash)
        };
        let telegram_key = IdempotencyKey::new(
            workflow.id().clone(),
            resulting_revision,
            OperationKind::TelegramSend,
            tg_target,
        );

        Ok(DeliveryRequest {
            artifact,
            pdf_bytes,
            drive_key,
            telegram_key,
            destination_name,
            topic,
            retry_policy,
            topic_message,
        })
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
    use domain::confirmation::{
        ConfirmationAction, ConfirmationRecord, ConfirmationStatus, MutationTargetFingerprint,
        PreviewDigest,
    };
    use domain::identity::{
        ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, WorkflowId,
    };
    use domain::retry::RetryPolicy;
    use domain::transition::{TransitionRequest, WorkflowTransition};
    use domain::workflow::{WaitDeadline, Workflow, WorkflowTimestamp};

    fn topic() -> TopicSessionId {
        TopicSessionId::new(
            ChatId::new(-1001),
            MessageThreadId::new(77).expect("thread id"),
        )
    }

    fn participant(value: i64) -> ParticipantId {
        ParticipantId::new(value).expect("participant id")
    }

    fn time(seconds: u64) -> WorkflowTimestamp {
        WorkflowTimestamp::from_unix_seconds(seconds)
    }

    fn advance(workflow: &Workflow, transition: WorkflowTransition, at: u64) -> Workflow {
        workflow
            .transition(TransitionRequest {
                transition,
                expected_revision: workflow.revision(),
                actor: participant(202),
                source_message: MessageId::new(i64::try_from(at).unwrap()).expect("message id"),
                timestamp: time(at),
            })
            .expect("transition accepted")
            .workflow
    }

    fn drafting_completed() -> Workflow {
        let wf = Workflow::new(
            WorkflowId::new("wf-quotation").expect("workflow id"),
            topic(),
            participant(101),
            time(1),
        );
        let wf = advance(
            &wf,
            WorkflowTransition::BeginAttachmentCollection {
                deadline: WaitDeadline::at(time(30)),
            },
            2,
        );
        let wf = advance(&wf, WorkflowTransition::FinishAttachmentCollection, 3);
        let wf = advance(&wf, WorkflowTransition::CompleteExtraction, 4);
        let wf = advance(&wf, WorkflowTransition::StartCalculationOrDrafting, 5);
        advance(&wf, WorkflowTransition::CompleteCalculationOrDrafting, 6)
    }

    fn policy() -> RetryPolicy {
        RetryPolicy::new(3, 500, 10_000, 2_000).expect("valid policy")
    }

    fn make_pending_confirmation(workflow: &Workflow) -> (Workflow, ConfirmationRecord) {
        let wf = workflow.clone();
        let transition = wf
            .issue_confirmation(domain::confirmation::ConfirmationIssueRequest {
                confirmation_id: ConfirmationId::new("confirmation-pdf").expect("conf id"),
                expected_workflow_revision: wf.revision(),
                topic: wf.topic(),
                preview_digest: PreviewDigest::new([1u8; 32]),
                mutation_target: MutationTargetFingerprint::new([2u8; 32]),
                action: ConfirmationAction::StartDirectPdfGeneration,
                deadline: WaitDeadline::at(time(43_200)),
                actor: participant(202),
                source_message: MessageId::new(7).expect("msg"),
                timestamp: time(7),
            })
            .expect("issue confirmation");
        let record = ConfirmationRecord::Pending(transition.confirmation);
        (transition.transition.workflow, record)
    }

    fn consume_confirmation(
        record: &ConfirmationRecord,
        workflow: &Workflow,
        at: u64,
    ) -> (Workflow, ConfirmationRecord) {
        use domain::authorization::{
            LiveMembershipEvidence, MembershipStatus, authorize_participant,
        };
        let authz_participant = authorize_participant(
            workflow.topic().chat_id(),
            participant(202),
            &LiveMembershipEvidence::new(
                workflow.topic().chat_id(),
                participant(202),
                MembershipStatus::Approved,
                time(at),
            ),
            time(at),
        )
        .expect("authorized");
        let authz = authz_participant.for_workflow(workflow);

        use domain::confirmation::{ConfirmationConsumeRequest, TopicMessageReference};
        let request = ConfirmationConsumeRequest {
            preview_digest: PreviewDigest::new([1u8; 32]),
            mutation_target: MutationTargetFingerprint::new([2u8; 32]),
            source: TopicMessageReference::new(
                workflow.topic(),
                MessageId::new(i64::try_from(at).unwrap()).expect("msg"),
            ),
            confirmed_at: time(at),
        };
        let outcome = record.consume(workflow, &authz, request).expect("consume");
        let consumed_record = outcome.confirmation().clone();
        (outcome.transition().workflow.clone(), consumed_record)
    }

    #[test]
    fn proof_from_consumed_rejects_pending_record() {
        let (_wf, pending) = make_pending_confirmation(&drafting_completed());
        assert_eq!(pending.status(), ConfirmationStatus::Pending);
        let result = ConfirmedPdfProof::from_consumed(&pending);
        assert!(matches!(result, Err(QuotationDeliveryError::InvalidAction)));
    }

    #[test]
    fn proof_from_consumed_rejects_wrong_action() {
        let wf = drafting_completed();
        let transition = wf
            .issue_confirmation(domain::confirmation::ConfirmationIssueRequest {
                confirmation_id: ConfirmationId::new("confirmation-sheet").expect("conf id"),
                expected_workflow_revision: wf.revision(),
                topic: wf.topic(),
                preview_digest: PreviewDigest::new([1u8; 32]),
                mutation_target: MutationTargetFingerprint::new([2u8; 32]),
                action: ConfirmationAction::StartSheetOrDocWrite,
                deadline: WaitDeadline::at(time(43_200)),
                actor: participant(202),
                source_message: MessageId::new(7).expect("msg"),
                timestamp: time(7),
            })
            .expect("issue");
        let record = ConfirmationRecord::Pending(transition.confirmation);
        let (_wf2, consumed) = consume_confirmation(&record, &transition.transition.workflow, 8);
        // consumed has StartSheetOrDocWrite, not StartDirectPdfGeneration
        assert_eq!(consumed.action(), ConfirmationAction::StartSheetOrDocWrite);
        let result = ConfirmedPdfProof::from_consumed(&consumed);
        assert!(matches!(result, Err(QuotationDeliveryError::InvalidAction)));
    }

    #[test]
    fn proof_from_consumed_succeeds_for_correct_action() {
        let (wf, pending) = make_pending_confirmation(&drafting_completed());
        let (wf2, consumed) = consume_confirmation(&pending, &wf, 8);
        assert_eq!(
            consumed.action(),
            ConfirmationAction::StartDirectPdfGeneration
        );
        let proof = ConfirmedPdfProof::from_consumed(&consumed).expect("proof should be valid");
        assert_eq!(proof.owner, participant(101));
        assert_eq!(proof.workflow_revision, wf2.revision());
    }

    #[test]
    fn build_request_produces_valid_delivery_request() {
        let (wf, pending) = make_pending_confirmation(&drafting_completed());
        let (wf2, consumed) = consume_confirmation(&pending, &wf, 8);
        let proof = ConfirmedPdfProof::from_consumed(&consumed).expect("proof");

        let pdf_bytes = b"%PDF-1.4 fake pdf content" as &[u8];
        let request = QuotationDeliveryService::build_request(
            &wf2,
            &proof,
            pdf_bytes.to_vec(),
            "quotation-2025-01.pdf".to_owned(),
            topic(),
            policy(),
            time(9),
        )
        .expect("build request");

        assert_eq!(request.artifact.class, ObjectClass::Artifact);
        assert_eq!(request.artifact.media_type, "application/pdf");
        assert_eq!(request.artifact.byte_length, pdf_bytes.len() as u64);
        assert_eq!(request.pdf_bytes, pdf_bytes);
        assert_eq!(request.destination_name, "quotation-2025-01.pdf");
        assert_eq!(request.topic.chat_id(), topic().chat_id());
        assert_eq!(
            request.topic.message_thread_id(),
            topic().message_thread_id()
        );
        assert_eq!(
            request.drive_key.operation_kind(),
            OperationKind::GoogleWrite
        );
        assert_eq!(
            request.telegram_key.operation_kind(),
            OperationKind::TelegramSend
        );
        // Keys bind the resulting workflow revision.
        assert_eq!(request.drive_key.workflow_revision(), wf2.revision());
        assert_eq!(request.telegram_key.workflow_revision(), wf2.revision());
        // Keys bind the correct workflow id.
        assert_eq!(request.drive_key.workflow_id(), wf2.id());
    }

    #[test]
    fn build_request_rejects_empty_destination_name() {
        let (wf, pending) = make_pending_confirmation(&drafting_completed());
        let (wf2, consumed) = consume_confirmation(&pending, &wf, 8);
        let proof = ConfirmedPdfProof::from_consumed(&consumed).expect("proof");

        let result = QuotationDeliveryService::build_request(
            &wf2,
            &proof,
            b"pdf".to_vec(),
            "  ".to_owned(),
            topic(),
            policy(),
            time(9),
        );
        assert!(matches!(
            result,
            Err(QuotationDeliveryError::InvalidDestinationName)
        ));
    }

    #[test]
    fn build_request_idempotency_keys_are_deterministic() {
        let (wf, pending) = make_pending_confirmation(&drafting_completed());
        let (wf2, consumed) = consume_confirmation(&pending, &wf, 8);
        let proof = ConfirmedPdfProof::from_consumed(&consumed).expect("proof");

        let pdf_bytes = b"same pdf bytes" as &[u8];
        let name = "quote.pdf";
        let t = topic();
        let rp = policy();
        let ts = time(9);

        let req1 = QuotationDeliveryService::build_request(
            &wf2,
            &proof,
            pdf_bytes.to_vec(),
            name.to_owned(),
            t,
            rp,
            ts,
        )
        .expect("request 1");
        let req2 = QuotationDeliveryService::build_request(
            &wf2,
            &proof,
            pdf_bytes.to_vec(),
            name.to_owned(),
            t,
            rp,
            ts,
        )
        .expect("request 2");

        assert_eq!(req1.drive_key, req2.drive_key);
        assert_eq!(req1.telegram_key, req2.telegram_key);
    }
}
