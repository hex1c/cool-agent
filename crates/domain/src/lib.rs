#![deny(unsafe_code)]

//! Pure workflow and policy contracts.

pub mod attachment;
pub mod authorization;
pub mod confirmation;
pub mod contracts;
pub mod idempotency;
pub mod identity;
pub mod money;
pub mod retry;
pub mod routing;
pub mod transition;
pub mod workflow;

pub use authorization::{
    AuthorizationError, AuthorizedActionAudit, AuthorizedActionError, AuthorizedParticipant,
    AuthorizedTransitionOutcome, AuthorizedTransitionRequest, AuthorizedWorkflowAction,
    CachedMembershipApproval, LiveMembershipEvidence, MAX_CACHED_MEMBERSHIP_AGE_SECONDS,
    MembershipAuthorizationSource, MembershipLookupOutage, MembershipLookupOutageKind,
    MembershipStatus, authorize_participant, authorize_participant_from_cache,
};
pub use confirmation::{
    ConfirmationAction, ConfirmationConsumePrecondition, ConfirmationConsumeRequest,
    ConfirmationConsumption, ConfirmationCorrectionOutcome, ConfirmationCorrectionRequest,
    ConfirmationError, ConfirmationIssueOutcome, ConfirmationIssuePrecondition,
    ConfirmationIssueRequest, ConfirmationRecord, ConfirmationStatus, ConsumedConfirmation,
    MutationTargetFingerprint, PendingConfirmation, PreviewDigest, TopicMessageReference,
};
pub use idempotency::{IdempotencyKey, OperationKind, OperationTargetFingerprint};
pub use retry::{
    AttemptNumber, JitterSample, MAX_EXTERNAL_OPERATION_ATTEMPTS, RetryDecision, RetryPolicy,
    RetryPolicyError,
};
pub use transition::{
    TransitionAudit, TransitionError, TransitionOutcome, TransitionRequest, WorkflowTransition,
    WorkflowTransitionKind,
};
pub use workflow::{
    ClarificationResume, WaitDeadline, Workflow, WorkflowRevision, WorkflowState,
    WorkflowStateKind, WorkflowTimestamp,
};
