use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt::{Display, Formatter};
use std::future::{Future, ready};
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use application::external_operation::{
    AttemptClaim, BackoffWait, BeginAttemptOutcome, CompletedAttempt, ExecutionError,
    ExecutionOutcome, ExternalOperationExecutor, ExternalResourceId, FailureCode, JitterSource,
    JournalPreparation, JournalState, OperationFailure, OperationJournal, ProviderOutcome,
    SanitizedSummary, SanitizedValueError,
};
use domain::identity::WorkflowId;
use domain::{
    AttemptNumber, IdempotencyKey, JitterSample, OperationKind, OperationTargetFingerprint,
    RetryPolicy, WorkflowRevision,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct FakeError(&'static str);

impl Display for FakeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for FakeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JournalEvent {
    Prepared,
    BeginAttemptSucceeded(String),
    AttemptCompleted(AttemptNumber),
    Exhausted,
}

#[derive(Debug, Default)]
struct JournalStateData {
    completed: Vec<CompletedAttempt>,
    in_progress: Option<(AttemptNumber, String)>,
    final_outcome: Option<ExecutionOutcome>,
    events: Vec<JournalEvent>,
    lose_next_begin: bool,
    fail_next_completion: bool,
}

#[derive(Debug, Default)]
struct FakeJournal {
    state: Mutex<JournalStateData>,
}

impl FakeJournal {
    fn snapshot(&self) -> Result<JournalSnapshot, FakeError> {
        let state = self
            .state
            .lock()
            .map_err(|_| FakeError("poisoned journal"))?;
        Ok(JournalSnapshot {
            completed: state.completed.clone(),
            events: state.events.clone(),
        })
    }

    fn inject_completed(&self, completed: Vec<CompletedAttempt>) -> Result<(), FakeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| FakeError("poisoned journal"))?;
        state.completed = completed;
        Ok(())
    }

    fn inject_in_progress(&self, attempt: AttemptNumber) -> Result<(), FakeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| FakeError("poisoned journal"))?;
        state.in_progress = Some((attempt, format!("claim-{}", attempt.get())));
        Ok(())
    }

    fn lose_next_begin(&self) -> Result<(), FakeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| FakeError("poisoned journal"))?;
        state.lose_next_begin = true;
        Ok(())
    }

    fn fail_next_completion(&self) -> Result<(), FakeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| FakeError("poisoned journal"))?;
        state.fail_next_completion = true;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct JournalSnapshot {
    completed: Vec<CompletedAttempt>,
    events: Vec<JournalEvent>,
}

#[derive(Debug, Clone)]
struct SharedJournal(Arc<FakeJournal>);

impl OperationJournal for SharedJournal {
    type Error = FakeError;

