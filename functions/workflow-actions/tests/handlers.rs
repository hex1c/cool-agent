//! Integration tests for the workflow-actions Lambda handlers (Task 34A).
//!
//! Exercises the three handler entry points against local fixtures covering
//! success, retryable failure, and terminal failure without AWS credentials.
//! The `delivery` handler uses an injected [`DeliveryRunner`] fake; the `pdf`
//! and `workflow` handlers are pure compute.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    dead_code
)]

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use application::artifact_delivery::{DeliveryError, DriveDeliveryError};
use domain::confirmation::ConfirmationAction;

use workflow_actions_functions::delivery::{
    DeliveryEvent, DeliveryEventError, DeliveryProcessError, DeliveryResultDto, DeliveryRunner,
    process_delivery,
};
use workflow_actions_functions::pdf::{PdfRenderError, PdfRenderEvent, process_pdf};
use workflow_actions_functions::workflow::{
    WorkflowActionError, WorkflowActionEvent, process_workflow_action,
};
use workflow_actions_functions::{EVENT_SCHEMA_VERSION, pdf::PdfRenderResult};

// ── PDF handler fixtures ──────────────────────────────────────────────

mod pdf_fixtures {
    use super::*;
    use pdf::render::{
        BankRenderData, CompanyRenderData, CustomerRenderData, LineItemRenderData,
        QuotationDocument, QuotationMetaRenderData, SummaryRenderData, TaxComponentRenderData,
    };

    pub fn valid_document() -> QuotationDocument {
        QuotationDocument {
            company: CompanyRenderData {
                legal_name: "Novus Consulting Pvt Ltd".to_owned(),
                tax_id: Some("29AABCN1234M1Z5".to_owned()),
                address: vec!["MG Road, Bengaluru".to_owned()],
                contact: vec!["+91 80 1234 5678".to_owned()],
                logo_ref: None,
                signature_ref: None,
            },
            bank: BankRenderData {
                bank_name: "HDFC Bank".to_owned(),
                account_name: "Novus Consulting Pvt Ltd".to_owned(),
                account_number: "50100012345678".to_owned(),
                ifsc: "HDFC0001234".to_owned(),
                branch: None,
                upi_id: None,
            },
            meta: QuotationMetaRenderData {
                quotation_number: "Q-2025-002".to_owned(),
                quotation_date: "2025-01-16".to_owned(),
                place_of_supply: "Karnataka (29)".to_owned(),
                validity: "30 days".to_owned(),
                currency: "INR".to_owned(),
                copy_label: None,
            },
            customer: CustomerRenderData {
                name: "Globex Corp".to_owned(),
                tax_id: None,
                billing_address: vec!["Globex HQ".to_owned()],
                shipping_address: vec!["Globex HQ".to_owned()],
                dispatch_origin: None,
            },
            line_items: vec![LineItemRenderData {
                sequence: 1,
                description: "Advisory".to_owned(),
                hsn_sac: "998314".to_owned(),
                quantity: "1".to_owned(),
                unit: "Nos".to_owned(),
                unit_rate: "75000.00".to_owned(),
                taxable_value: "75000.00".to_owned(),
                tax_rate: "18%".to_owned(),
                tax_amount: "13500.00".to_owned(),
                amount: "88500.00".to_owned(),
            }],
            summary: SummaryRenderData {
                item_count: 1,
                total_quantity: "1".to_owned(),
                taxable_total: "75000.00".to_owned(),
                tax_components: vec![TaxComponentRenderData {
                    label: "CGST+SGST".to_owned(),
                    rate: "18%".to_owned(),
                    amount: "13500.00".to_owned(),
                }],
                grand_total: "88500.00".to_owned(),
            },
            terms: vec!["Payment within 15 days.".to_owned()],
            amount_in_words: "Eighty eight thousand five hundred only".to_owned(),
            signatory_label: "Authorised Signatory".to_owned(),
        }
    }

    pub fn valid_event() -> PdfRenderEvent {
        PdfRenderEvent {
            schema_version: EVENT_SCHEMA_VERSION.to_owned(),
            document: valid_document(),
        }
    }
}

#[test]
fn pdf_handler_success() {
    let event = pdf_fixtures::valid_event();
    let result: PdfRenderResult = process_pdf(&event).expect("render succeeds");
    assert!(result.byte_length > 0);
    assert_eq!(result.sha256_hex.len(), 64);
    assert!(!result.pdf_bytes_base64.is_empty());
}

