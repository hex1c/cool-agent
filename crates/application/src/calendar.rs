//! Confirmed Calendar event creation orchestration (Task 36).
//!
//! Pure application layer that binds a consumed confirmation to the external
//! operation journal key for a Google Calendar event creation.  Like
//! [`crate::existing_files`], every method is pure — it operates over domain
//! types and returns only the validated binding.  Persistence
//! (`ConfirmationRepository::consume_and_prepare_operation`) and provider
//! invocation (`ExternalOperationExecutor` with the
//! [`google::calendar::GoogleCalendarService`]) are the caller's
//! responsibility.
//!
//! The binding enforces the Task 36 contract:
//! - The confirmation must be consumed with `StartCalendarOrEmailAction`.
//! - The operation key binds the event's mutation-target fingerprint via the
//!   confirmation's mutation-target fingerprint and the resulting workflow
//!   revision.
//! - Calendar creation uses [`OperationKind::GoogleWrite`] (email uses
//!   `SmtpSend` — see Task 37).
//! - A stale (already-consumed) confirmation is rejected by the domain
//!   `consume` step before this service is reached; a duplicate
//!   `consume_and_prepare_operation` is rejected by the repository conditional
//!   write.  In both cases zero provider writes occur, so reminders and event
//!   creations cannot duplicate on retry.

use std::fmt::{Display, Formatter};

use domain::confirmation::ConfirmationConsumption;
use domain::idempotency::{IdempotencyKey, OperationKind, OperationTargetFingerprint};
use domain::retry::RetryPolicy;
use domain::workflow::WorkflowRevision;

use crate::repositories::{ConsumeAndPrepareError, ConsumeAndPrepareRequest};

/// Rejected calendar operation binding.
#[derive(Debug)]
pub enum CalendarFlowError {
    /// The confirmation-to-operation binding failed (revision, target, or
    /// action-kind mismatch).
    Binding(ConsumeAndPrepareError),
}

impl Display for CalendarFlowError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Binding(error) => {
                write!(formatter, "calendar operation binding failed: {error}")
            }
        }
    }
}

impl std::error::Error for CalendarFlowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binding(error) => Some(error),
        }
    }
}

impl From<ConsumeAndPrepareError> for CalendarFlowError {
    fn from(value: ConsumeAndPrepareError) -> Self {
        Self::Binding(value)
    }
}

/// A validated confirmation-to-operation binding ready for atomic persistence
/// and execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCalendarEvent {
    consume_request: ConsumeAndPrepareRequest,
    retry_policy: RetryPolicy,
}

impl PreparedCalendarEvent {
    /// The atomic consume-and-prepare request for the confirmation repository.
    pub const fn consume_request(&self) -> &ConsumeAndPrepareRequest {
        &self.consume_request
    }

    /// The retry policy for the external operation executor.
    pub const fn retry_policy(&self) -> RetryPolicy {
        self.retry_policy
    }

    /// The stable operation key reused by every physical retry attempt.
    pub const fn operation_key(&self) -> &IdempotencyKey {
        self.consume_request.operation_key()
    }
}

/// Stateless orchestration for confirmed Calendar event creation.
///
/// Binds a consumed [`ConfirmationConsumption`] to the [`IdempotencyKey`] the
/// external operation executor will reuse for every retry attempt.  The key
/// binds:
/// - the workflow id and **resulting** revision (after consumption),
/// - [`OperationKind::GoogleWrite`], and
/// - the confirmation's mutation-target fingerprint as the
///   [`OperationTargetFingerprint`].
///
/// [`ConsumeAndPrepareRequest::new`] then validates that the confirmation
/// action pairs with `GoogleWrite`, that the target matches, and that the
/// revisions agree — all before any I/O.
pub struct CalendarOperationService;

impl CalendarOperationService {
    /// Build the operation binding from a freshly consumed confirmation.
    ///
    /// The caller must have already obtained `consumption` from
    /// [`crate::resumable_confirmation::ResumableConfirmationService::consume_confirmation`],
    /// which rejects stale (already-consumed) confirmations at the domain
    /// layer.  This method then constructs and validates the durable operation
    /// key.
    pub fn prepare(
        consumption: ConfirmationConsumption,
        retry_policy: RetryPolicy,
    ) -> Result<PreparedCalendarEvent, CalendarFlowError> {
        let transition = consumption.transition();
        let resulting_revision: WorkflowRevision = transition.workflow.revision();
        let mutation_target = consumption.confirmation().mutation_target();
        let target: OperationTargetFingerprint =
            OperationTargetFingerprint::new(*mutation_target.as_bytes());
        let operation_key = IdempotencyKey::new(
            transition.workflow.id().clone(),
            resulting_revision,
            OperationKind::GoogleWrite,
            target,
        );
        let consume_request = ConsumeAndPrepareRequest::new(consumption, operation_key)?;
        Ok(PreparedCalendarEvent {
            consume_request,
            retry_policy,
        })
    }
}