    async fn prepare(&self, _key: &IdempotencyKey) -> Result<JournalPreparation, Self::Error> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| FakeError("poisoned journal"))?;
        state.events.push(JournalEvent::Prepared);
        if let Some(outcome) = &state.final_outcome {
            return Ok(JournalPreparation::new(JournalState::Final(
                outcome.clone(),
            )));
        }
        if let Some((attempt, _claim)) = &state.in_progress {
            return Ok(JournalPreparation::new(JournalState::InProgress {
                attempt: *attempt,
                completed_attempts: state.completed.clone(),
            }));
        }
        Ok(JournalPreparation::new(JournalState::Ready {
            completed_attempts: state.completed.clone(),
        }))
    }

    async fn begin_attempt(
        &self,
        _key: &IdempotencyKey,
        expected_attempt: AttemptNumber,
    ) -> Result<BeginAttemptOutcome, Self::Error> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| FakeError("poisoned journal"))?;
        if state.lose_next_begin {
            state.lose_next_begin = false;
            return Ok(BeginAttemptOutcome::RaceLost);
        }
        if state.final_outcome.is_some() {
            return Ok(BeginAttemptOutcome::RaceLost);
        }
        if state.in_progress.is_some() {
            return Ok(BeginAttemptOutcome::RaceLost);
        }
        let next_attempt = state
            .completed
            .last()
            .map_or(1_u8, |last| last.attempt().get().saturating_add(1));
        let attempt =
            AttemptNumber::new(next_attempt).map_err(|_| FakeError("invalid attempt number"))?;
        if attempt != expected_attempt {
            return Ok(BeginAttemptOutcome::RaceLost);
        }
        let claim = format!("claim-{next_attempt}");
        state.in_progress = Some((attempt, claim.clone()));
        state
            .events
            .push(JournalEvent::BeginAttemptSucceeded(claim.clone()));
        Ok(BeginAttemptOutcome::Acquired(
            AttemptClaim::new(claim).map_err(|_| FakeError("invalid claim"))?,
        ))
    }

    async fn complete_attempt(
        &self,
        _key: &IdempotencyKey,
        claim: &AttemptClaim,
        attempt: &CompletedAttempt,
    ) -> Result<(), Self::Error> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| FakeError("poisoned journal"))?;
        if state.fail_next_completion {
            state.fail_next_completion = false;
            return Err(FakeError("completion write failed"));
        }
        let stored_claim = state.in_progress.as_ref().map(|(_, stored)| stored.clone());
        match stored_claim {
            Some(stored) if stored == claim.as_str() => {}
            _ => return Err(FakeError("claim mismatch")),
        }
        state.in_progress = None;
        state.completed.push(attempt.clone());
        state
            .events
            .push(JournalEvent::AttemptCompleted(attempt.attempt()));
        state.final_outcome = match attempt.outcome() {
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
                completed_attempts: state.completed.clone(),
            }),
            ProviderOutcome::RetryableFailure(_) => None,
        };
        Ok(())
    }

    async fn record_exhausted(
        &self,
        _key: &IdempotencyKey,
        completed_attempts: &[CompletedAttempt],
    ) -> Result<(), Self::Error> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| FakeError("poisoned journal"))?;
        let outcome = ExecutionOutcome::Exhausted {
            completed_attempts: completed_attempts.to_vec(),
        };
        state.final_outcome = Some(outcome);
        state.events.push(JournalEvent::Exhausted);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct ZeroJitter(JitterSample);

impl ZeroJitter {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self(JitterSample::new(0)?))
    }
}

impl JitterSource for ZeroJitter {
    fn sample(&self) -> JitterSample {
        self.0
    }
}

#[derive(Debug, Default)]
struct RecordingWait {
    delays: Mutex<Vec<u32>>,
}

impl RecordingWait {
    fn delays(&self) -> Result<Vec<u32>, FakeError> {
        Ok(self
            .delays
            .lock()
            .map_err(|_| FakeError("poisoned wait"))?
            .clone())
    }
}

#[derive(Debug, Clone)]
struct SharedWait(Arc<RecordingWait>);

impl BackoffWait for SharedWait {
    type Error = FakeError;

    async fn wait(&self, delay_ms: u32) -> Result<(), Self::Error> {
        self.0
            .delays
            .lock()
            .map_err(|_| FakeError("poisoned wait"))?
            .push(delay_ms);
        Ok(())
    }
}

fn key() -> Result<IdempotencyKey, Box<dyn std::error::Error>> {
    Ok(IdempotencyKey::new(
        WorkflowId::new("workflow-operation")?,
        WorkflowRevision::new(8),
        OperationKind::GoogleWrite,
        OperationTargetFingerprint::new([7; 32]),
    ))
}

fn policy() -> Result<RetryPolicy, Box<dyn std::error::Error>> {
    Ok(RetryPolicy::new(3, 500, 10_000, 2_000)?)
}

fn failure(code: &str) -> Result<OperationFailure, Box<dyn std::error::Error>> {
    Ok(OperationFailure::new(
        FailureCode::new(code)?,
        SanitizedSummary::new(format!("sanitized {code}"))?,
    ))
}

fn accepted() -> Result<ProviderOutcome, Box<dyn std::error::Error>> {
    Ok(ProviderOutcome::Accepted {
        resource_id: Some(ExternalResourceId::new("resource-17")?),
    })
}

