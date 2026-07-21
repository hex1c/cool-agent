use std::fmt::{Display, Formatter};

use domain::{
    AttemptNumber, IdempotencyKey, JitterSample, RetryDecision, RetryPolicy, RetryPolicyError,
};

const MAX_FAILURE_CODE_BYTES: usize = 64;
const MAX_SUMMARY_BYTES: usize = 256;
const MAX_RESOURCE_ID_BYTES: usize = 256;

/// Short, non-sensitive machine-readable failure code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureCode(String);

impl FailureCode {
    pub fn new(value: impl Into<String>) -> Result<Self, SanitizedValueError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_FAILURE_CODE_BYTES {
            return Err(SanitizedValueError::InvalidLength {
                field: "failure code",
                maximum_bytes: MAX_FAILURE_CODE_BYTES,
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(SanitizedValueError::InvalidCharacters {
                field: "failure code",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bounded, non-sensitive summary suitable for durable history and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedSummary(String);

impl SanitizedSummary {
    pub fn new(value: impl Into<String>) -> Result<Self, SanitizedValueError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_SUMMARY_BYTES {
            return Err(SanitizedValueError::InvalidLength {
                field: "summary",
                maximum_bytes: MAX_SUMMARY_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(SanitizedValueError::InvalidCharacters { field: "summary" });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bounded opaque provider resource identifier, never a raw payload or secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalResourceId(String);

impl ExternalResourceId {
    pub fn new(value: impl Into<String>) -> Result<Self, SanitizedValueError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_RESOURCE_ID_BYTES {
            return Err(SanitizedValueError::InvalidLength {
                field: "external resource id",
                maximum_bytes: MAX_RESOURCE_ID_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(SanitizedValueError::InvalidCharacters {
                field: "external resource id",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SanitizedValueError {
    InvalidLength {
        field: &'static str,
        maximum_bytes: usize,
    },
    InvalidCharacters {
        field: &'static str,
    },
}

impl Display for SanitizedValueError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "invalid sanitized external-operation value: {self:?}"
        )
    }
}

impl std::error::Error for SanitizedValueError {}

/// Sanitized provider failure persisted without raw request or response data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationFailure {
    code: FailureCode,
    summary: SanitizedSummary,
}

impl OperationFailure {
    pub const fn new(code: FailureCode, summary: SanitizedSummary) -> Self {
        Self { code, summary }
    }

    pub const fn code(&self) -> &FailureCode {
        &self.code
    }

    pub const fn summary(&self) -> &SanitizedSummary {
        &self.summary
    }

    fn interrupted_attempt() -> Self {
        Self {
            code: FailureCode("attempt_outcome_unknown".to_owned()),
            summary: SanitizedSummary(
                "a started attempt has no durable provider outcome".to_owned(),
            ),
        }
    }
}

/// Classification of one provider invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOutcome {
    Accepted {
        resource_id: Option<ExternalResourceId>,
    },
    RetryableFailure(OperationFailure),
    TerminalFailure(OperationFailure),
    Ambiguous(OperationFailure),
}

/// Durable record of one completed physical attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedAttempt {
    attempt: AttemptNumber,
    outcome: ProviderOutcome,
    delay_before_ms: Option<u32>,
}

impl CompletedAttempt {
    pub const fn new(
        attempt: AttemptNumber,
        outcome: ProviderOutcome,
        delay_before_ms: Option<u32>,
    ) -> Self {
        Self {
            attempt,
            outcome,
            delay_before_ms,
        }
    }

    pub const fn attempt(&self) -> AttemptNumber {
        self.attempt
    }

    pub const fn outcome(&self) -> &ProviderOutcome {
        &self.outcome
    }

    pub const fn delay_before_ms(&self) -> Option<u32> {
        self.delay_before_ms
    }
}

/// Durable final result replayed without another provider invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionOutcome {
    Accepted {
        attempt: AttemptNumber,
        resource_id: Option<ExternalResourceId>,
    },
    TerminalFailure {
        attempt: AttemptNumber,
        failure: OperationFailure,
    },
    ManualReview {
        attempt: AttemptNumber,
        failure: OperationFailure,
        completed_attempts: Vec<CompletedAttempt>,
    },
    Exhausted {
        completed_attempts: Vec<CompletedAttempt>,
    },
}

/// Result of atomically reserving a logical operation key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReservationOutcome {
    /// This caller owns the reservation and may continue after validating history.
    Acquired {
        completed_attempts: Vec<CompletedAttempt>,
    },
    /// Another invocation started an attempt whose provider result is not durable.
    AlreadyInProgress {
        started_attempt: AttemptNumber,
        completed_attempts: Vec<CompletedAttempt>,
    },
    /// The operation is final and must be replayed without provider invocation.
    AlreadyCompleted(ExecutionOutcome),
}

/// Persistence boundary for Task 17. DynamoDB details remain in Tasks 19-20.
#[allow(async_fn_in_trait)]
pub trait OperationJournal {
    type Error: Display;

    /// Atomically acquires a new or resumable reservation for one caller.
    async fn reserve(&self, key: &IdempotencyKey) -> Result<ReservationOutcome, Self::Error>;

    /// Records an in-flight attempt before any provider bytes are sent.
    async fn record_attempt_started(
        &self,
        key: &IdempotencyKey,
        attempt: AttemptNumber,
    ) -> Result<(), Self::Error>;

    /// Records the provider result. Implementations must atomically make accepted,
    /// terminal, and ambiguous outcomes final when writing this record.
    async fn record_attempt_completed(
        &self,
        key: &IdempotencyKey,
        attempt: &CompletedAttempt,
    ) -> Result<(), Self::Error>;

    /// Marks a fully retryable history as permanently exhausted.
    async fn record_exhausted(
        &self,
        key: &IdempotencyKey,
        completed_attempts: &[CompletedAttempt],
    ) -> Result<(), Self::Error>;
}

pub trait JitterSource {
    fn sample(&self) -> JitterSample;
}

#[allow(async_fn_in_trait)]
pub trait BackoffWait {
    type Error: Display;

    async fn wait(&self, delay_ms: u32) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationHistoryError {
    TooManyCompletedAttempts,
    NonContiguousAttempt { expected: u8, actual: u8 },
    NonRetryableCompletedAttempt { attempt: AttemptNumber },
    RetryPolicy(RetryPolicyError),
}

impl Display for OperationHistoryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid external-operation history: {self:?}")
    }
}

impl std::error::Error for OperationHistoryError {}

#[derive(Debug)]
pub enum ExecutionError<JournalError: Display, WaitError: Display> {
    PersistenceBeforeInvocation(JournalError),
    PersistenceAfterInvocation(JournalError),
    Wait(WaitError),
    InvalidHistory(OperationHistoryError),
}

impl<JournalError: Display, WaitError: Display> Display
    for ExecutionError<JournalError, WaitError>
{
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PersistenceBeforeInvocation(error) => {
                write!(
                    formatter,
                    "persistence failed before provider invocation: {error}"
                )
            }
            Self::PersistenceAfterInvocation(error) => write!(
                formatter,
                "persistence failed after provider invocation; manual review required: {error}"
            ),
            Self::Wait(error) => write!(formatter, "external-operation backoff failed: {error}"),
            Self::InvalidHistory(error) => error.fmt(formatter),
        }
    }
}

impl<JournalError, WaitError> std::error::Error for ExecutionError<JournalError, WaitError>
where
    JournalError: Display + std::error::Error + 'static,
    WaitError: Display + std::error::Error + 'static,
{
}

pub struct ExternalOperationExecutor<Journal, Jitter, Wait> {
    journal: Journal,
    jitter: Jitter,
    wait: Wait,
}

impl<Journal, Jitter, Wait> ExternalOperationExecutor<Journal, Jitter, Wait>
where
    Journal: OperationJournal,
    Jitter: JitterSource,
    Wait: BackoffWait,
{
    pub const fn new(journal: Journal, jitter: Jitter, wait: Wait) -> Self {
        Self {
            journal,
            jitter,
            wait,
        }
    }

    pub async fn execute<Provider, ProviderFuture>(
        &self,
        key: &IdempotencyKey,
        policy: RetryPolicy,
        provider: Provider,
    ) -> Result<ExecutionOutcome, ExecutionError<Journal::Error, Wait::Error>>
    where
        Provider: Fn() -> ProviderFuture,
        ProviderFuture: std::future::Future<Output = ProviderOutcome>,
    {
        let reservation = self
            .journal
            .reserve(key)
            .await
            .map_err(ExecutionError::PersistenceBeforeInvocation)?;
        let mut completed_attempts = match reservation {
            ReservationOutcome::AlreadyCompleted(outcome) => return Ok(outcome),
            ReservationOutcome::AlreadyInProgress {
                started_attempt,
                completed_attempts,
            } => {
                return Ok(ExecutionOutcome::ManualReview {
                    attempt: started_attempt,
                    failure: OperationFailure::interrupted_attempt(),
                    completed_attempts,
                });
            }
            ReservationOutcome::Acquired { completed_attempts } => completed_attempts,
        };

        validate_retry_history(&completed_attempts, policy)
            .map_err(ExecutionError::InvalidHistory)?;
        let mut provider_was_invoked = false;

        loop {
            let (attempt, delay_before_ms) = match completed_attempts.last() {
                Some(previous) => match policy
                    .after_failure(previous.attempt(), self.jitter.sample())
                    .map_err(|error| {
                        ExecutionError::InvalidHistory(OperationHistoryError::RetryPolicy(error))
                    })? {
                    RetryDecision::Exhausted => {
                        self.record_exhausted(key, &completed_attempts, provider_was_invoked)
                            .await?;
                        return Ok(ExecutionOutcome::Exhausted { completed_attempts });
                    }
                    RetryDecision::Retry {
                        next_attempt,
                        delay_ms,
                    } => {
                        self.wait
                            .wait(delay_ms)
                            .await
                            .map_err(ExecutionError::Wait)?;
                        (next_attempt, Some(delay_ms))
                    }
                },
                None => (
                    AttemptNumber::new(1).map_err(|error| {
                        ExecutionError::InvalidHistory(OperationHistoryError::RetryPolicy(error))
                    })?,
                    None,
                ),
            };

            self.journal
                .record_attempt_started(key, attempt)
                .await
                .map_err(ExecutionError::PersistenceBeforeInvocation)?;

            let outcome = provider().await;
            provider_was_invoked = true;
            let completed = CompletedAttempt::new(attempt, outcome.clone(), delay_before_ms);
            self.journal
                .record_attempt_completed(key, &completed)
                .await
                .map_err(ExecutionError::PersistenceAfterInvocation)?;
            completed_attempts.push(completed);

            match outcome {
                ProviderOutcome::Accepted { resource_id } => {
                    return Ok(ExecutionOutcome::Accepted {
                        attempt,
                        resource_id,
                    });
                }
                ProviderOutcome::TerminalFailure(failure) => {
                    return Ok(ExecutionOutcome::TerminalFailure { attempt, failure });
                }
                ProviderOutcome::Ambiguous(failure) => {
                    return Ok(ExecutionOutcome::ManualReview {
                        attempt,
                        failure,
                        completed_attempts,
                    });
                }
                ProviderOutcome::RetryableFailure(_) => {}
            }
        }
    }

    async fn record_exhausted(
        &self,
        key: &IdempotencyKey,
        completed_attempts: &[CompletedAttempt],
        provider_was_invoked: bool,
    ) -> Result<(), ExecutionError<Journal::Error, Wait::Error>> {
        self.journal
            .record_exhausted(key, completed_attempts)
            .await
            .map_err(|error| {
                if provider_was_invoked {
                    ExecutionError::PersistenceAfterInvocation(error)
                } else {
                    ExecutionError::PersistenceBeforeInvocation(error)
                }
            })
    }
}

fn validate_retry_history(
    completed_attempts: &[CompletedAttempt],
    policy: RetryPolicy,
) -> Result<(), OperationHistoryError> {
    if completed_attempts.len() > usize::from(policy.max_attempts()) {
        return Err(OperationHistoryError::TooManyCompletedAttempts);
    }

    for (index, completed) in completed_attempts.iter().enumerate() {
        let expected = u8::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(OperationHistoryError::TooManyCompletedAttempts)?;
        if completed.attempt().get() != expected {
            return Err(OperationHistoryError::NonContiguousAttempt {
                expected,
                actual: completed.attempt().get(),
            });
        }
        if !matches!(completed.outcome(), ProviderOutcome::RetryableFailure(_)) {
            return Err(OperationHistoryError::NonRetryableCompletedAttempt {
                attempt: completed.attempt(),
            });
        }
    }
    Ok(())
}
