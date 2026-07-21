//! Integration tests for the confirmed existing-file modification flow
//! (Task 35 application layer).
//!
//! These tests prove the Task 35 acceptance criteria at the application
//! orchestration layer:
//! - A consumed confirmation binds to exactly one operation key whose
//!   mutation-target fingerprint matches the confirmation.
//! - Fault injection (replay after an accepted outcome) does not invoke the
//!   provider a second time — the durable journal is final.
//! - A stale (already-consumed) confirmation is rejected by the domain
//!   consume step before any provider or repository write.
//! - A duplicate `consume_and_prepare_operation` returns `Conflict`, so no
//!   second execution occurs.
//!
//! The provider is a mock closure; the google adapter client mapping is
//! covered by `crates/google/tests/existing_file_contracts.rs`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::future::ready;
use std::sync::{Arc, Mutex};

use application::existing_files::{ExistingFileFlowError, ExistingFileOperationService};
use application::external_operation::{
    AttemptClaim, BackoffWait, BeginAttemptOutcome, CompletedAttempt, ExecutionError,
    ExecutionOutcome, ExternalOperationExecutor, ExternalResourceId, JitterSource,
    JournalPreparation, JournalState, OperationJournal, ProviderOutcome,
};
use application::repositories::{
    ConditionalWriteOutcome, ConfirmationRepository, ConsumeAndPrepareRequest,
};
use application::resumable_confirmation::{
    IssueConfirmationRequest, Preview, PreviewLineItem, ResumableConfirmationService,
};
use domain::authorization::{LiveMembershipEvidence, MembershipStatus, authorize_participant};
use domain::confirmation::{
    ConfirmationAction, ConfirmationConsumption, ConfirmationError, ConfirmationRecord,
    ConfirmationStatus, TopicMessageReference,
};
use domain::idempotency::IdempotencyKey;
use domain::identity::{
    ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
};
use domain::retry::{JitterSample, RetryPolicy};
use domain::transition::{TransitionRequest, WorkflowTransition};
use domain::workflow::{WaitDeadline, Workflow, WorkflowTimestamp};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct ZeroJitter;

impl ZeroJitter {
    fn new() -> Self {
        Self
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum JournalEvent {
    Prepared,
    BeginAttemptSucceeded(&'static str),
    AttemptCompleted,
    ExhaustedRecorded,
}

#[derive(Debug, Default)]
struct FakeJournal {
    completed: Mutex<Vec<CompletedAttempt>>,
    final_outcome: Mutex<Option<ExecutionOutcome>>,
    events: Mutex<Vec<JournalEvent>>,
}

impl FakeJournal {
    fn snapshot(&self) -> Vec<JournalEvent> {
        self.events.lock().expect("journal events").clone()
    }
}

#[derive(Debug, Clone)]
struct SharedJournal(Arc<FakeJournal>);

impl OperationJournal for SharedJournal {
    type Error = FakeError;

    async fn prepare(&self, _key: &IdempotencyKey) -> Result<JournalPreparation, Self::Error> {
        self.0
            .events
            .lock()
            .expect("events")
            .push(JournalEvent::Prepared);
        let final_outcome = self.0.final_outcome.lock().expect("outcome").clone();
        if let Some(outcome) = final_outcome {
            return Ok(JournalPreparation::new(JournalState::Final(outcome)));
        }
        let completed = self.0.completed.lock().expect("completed").clone();
        Ok(JournalPreparation::new(JournalState::Ready {
            completed_attempts: completed,
        }))
    }

    async fn begin_attempt(
        &self,
        _key: &IdempotencyKey,
        attempt: domain::retry::AttemptNumber,
    ) -> Result<BeginAttemptOutcome, Self::Error> {
        self.0
            .events
            .lock()
            .expect("events")
            .push(JournalEvent::BeginAttemptSucceeded("claim"));
        Ok(BeginAttemptOutcome::Acquired(
            AttemptClaim::new(format!("claim-{}", attempt.get())).expect("claim is valid"),
        ))
    }

    async fn complete_attempt(
        &self,
        _key: &IdempotencyKey,
        _claim: &AttemptClaim,
        attempt: &CompletedAttempt,
    ) -> Result<(), Self::Error> {
        self.0
            .completed
            .lock()
            .expect("completed")
            .push(attempt.clone());
        self.0
            .events
            .lock()
            .expect("events")
            .push(JournalEvent::AttemptCompleted);
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
                completed_attempts: self.0.completed.lock().expect("completed").clone(),
            }),
            ProviderOutcome::RetryableFailure(_) => None,
        };
        if let Some(outcome) = finalized {
            *self.0.final_outcome.lock().expect("outcome") = Some(outcome);
        }
        Ok(())
    }