fn executor(
    journal: Arc<FakeJournal>,
    wait: Arc<RecordingWait>,
) -> Result<
    ExternalOperationExecutor<SharedJournal, ZeroJitter, SharedWait>,
    Box<dyn std::error::Error>,
> {
    Ok(ExternalOperationExecutor::new(
        SharedJournal(journal),
        ZeroJitter::new()?,
        SharedWait(wait),
    ))
}

#[test]
fn accepted_operation_is_persisted_before_replay_and_invoked_once()
-> Result<(), Box<dyn std::error::Error>> {
    let journal = Arc::new(FakeJournal::default());
    let wait = Arc::new(RecordingWait::default());
    let executor = executor(Arc::clone(&journal), wait)?;
    let calls = Cell::new(0_u8);
    let provider_result = accepted()?;
    let provider = || {
        calls.set(calls.get().saturating_add(1));
        ready(provider_result.clone())
    };

    let first = block_on(executor.execute(&key()?, policy()?, provider))?;
    let replay = block_on(executor.execute(&key()?, policy()?, provider))?;

    assert_eq!(calls.get(), 1);
    assert_eq!(first, replay);
    assert!(matches!(first, ExecutionOutcome::Accepted { attempt, .. } if attempt.get() == 1));
    assert_eq!(
        journal.snapshot()?.events,
        vec![
            JournalEvent::Prepared,
            JournalEvent::BeginAttemptSucceeded("claim-1".to_owned()),
            JournalEvent::AttemptCompleted(AttemptNumber::new(1)?),
            JournalEvent::Prepared,
        ]
    );
    Ok(())
}

#[test]
fn retryable_failure_waits_then_succeeds_with_durable_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    let journal = Arc::new(FakeJournal::default());
    let wait = Arc::new(RecordingWait::default());
    let executor = executor(Arc::clone(&journal), Arc::clone(&wait))?;
    let outcomes = RefCell::new(VecDeque::from([
        ProviderOutcome::RetryableFailure(failure("temporary")?),
        accepted()?,
    ]));
    let fallback = ProviderOutcome::TerminalFailure(failure("missing_fixture")?);
    let calls = Cell::new(0_u8);
    let provider = || {
        calls.set(calls.get().saturating_add(1));
        ready(
            outcomes
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| fallback.clone()),
        )
    };

    let result = block_on(executor.execute(&key()?, policy()?, provider))?;

    assert!(matches!(result, ExecutionOutcome::Accepted { attempt, .. } if attempt.get() == 2));
    assert_eq!(calls.get(), 2);
    assert_eq!(wait.delays()?, vec![500]);
    let snapshot = journal.snapshot()?;
    assert_eq!(snapshot.completed.len(), 2);
    assert_eq!(
        snapshot
            .completed
            .last()
            .and_then(CompletedAttempt::delay_before_ms),
        Some(500)
    );
    Ok(())
}

#[test]
fn retryable_failures_exhaust_at_three_and_replay_without_a_fourth_call()
-> Result<(), Box<dyn std::error::Error>> {
    let journal = Arc::new(FakeJournal::default());
    let wait = Arc::new(RecordingWait::default());
    let executor = executor(Arc::clone(&journal), Arc::clone(&wait))?;
    let calls = Cell::new(0_u8);
    let retryable = ProviderOutcome::RetryableFailure(failure("temporary")?);
    let provider = || {
        calls.set(calls.get().saturating_add(1));
        ready(retryable.clone())
    };

    let first = block_on(executor.execute(&key()?, policy()?, provider))?;
    let replay = block_on(executor.execute(&key()?, policy()?, provider))?;

    assert_eq!(calls.get(), 3);
    assert_eq!(wait.delays()?, vec![500, 1_000]);
    assert_eq!(first, replay);
    assert!(matches!(
        first,
        ExecutionOutcome::Exhausted { ref completed_attempts }
            if completed_attempts.len() == 3
    ));
    assert_eq!(
        journal
            .snapshot()?
            .events
            .iter()
            .filter(|event| matches!(event, JournalEvent::BeginAttemptSucceeded(_)))
            .count(),
        3
    );
    Ok(())
}