#[test]
fn pdf_handler_terminal_failure_invalid_document() {
    let mut doc = pdf_fixtures::valid_document();
    doc.meta.quotation_number = String::new();
    let event = PdfRenderEvent {
        schema_version: EVENT_SCHEMA_VERSION.to_owned(),
        document: doc,
    };
    let err = process_pdf(&event).expect_err("invalid doc is terminal");
    assert!(matches!(err, PdfRenderError::InvalidDocument { .. }));
}

#[test]
fn pdf_handler_terminal_failure_schema_mismatch() {
    let mut event = pdf_fixtures::valid_event();
    event.schema_version = "novus.workflow-actions.v0".to_owned();
    let err = process_pdf(&event).expect_err("schema mismatch is terminal");
    assert!(matches!(err, PdfRenderError::SchemaVersionMismatch));
}

// ── Workflow handler fixtures ─────────────────────────────────────────

mod workflow_fixtures {
    use super::*;
    use application::resumable_confirmation::{
        IssueConfirmationRequest, Preview, PreviewLineItem, ResumableConfirmationService,
    };
    use domain::authorization::MembershipStatus;
    use domain::confirmation::{ConfirmationAction, ConfirmationRecord, TopicMessageReference};
    use domain::identity::{
        ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, TopicSessionId,
        WorkflowId,
    };
    use domain::transition::{TransitionRequest, WorkflowTransition};
    use domain::workflow::{WaitDeadline, Workflow, WorkflowTimestamp};