    async fn record_exhausted(
        &self,
        _key: &IdempotencyKey,
        _completed_attempts: &[CompletedAttempt],
    ) -> Result<(), Self::Error> {
        self.0
            .events
            .lock()
            .expect("events")
            .push(JournalEvent::ExhaustedRecorded);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct FakeError;

impl Display for FakeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("fake journal error")
    }
}

impl std::error::Error for FakeError {}

/// Confirmation repository that succeeds once and then conflicts on duplicate
/// consume for the same workflow id, mirroring the real conditional write.
#[derive(Debug, Default)]
struct FakeConfirmationRepo {
    consumed: Mutex<HashMap<domain::identity::WorkflowId, ()>>,
}

impl ConfirmationRepository for FakeConfirmationRepo {
    type Error = FakeError;

    async fn load(
        &self,
        _workflow_id: &WorkflowId,
        _confirmation_id: &ConfirmationId,
    ) -> Result<Option<ConfirmationRecord>, Self::Error> {
        Ok(None)
    }

    async fn issue(
        &self,
        _outcome: &domain::confirmation::ConfirmationIssueOutcome,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        Ok(ConditionalWriteOutcome::Committed)
    }

    async fn consume_and_prepare_operation(
        &self,
        request: &ConsumeAndPrepareRequest,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        let workflow_id = request.operation_key().workflow_id().clone();
        let mut consumed = self.consumed.lock().expect("consumed map");
        if consumed.contains_key(&workflow_id) {
            return Ok(ConditionalWriteOutcome::Conflict);
        }
        consumed.insert(workflow_id, ());
        Ok(ConditionalWriteOutcome::Committed)
    }