#[test]
fn ambiguous_and_terminal_outcomes_never_retry() -> Result<(), Box<dyn std::error::Error>> {
    for outcome in [
        ProviderOutcome::Ambiguous(failure("unknown_acceptance")?),
        ProviderOutcome::TerminalFailure(failure("permanent_rejection")?),
    ] {
        let journal = Arc::new(FakeJournal::default());
        let executor = executor(journal, Arc::new(RecordingWait::default()))?;
        let calls = Cell::new(0_u8);
        let provider = || {
            calls.set(calls.get().saturating_add(1));
            ready(outcome.clone())
        };

        let first = block_on(executor.execute(&key()?, policy()?, provider))?;
        let replay = block_on(executor.execute(&key()?, policy()?, provider))?;

        assert_eq!(calls.get(), 1);
        assert_eq!(first, replay);
        assert!(matches!(
            first,
            ExecutionOutcome::ManualReview { .. } | ExecutionOutcome::TerminalFailure { .. }
        ));
    }
    Ok(())
}

#[test]
fn started_but_unfinished_attempt_requires_manual_review_without_invocation()
-> Result<(), Box<dyn std::error::Error>> {
    let journal = Arc::new(FakeJournal::default());
    journal.inject_in_progress(AttemptNumber::new(2)?)?;
    let executor = executor(journal, Arc::new(RecordingWait::default()))?;
    let calls = Cell::new(0_u8);
    let provider_result = accepted()?;
    let result = block_on(executor.execute(&key()?, policy()?, || {
        calls.set(calls.get().saturating_add(1));
        ready(provider_result.clone())
    }))?;

    assert_eq!(calls.get(), 0);
    assert!(matches!(
        result,
        ExecutionOutcome::ManualReview { attempt, .. } if attempt.get() == 2
    ));
    Ok(())
}

#[test]
fn persistence_failure_after_invocation_fails_closed_on_reentry()
-> Result<(), Box<dyn std::error::Error>> {
    let journal = Arc::new(FakeJournal::default());
    journal.fail_next_completion()?;
    let executor = executor(journal, Arc::new(RecordingWait::default()))?;
    let calls = Cell::new(0_u8);
    let provider_result = accepted()?;
    let provider = || {
        calls.set(calls.get().saturating_add(1));
        ready(provider_result.clone())
    };

    let first = block_on(executor.execute(&key()?, policy()?, provider));
    let second = block_on(executor.execute(&key()?, policy()?, provider))?;

    assert!(matches!(
        first,
        Err(ExecutionError::PersistenceAfterInvocation(FakeError(
            "completion write failed"
        )))
    ));
    assert_eq!(calls.get(), 1);
    assert!(matches!(second, ExecutionOutcome::ManualReview { .. }));
    Ok(())
}

#[test]
fn begin_attempt_race_loss_returns_error_without_provider_invocation()
-> Result<(), Box<dyn std::error::Error>> {
    let journal = Arc::new(FakeJournal::default());
    journal.lose_next_begin()?;
    let executor = executor(journal, Arc::new(RecordingWait::default()))?;
    let calls = Cell::new(0_u8);
    let provider_result = accepted()?;
    let result = block_on(executor.execute(&key()?, policy()?, || {
        calls.set(calls.get().saturating_add(1));
        ready(provider_result.clone())
    }));

    assert_eq!(calls.get(), 0);
    assert!(matches!(result, Err(ExecutionError::RaceLost)));
    Ok(())
}

#[test]
fn resumed_retry_history_preserves_the_global_attempt_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let journal = Arc::new(FakeJournal::default());
    journal.inject_completed(vec![
        CompletedAttempt::new(
            AttemptNumber::new(1)?,
            ProviderOutcome::RetryableFailure(failure("temporary")?),
            None,
        ),
        CompletedAttempt::new(
            AttemptNumber::new(2)?,
            ProviderOutcome::RetryableFailure(failure("temporary")?),
            Some(500),
        ),
    ])?;
    let wait = Arc::new(RecordingWait::default());
    let executor = executor(Arc::clone(&journal), Arc::clone(&wait))?;
    let calls = Cell::new(0_u8);
    let retryable = ProviderOutcome::RetryableFailure(failure("temporary")?);

    let result = block_on(executor.execute(&key()?, policy()?, || {
        calls.set(calls.get().saturating_add(1));
        ready(retryable.clone())
    }))?;

    assert_eq!(calls.get(), 1);
    assert_eq!(wait.delays()?, vec![1_000]);
    assert!(matches!(
        result,
        ExecutionOutcome::Exhausted { ref completed_attempts }
            if completed_attempts.len() == 3
    ));
    Ok(())
}