    pub fn topic() -> TopicSessionId {
        TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77).expect("tid"))
    }

    pub fn participant(value: i64) -> ParticipantId {
        ParticipantId::new(value).expect("pid")
    }

    pub fn time(seconds: u64) -> WorkflowTimestamp {
        WorkflowTimestamp::from_unix_seconds(seconds)
    }

    fn advance(workflow: &Workflow, transition: WorkflowTransition, at: u64) -> Workflow {
        workflow
            .transition(TransitionRequest {
                transition,
                expected_revision: workflow.revision(),
                actor: participant(202),
                source_message: MessageId::new(i64::try_from(at).unwrap()).expect("msg"),
                timestamp: time(at),
            })
            .expect("transition accepted")
            .workflow
    }

    pub fn drafting_completed() -> Workflow {
        let wf = Workflow::new(
            WorkflowId::new("wf-handler-int").expect("wid"),
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

    pub fn preview() -> Preview {
        Preview::new(
            "INR".to_owned(),
            vec![PreviewLineItem {
                description: "Advisory".to_owned(),
                quantity: "1".to_owned(),
                unit_price_micro_inr: 75_000_000,
            }],
            vec!["standard rate".to_owned()],
            75_000_000,
            1800,
            13_500_000,
            88_500_000,
        )
        .expect("valid preview")
    }

    pub fn pending_event(at: u64) -> WorkflowActionEvent {
        let svc = ResumableConfirmationService;
        let wf = drafting_completed();
        let p = preview();
        let outcome = svc
            .issue_confirmation(
                &wf,
                IssueConfirmationRequest {
                    preview: &p,
                    action: ConfirmationAction::StartDirectPdfGeneration,
                    target_label: "pdf-render",
                    confirmation_id: ConfirmationId::new("confirmation-int").expect("cid"),
                    actor: participant(202),
                    source_message: MessageId::new(7).expect("msg"),
                    deadline: WaitDeadline::at(time(43_200)),
                    timestamp: time(7),
                },
            )
            .expect("issue confirmation");
        WorkflowActionEvent {
            schema_version: EVENT_SCHEMA_VERSION.to_owned(),
            confirmation: ConfirmationRecord::from(outcome.confirmation),
            workflow: outcome.transition.workflow,
            preview: p,
            target_label: "pdf-render".to_owned(),
            source: TopicMessageReference::new(topic(), MessageId::new(8).expect("msg")),
            confirmed_at: at,
            actor_id: 202,
            chat_id: -1001,
            observed_at: at,
        }
    }

    pub fn _unused_membership() -> MembershipStatus {
        MembershipStatus::Approved
    }
}

#[test]
fn workflow_handler_success() {
    let event = workflow_fixtures::pending_event(8);
    let result = process_workflow_action(&event).expect("consume succeeds");
    assert_eq!(result.owner, 101);
    assert!(result.workflow_revision > event.workflow.revision().get());
    assert_eq!(result.mutation_target_hex.len(), 64);
}

#[test]
fn workflow_handler_terminal_failure_wrong_action() {
    use application::resumable_confirmation::{
        IssueConfirmationRequest, ResumableConfirmationService,
    };
    use domain::confirmation::ConfirmationRecord;
    use domain::workflow::WaitDeadline;
    let svc = ResumableConfirmationService;
    let wf = workflow_fixtures::drafting_completed();
    let p = workflow_fixtures::preview();
    let outcome = svc
        .issue_confirmation(
            &wf,
            IssueConfirmationRequest {
                preview: &p,
                action: ConfirmationAction::StartSheetOrDocWrite,
                target_label: "sheet-write",
                confirmation_id: domain::identity::ConfirmationId::new("confirmation-sheet-int")
                    .expect("cid"),
                actor: workflow_fixtures::participant(202),
                source_message: domain::identity::MessageId::new(7).expect("msg"),
                deadline: WaitDeadline::at(workflow_fixtures::time(43_200)),
                timestamp: workflow_fixtures::time(7),
            },
        )
        .expect("issue");
    let event = WorkflowActionEvent {
        schema_version: EVENT_SCHEMA_VERSION.to_owned(),
        confirmation: ConfirmationRecord::from(outcome.confirmation),
        workflow: outcome.transition.workflow,
        preview: p,
        target_label: "sheet-write".to_owned(),
        source: domain::confirmation::TopicMessageReference::new(
            workflow_fixtures::topic(),
            domain::identity::MessageId::new(8).expect("msg"),
        ),
        confirmed_at: 8,
        actor_id: 202,
        chat_id: -1001,
        observed_at: 8,
    };
    let err = process_workflow_action(&event).expect_err("wrong action is terminal");
    assert!(matches!(err, WorkflowActionError::WrongAction));
}

// ── Delivery handler fixtures ─────────────────────────────────────────

mod delivery_fixtures {
    use super::*;
    use application::artifact_delivery::{DeliveryError, DriveDeliveryError};
    use application::external_operation::{ExecutionOutcome, ExternalResourceId};
    use application::ports::PresignedObjectLink;
    use application::repositories::ConditionalWriteOutcome;
    use domain::idempotency::{IdempotencyKey, OperationKind, OperationTargetFingerprint};
    use domain::identity::{ChatId, MessageThreadId, TopicSessionId, WorkflowId};
    use domain::workflow::WorkflowRevision;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    pub fn valid_event() -> DeliveryEvent {
        let pdf_bytes = b"%PDF-1.4 integration test" as &[u8];
        use sha2::Digest;
        let hash: [u8; 32] = sha2::Sha256::digest(pdf_bytes).into();
        let workflow_id = WorkflowId::new("wf-handler-int-delivery").expect("wid");
        let storage_key = application::ports::StorageKey::new(format!(
            "artifacts/{}/obj-1",
            workflow_id.as_str()
        ))
        .expect("key");
        let target = OperationTargetFingerprint::new([4u8; 32]);
        DeliveryEvent {
            schema_version: EVENT_SCHEMA_VERSION.to_owned(),
            artifact: workflow_actions_functions::delivery::ArtifactDto {
                workflow_id: workflow_id.as_str().to_owned(),
                object_id: "obj-1".to_owned(),
                storage_key: storage_key.as_str().to_owned(),
                byte_length: pdf_bytes.len() as u64,
                sha256_hex: hex::encode(hash),
                media_type: "application/pdf".to_owned(),
                created_at: 1_700_000_000,
            },
            pdf_bytes_base64: BASE64.encode(pdf_bytes),
            drive_key: IdempotencyKey::new(
                workflow_id.clone(),
                WorkflowRevision::new(9),
                OperationKind::GoogleWrite,
                target,
            ),
            telegram_key: IdempotencyKey::new(
                workflow_id,
                WorkflowRevision::new(9),
                OperationKind::TelegramSend,
                target,
            ),
            destination_name: "quote-int.pdf".to_owned(),
            topic: TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77).expect("tid")),
            retry_policy: workflow_actions_functions::delivery::RetryPolicyDto {
                max_attempts: 3,
                base_delay_ms: 500,
                maximum_delay_ms: 10_000,
                jitter_basis_points: 2_000,
            },
            topic_message: "Your quotation is ready: {link}".to_owned(),
        }
    }

    pub struct FakeRunner {
        pub result: Result<application::artifact_delivery::DeliveryResult, DeliveryError>,
        pub calls: Arc<AtomicU32>,
    }

    impl DeliveryRunner for FakeRunner {
        async fn run(
            &self,
            _request: application::artifact_delivery::DeliveryRequest,
        ) -> Result<application::artifact_delivery::DeliveryResult, DeliveryError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.result {
                Ok(r) => Ok(clone_result(r)),
                Err(e) => Err(e.clone()),
            }
        }
    }

    fn clone_result(
        r: &application::artifact_delivery::DeliveryResult,
    ) -> application::artifact_delivery::DeliveryResult {
        let link = PresignedObjectLink::new("https://s3.example.com/presigned/int").expect("link");
        application::artifact_delivery::DeliveryResult {
            s3_outcome: r.s3_outcome,
            drive_resource_id: r.drive_resource_id.clone(),
            presigned_link: link,
            telegram_outcome: r.telegram_outcome.clone(),
        }
    }

    pub fn accepted_result() -> application::artifact_delivery::DeliveryResult {
        application::artifact_delivery::DeliveryResult {
            s3_outcome: ConditionalWriteOutcome::Committed,
            drive_resource_id: Some(ExternalResourceId::new("drive-int-abc").expect("id")),
            presigned_link: PresignedObjectLink::new("https://s3.example.com/presigned/int")
                .expect("link"),
            telegram_outcome: ExecutionOutcome::Accepted {
                attempt: domain::retry::AttemptNumber::new(1).expect("a"),
                resource_id: None,
            },
        }
    }

    pub fn retryable_drive_error() -> DeliveryError {
        DeliveryError::Drive(DriveDeliveryError::Exhausted)
    }

    pub fn terminal_telegram_error() -> DeliveryError {
        DeliveryError::Drive(DriveDeliveryError::Terminal)
    }

    // Keep ExecutionOutcome clone-reachable for clone_result.
    #[allow(dead_code)]
    pub fn _ensure_clone() {
        let _ = ExecutionOutcome::Exhausted {
            completed_attempts: vec![],
        };
    }
}