    async fn correct(
        &self,
        _outcome: &domain::confirmation::ConfirmationCorrectionOutcome,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        Ok(ConditionalWriteOutcome::Committed)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        WorkflowId::new("wf-existing-file").expect("workflow id"),
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

fn authorized_participant(at: u64) -> domain::authorization::AuthorizedParticipant {
    authorize_participant(
        topic().chat_id(),
        participant(202),
        &LiveMembershipEvidence::new(
            topic().chat_id(),
            participant(202),
            MembershipStatus::Approved,
            time(at),
        ),
        time(at),
    )
    .expect("authorized participant")
}

fn preview() -> Preview {
    Preview::new(
        "INR".to_string(),
        vec![PreviewLineItem {
            description: "Consulting".to_string(),
            quantity: "2".to_string(),
            unit_price_micro_inr: 50_000_000,
        }],
        vec!["standard rate".to_string()],
        100_000_000,
        1800,
        18_000_000,
        118_000_000,
    )
    .expect("valid preview")
}

fn policy() -> RetryPolicy {
    RetryPolicy::new(3, 500, 10_000, 2_000).expect("valid policy")
}

fn accepted_outcome() -> ProviderOutcome {
    ProviderOutcome::Accepted {
        resource_id: Some(ExternalResourceId::new("sheet-1A-update").expect("resource id")),
    }
}

/// Build a pending confirmation record and the workflow waiting for it.
fn waiting_for_confirmation() -> (Workflow, Preview, ConfirmationRecord) {
    let svc = ResumableConfirmationService;
    let wf = drafting_completed();
    let p = preview();
    let outcome = svc
        .issue_confirmation(
            &wf,
            IssueConfirmationRequest {
                preview: &p,
                action: ConfirmationAction::StartSheetOrDocWrite,
                target_label: "existing-sheet-write",
                confirmation_id: ConfirmationId::new("confirmation-existing").expect("conf id"),
                actor: participant(202),
                source_message: MessageId::new(7).expect("message id"),
                deadline: WaitDeadline::at(time(43_200)),
                timestamp: time(7),
            },
        )
        .expect("issue confirmation");
    let record = ConfirmationRecord::from(outcome.confirmation);
    (outcome.transition.workflow, p, record)
}

/// Consume the pending confirmation and return the consumption.
fn consume_fresh(
    record: &ConfirmationRecord,
    wf: &Workflow,
    preview: &Preview,
    at: u64,
) -> ConfirmationConsumption {
    let svc = ResumableConfirmationService;
    let authz = authorized_participant(at).for_workflow(wf);
    svc.consume_confirmation(
        record,
        wf,
        &authz,
        preview,
        "existing-sheet-write",
        TopicMessageReference::new(
            topic(),
            MessageId::new(i64::try_from(at).unwrap()).expect("msg"),
        ),
        time(at),
    )
    .expect("consume confirmation")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prepare_binds_consumed_confirmation_to_google_write_key() {
    let (wf, preview, record) = waiting_for_confirmation();
    let consumption = consume_fresh(&record, &wf, &preview, 8);
    assert_eq!(
        consumption.confirmation().status(),
        ConfirmationStatus::Consumed
    );

    let prepared =
        ExistingFileOperationService::prepare(consumption, policy()).expect("prepare mutation");
    let key = prepared.operation_key();
    assert_eq!(
        key.operation_kind(),
        domain::idempotency::OperationKind::GoogleWrite
    );
    // The key binds the resulting workflow revision, not the pending one.
    assert_eq!(
        key.workflow_revision(),
        prepared
            .consume_request()
            .consumption()
            .transition()
            .workflow
            .revision()
    );
    // The target fingerprint matches the confirmation mutation target.
    let confirmation_target = prepared
        .consume_request()
        .consumption()
        .confirmation()
        .mutation_target();
    assert_eq!(key.target().as_bytes(), confirmation_target.as_bytes());
}

#[tokio::test]
async fn stale_confirmation_rejected_before_any_provider_write() {
    let (wf, preview, record) = waiting_for_confirmation();
    // First consume succeeds and produces a Consumed record.
    let consumption = consume_fresh(&record, &wf, &preview, 8);
    let consumed_record = consumption.confirmation().clone();
    assert_eq!(consumed_record.status(), ConfirmationStatus::Consumed);

    // Re-consuming the now-Consumed record is rejected by the domain before
    // any repository write or provider invocation. `consume` checks
    // AlreadyConsumed before any other validation.
    let svc = ResumableConfirmationService;
    let authz = authorized_participant(9).for_workflow(&wf);
    let source = TopicMessageReference::new(topic(), MessageId::new(9).expect("msg"));
    let result = svc.consume_confirmation(
        &consumed_record,
        &wf,
        &authz,
        &preview,
        "existing-sheet-write",
        source,
        time(9),
    );
    assert!(matches!(
        result,
        Err(
            application::resumable_confirmation::ResumableError::Confirmation(
                ConfirmationError::AlreadyConsumed
            )
        )
    ));
    // No provider closure or repository call is ever made — the domain
    // rejected before ExistingFileOperationService::prepare is reached.
}

#[tokio::test]
async fn duplicate_consume_and_prepare_returns_conflict() {
    let (wf, preview, record) = waiting_for_confirmation();
    let consumption = consume_fresh(&record, &wf, &preview, 8);
    let prepared =
        ExistingFileOperationService::prepare(consumption, policy()).expect("prepare mutation");
    let repo = FakeConfirmationRepo::default();

    let first = repo
        .consume_and_prepare_operation(prepared.consume_request())
        .await
        .expect("first repo call");
    assert_eq!(first, ConditionalWriteOutcome::Committed);

    let second = repo
        .consume_and_prepare_operation(prepared.consume_request())
        .await
        .expect("second repo call");
    assert_eq!(second, ConditionalWriteOutcome::Conflict);
}

#[tokio::test]
async fn fault_injection_replay_does_not_invoke_provider_again() {
    let (wf, preview, record) = waiting_for_confirmation();
    let consumption = consume_fresh(&record, &wf, &preview, 8);
    let prepared =
        ExistingFileOperationService::prepare(consumption, policy()).expect("prepare mutation");

    let journal = Arc::new(FakeJournal::default());
    let executor = ExternalOperationExecutor::new(
        SharedJournal(Arc::clone(&journal)),
        ZeroJitter::new(),
        NoopWait,
    );
    let calls = Cell::new(0u8);
    let outcome = accepted_outcome();
    let provider = || {
        calls.set(calls.get().saturating_add(1));
        ready(outcome.clone())
    };

    let key = prepared.operation_key().clone();
    let first = executor
        .execute(&key, prepared.retry_policy(), provider)
        .await;
    let first = match first {
        Ok(outcome) => outcome,
        Err(ExecutionError::RaceLost) => panic!("unexpected race loss"),
        Err(_) => panic!("executor failed"),
    };
    assert!(matches!(
        first,
        ExecutionOutcome::Accepted { attempt, .. } if attempt.get() == 1
    ));
    assert_eq!(calls.get(), 1);

    // Replay: the journal is Final, so the provider must NOT be invoked again.
    let replay_outcome = accepted_outcome();
    let replay_provider = || {
        calls.set(calls.get().saturating_add(1));
        ready(replay_outcome.clone())
    };
    let replay = executor
        .execute(&key, prepared.retry_policy(), replay_provider)
        .await
        .expect("replay executes");
    assert_eq!(calls.get(), 1, "provider must not be invoked on replay");
    assert_eq!(first, replay);

    let events = journal.snapshot();
    // Two prepare events (one per execute), only one begin/complete pair.
    let begins = events
        .iter()
        .filter(|event| matches!(event, JournalEvent::BeginAttemptSucceeded(_)))
        .count();
    assert_eq!(begins, 1, "only one physical attempt must be begun");
}

#[tokio::test]
async fn wrong_action_kind_pair_rejected_at_prepare() {
    // A consumed confirmation with a non-GoogleWrite action cannot bind to a
    // GoogleWrite operation key. ConsumeAndPrepareRequest::new enforces the
    // action-kind pair. Use StartDirectPdfGeneration to trigger the mismatch.
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
                confirmation_id: ConfirmationId::new("confirmation-pdf").expect("conf id"),
                actor: participant(202),
                source_message: MessageId::new(7).expect("message id"),
                deadline: WaitDeadline::at(time(43_200)),
                timestamp: time(7),
            },
        )
        .expect("issue confirmation");
    let record = ConfirmationRecord::from(outcome.confirmation);
    let authz = authorized_participant(8).for_workflow(&outcome.transition.workflow);
    let consumption = svc
        .consume_confirmation(
            &record,
            &outcome.transition.workflow,
            &authz,
            &p,
            "pdf-render",
            TopicMessageReference::new(topic(), MessageId::new(8).expect("msg")),
            time(8),
        )
        .expect("consume");

    // ExistingFileOperationService always builds a GoogleWrite key; pairing
    // validation in ConsumeAndPrepareRequest must reject StartDirectPdfGeneration.
    let result = ExistingFileOperationService::prepare(consumption, policy());
    assert!(matches!(
        result,
        Err(ExistingFileFlowError::Binding(
            application::repositories::ConsumeAndPrepareError::InvalidActionKindPair { .. }
        ))
    ));
}