#[test]
fn begin_attempt_on_final_journal_returns_race_lost_and_no_provider_call()
-> Result<(), Box<dyn std::error::Error>> {
    let journal = Arc::new(FakeJournal::default());
    let wait = Arc::new(RecordingWait::default());
    let executor = executor(Arc::clone(&journal), Arc::clone(&wait))?;
    let provider_result = accepted()?;
    let calls = Cell::new(0_u8);

    // First: consume the operation to set Final state
    let _first = block_on(executor.execute(&key()?, policy()?, || {
        calls.set(calls.get().saturating_add(1));
        ready(provider_result.clone())
    }))?;

    assert_eq!(calls.get(), 1);

    // Second: directly call begin_attempt on a final journal — should get RaceLost
    let shared = SharedJournal(Arc::clone(&journal));
    let begin = block_on(shared.begin_attempt(&key()?, AttemptNumber::new(1)?))?;
    assert!(matches!(begin, BeginAttemptOutcome::RaceLost));
    // Provider was never called a second time
    assert_eq!(calls.get(), 1);
    Ok(())
}

#[test]
fn claim_conditioned_completion_rejects_wrong_claim() -> Result<(), Box<dyn std::error::Error>> {
    let journal = Arc::new(FakeJournal::default());
    let shared = SharedJournal(Arc::clone(&journal));
    let key = key()?;

    // Prepare and begin to get a claim
    let preparation = block_on(shared.prepare(&key))?;
    assert!(matches!(preparation.state, JournalState::Ready { .. }));
    let begin = block_on(shared.begin_attempt(&key, AttemptNumber::new(1)?))?;
    let BeginAttemptOutcome::Acquired(claim) = begin else {
        return Err(Box::new(FakeError("expected acquired attempt claim")));
    };

    let wrong_claim = AttemptClaim::new("wrong-claim")?;
    let attempt = CompletedAttempt::new(AttemptNumber::new(1)?, accepted()?, None);
    let wrong_result = block_on(shared.complete_attempt(&key, &wrong_claim, &attempt));
    assert!(matches!(wrong_result, Err(FakeError("claim mismatch"))));

    block_on(shared.complete_attempt(&key, &claim, &attempt))?;
    let replay = block_on(shared.prepare(&key))?;
    assert!(matches!(replay.state, JournalState::Final(_)));
    Ok(())
}

#[test]
fn provider_history_values_reject_unbounded_or_control_bearing_text() {
    assert!(matches!(
        FailureCode::new("not allowed"),
        Err(SanitizedValueError::InvalidCharacters { .. })
    ));
    assert!(matches!(
        SanitizedSummary::new("line one\nsecret"),
        Err(SanitizedValueError::InvalidCharacters { .. })
    ));
    assert!(matches!(
        ExternalResourceId::new(" "),
        Err(SanitizedValueError::InvalidLength { .. })
    ));
}

#[test]
fn attempt_claim_rejects_empty_or_control_bearing_values() {
    assert!(matches!(
        AttemptClaim::new(""),
        Err(SanitizedValueError::InvalidLength { .. })
    ));
    assert!(matches!(
        AttemptClaim::new("a".repeat(257)),
        Err(SanitizedValueError::InvalidLength { .. })
    ));
    assert!(matches!(
        AttemptClaim::new("claim\nsecret"),
        Err(SanitizedValueError::InvalidCharacters { .. })
    ));
    assert!(AttemptClaim::new("claim-1").is_ok());
}

fn block_on<Output>(future: impl Future<Output = Output>) -> Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