#[tokio::test]
async fn delivery_handler_success() {
    let event = delivery_fixtures::valid_event();
    let calls = Arc::new(AtomicU32::new(0));
    let runner = delivery_fixtures::FakeRunner {
        result: Ok(delivery_fixtures::accepted_result()),
        calls: Arc::clone(&calls),
    };
    let dto: DeliveryResultDto = process_delivery(event, &runner).await.expect("ok");
    assert_eq!(dto.s3_outcome, "committed");
    assert_eq!(dto.drive_resource_id.as_deref(), Some("drive-int-abc"));
    assert_eq!(dto.telegram_outcome, "accepted");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn delivery_handler_retryable_failure() {
    let event = delivery_fixtures::valid_event();
    let runner = delivery_fixtures::FakeRunner {
        result: Err(delivery_fixtures::retryable_drive_error()),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let err = process_delivery(event, &runner)
        .await
        .expect_err("retryable failure");
    assert!(matches!(
        err,
        DeliveryProcessError::Delivery(DeliveryError::Drive(DriveDeliveryError::Exhausted))
    ));
}

#[tokio::test]
async fn delivery_handler_terminal_failure() {
    let event = delivery_fixtures::valid_event();
    let runner = delivery_fixtures::FakeRunner {
        result: Err(delivery_fixtures::terminal_telegram_error()),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let err = process_delivery(event, &runner)
        .await
        .expect_err("terminal failure");
    assert!(matches!(
        err,
        DeliveryProcessError::Delivery(DeliveryError::Drive(DriveDeliveryError::Terminal))
    ));
}

#[tokio::test]
async fn delivery_handler_rejects_malformed_event_before_runner() {
    let mut event = delivery_fixtures::valid_event();
    event.schema_version = "novus.workflow-actions.v0".to_owned();
    let runner = delivery_fixtures::FakeRunner {
        result: Ok(delivery_fixtures::accepted_result()),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let err = process_delivery(event, &runner)
        .await
        .expect_err("schema mismatch");
    assert!(matches!(
        err,
        DeliveryProcessError::Event(DeliveryEventError::SchemaVersionMismatch)
    ));
    assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}
