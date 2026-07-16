use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::authorization::{AuthorizedWorkflowAction, MembershipAuthorizationSource};
use crate::identity::{ConfirmationId, MessageId, ParticipantId, TopicSessionId, WorkflowId};
use crate::transition::{
    TransitionError, TransitionOutcome, TransitionRequest, WorkflowTransition,
};
use crate::workflow::{
    WaitDeadline, Workflow, WorkflowRevision, WorkflowState, WorkflowStateKind, WorkflowTimestamp,
};

/// Digest of the exact preview shown to participants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PreviewDigest([u8; 32]);

impl PreviewDigest {
    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Fingerprint of the exact external resource and mutation payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MutationTargetFingerprint([u8; 32]);

impl MutationTargetFingerprint {
    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Sensitive state-machine action selected when the preview is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationAction {
    StartSheetOrDocWrite,
    StartDirectPdfGeneration,
    StartCalendarOrEmailAction,
}

/// Durable confirmation waiting to be consumed exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingConfirmation {
    confirmation_id: ConfirmationId,
    workflow_id: WorkflowId,
    workflow_revision: WorkflowRevision,
    owner: ParticipantId,
    topic: TopicSessionId,
    preview_digest: PreviewDigest,
    mutation_target: MutationTargetFingerprint,
    action: ConfirmationAction,
    expires_at: WaitDeadline,
}

impl PendingConfirmation {
    pub fn confirmation_id(&self) -> &ConfirmationId {
        &self.confirmation_id
    }

    pub fn workflow_id(&self) -> &WorkflowId {
        &self.workflow_id
    }

    pub const fn workflow_revision(&self) -> WorkflowRevision {
        self.workflow_revision
    }

    pub const fn owner(&self) -> ParticipantId {
        self.owner
    }

    pub const fn topic(&self) -> TopicSessionId {
        self.topic
    }

    pub const fn preview_digest(&self) -> PreviewDigest {
        self.preview_digest
    }

    pub const fn mutation_target(&self) -> MutationTargetFingerprint {
        self.mutation_target
    }

    pub const fn action(&self) -> ConfirmationAction {
        self.action
    }

    pub const fn expires_at(&self) -> WaitDeadline {
        self.expires_at
    }
}

/// Topic-qualified Telegram message used to consume a confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopicMessageReference {
    topic: TopicSessionId,
    message_id: MessageId,
}

impl TopicMessageReference {
    pub const fn new(topic: TopicSessionId, message_id: MessageId) -> Self {
        Self { topic, message_id }
    }

    pub const fn topic(&self) -> TopicSessionId {
        self.topic
    }

    pub const fn message_id(&self) -> MessageId {
        self.message_id
    }
}

/// Durable lifecycle state of a confirmation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationStatus {
    Pending,
    Consumed,
}

/// Durable attribution captured when a pending confirmation is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumedConfirmation {
    pending: PendingConfirmation,
    confirming_actor: ParticipantId,
    membership_authorization: MembershipAuthorizationSource,
    source: TopicMessageReference,
    confirmed_at: WorkflowTimestamp,
    resulting_workflow_revision: WorkflowRevision,
}

/// Persisted record used to reject replay after successful consumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "confirmation", rename_all = "snake_case")]
pub enum ConfirmationRecord {
    Pending(PendingConfirmation),
    Consumed(ConsumedConfirmation),
}

impl From<PendingConfirmation> for ConfirmationRecord {
    fn from(value: PendingConfirmation) -> Self {
        Self::Pending(value)
    }
}

impl ConfirmationRecord {
    pub const fn status(&self) -> ConfirmationStatus {
        match self {
            Self::Pending(_) => ConfirmationStatus::Pending,
            Self::Consumed(_) => ConfirmationStatus::Consumed,
        }
    }

    pub const fn confirming_actor(&self) -> Option<ParticipantId> {
        match self {
            Self::Pending(_) => None,
            Self::Consumed(consumed) => Some(consumed.confirming_actor),
        }
    }

    pub const fn membership_authorization(&self) -> Option<MembershipAuthorizationSource> {
        match self {
            Self::Pending(_) => None,
            Self::Consumed(consumed) => Some(consumed.membership_authorization),
        }
    }

    /// Validate and consume a confirmation, returning writes that must commit atomically.
    pub fn consume(
        &self,
        workflow: &Workflow,
        authorization: &AuthorizedWorkflowAction,
        request: ConfirmationConsumeRequest,
    ) -> Result<ConfirmationConsumption, ConfirmationError> {
        let pending = match self {
            Self::Pending(pending) => pending,
            Self::Consumed(_) => return Err(ConfirmationError::AlreadyConsumed),
        };

        if authorization.authorized_at() != request.confirmed_at {
            return Err(ConfirmationError::AuthorizationNotCurrent {
                authorized_at: authorization.authorized_at(),
                confirmed_at: request.confirmed_at,
            });
        }
        if authorization.approved_forum() != pending.topic.chat_id() {
            return Err(ConfirmationError::ForumMismatch {
                expected: pending.topic.chat_id(),
                actual: authorization.approved_forum(),
            });
        }
        if request.source.topic != pending.topic {
            return Err(ConfirmationError::TopicMismatch {
                expected: pending.topic,
                actual: request.source.topic,
            });
        }
        if workflow.id() != &pending.workflow_id {
            return Err(ConfirmationError::WorkflowMismatch);
        }
        if workflow.owner() != pending.owner {
            return Err(ConfirmationError::OwnerMismatch {
                expected: pending.owner,
                actual: workflow.owner(),
            });
        }
        if authorization.google_principal() != pending.owner {
            return Err(ConfirmationError::OwnerMismatch {
                expected: pending.owner,
                actual: authorization.google_principal(),
            });
        }

        let workflow_deadline = match workflow.state() {
            WorkflowState::WaitingForConfirmation { deadline } => *deadline,
            state => {
                return Err(ConfirmationError::WorkflowNotWaiting {
                    state: state.kind(),
                });
            }
        };
        if workflow_deadline != pending.expires_at {
            return Err(ConfirmationError::DeadlineMismatch {
                expected: pending.expires_at,
                actual: workflow_deadline,
            });
        }
        if workflow.revision() != pending.workflow_revision {
            return Err(ConfirmationError::RevisionMismatch {
                expected: pending.workflow_revision,
                actual: workflow.revision(),
            });
        }
        if pending.expires_at.has_elapsed(request.confirmed_at) {
            return Err(ConfirmationError::Expired {
                expired_at: pending.expires_at,
                attempted_at: request.confirmed_at,
            });
        }
        if request.preview_digest != pending.preview_digest {
            return Err(ConfirmationError::PreviewDigestMismatch);
        }
        if request.mutation_target != pending.mutation_target {
            return Err(ConfirmationError::MutationTargetMismatch);
        }

        let transition_kind = match pending.action {
            ConfirmationAction::StartSheetOrDocWrite => WorkflowTransition::StartSheetOrDocWrite,
            ConfirmationAction::StartDirectPdfGeneration => WorkflowTransition::StartPdfGeneration,
            ConfirmationAction::StartCalendarOrEmailAction => {
                WorkflowTransition::StartCalendarOrEmailAction
            }
        };
        let transition = workflow.transition_with_confirmation_boundary(TransitionRequest {
            transition: transition_kind,
            expected_revision: pending.workflow_revision,
            actor: authorization.actor(),
            source_message: request.source.message_id,
            timestamp: request.confirmed_at,
        })?;
        let precondition = ConfirmationConsumePrecondition {
            expected_workflow_revision: pending.workflow_revision,
            confirmation_id: pending.confirmation_id.clone(),
            expected_confirmation_status: ConfirmationStatus::Pending,
        };
        let confirmation = Self::Consumed(ConsumedConfirmation {
            pending: pending.clone(),
            confirming_actor: authorization.actor(),
            membership_authorization: authorization.authorization_source(),
            source: request.source,
            confirmed_at: request.confirmed_at,
            resulting_workflow_revision: transition.workflow.revision(),
        });

        Ok(ConfirmationConsumption {
            transition,
            confirmation,
            precondition,
        })
    }
}

/// Callback values checked against the durable pending binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationConsumeRequest {
    pub preview_digest: PreviewDigest,
    pub mutation_target: MutationTargetFingerprint,
    pub source: TopicMessageReference,
    pub confirmed_at: WorkflowTimestamp,
}

/// Storage-neutral conditions required for one-time atomic consumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationConsumePrecondition {
    pub expected_workflow_revision: WorkflowRevision,
    pub confirmation_id: ConfirmationId,
    pub expected_confirmation_status: ConfirmationStatus,
}

/// Consumed record and workflow transition that must be persisted together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationConsumption {
    pub transition: TransitionOutcome,
    pub confirmation: ConfirmationRecord,
    pub precondition: ConfirmationConsumePrecondition,
}

/// Typed fail-closed rejection reasons for confirmation consumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmationError {
    AlreadyConsumed,
    AuthorizationNotCurrent {
        authorized_at: WorkflowTimestamp,
        confirmed_at: WorkflowTimestamp,
    },
    ForumMismatch {
        expected: crate::identity::ChatId,
        actual: crate::identity::ChatId,
    },
    TopicMismatch {
        expected: TopicSessionId,
        actual: TopicSessionId,
    },
    WorkflowMismatch,
    OwnerMismatch {
        expected: ParticipantId,
        actual: ParticipantId,
    },
    WorkflowNotWaiting {
        state: WorkflowStateKind,
    },
    DeadlineMismatch {
        expected: WaitDeadline,
        actual: WaitDeadline,
    },
    RevisionMismatch {
        expected: WorkflowRevision,
        actual: WorkflowRevision,
    },
    Expired {
        expired_at: WaitDeadline,
        attempted_at: WorkflowTimestamp,
    },
    PreviewDigestMismatch,
    MutationTargetMismatch,
    Transition(TransitionError),
}

impl From<TransitionError> for ConfirmationError {
    fn from(value: TransitionError) -> Self {
        Self::Transition(value)
    }
}

impl Display for ConfirmationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "confirmation rejected: {self:?}")
    }
}

impl std::error::Error for ConfirmationError {}

/// Inputs that atomically enter the wait state and issue its pending record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationIssueRequest {
    pub confirmation_id: ConfirmationId,
    pub expected_workflow_revision: WorkflowRevision,
    pub topic: TopicSessionId,
    pub preview_digest: PreviewDigest,
    pub mutation_target: MutationTargetFingerprint,
    pub action: ConfirmationAction,
    pub deadline: WaitDeadline,
    pub actor: ParticipantId,
    pub source_message: MessageId,
    pub timestamp: WorkflowTimestamp,
}

/// Storage-neutral condition required when persisting an issuance outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationIssuePrecondition {
    pub expected_workflow_revision: WorkflowRevision,
    pub confirmation_id_must_not_exist: ConfirmationId,
}

/// Workflow transition and pending record that must be persisted together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationIssueOutcome {
    pub transition: TransitionOutcome,
    pub confirmation: PendingConfirmation,
    pub precondition: ConfirmationIssuePrecondition,
}

impl Workflow {
    /// Enter the confirmation wait and bind its resulting revision in one domain operation.
    pub fn issue_confirmation(
        &self,
        request: ConfirmationIssueRequest,
    ) -> Result<ConfirmationIssueOutcome, TransitionError> {
        if request.topic != self.topic() {
            return Err(TransitionError::TopicMismatch {
                expected: self.topic(),
                actual: request.topic,
            });
        }
        let old_revision = request.expected_workflow_revision;
        let confirmation_id = request.confirmation_id;
        let topic = request.topic;
        let preview_digest = request.preview_digest;
        let mutation_target = request.mutation_target;
        let action = request.action;
        let deadline = request.deadline;

        let transition = self.transition_with_confirmation_boundary(TransitionRequest {
            transition: WorkflowTransition::RequestConfirmation { deadline },
            expected_revision: old_revision,
            actor: request.actor,
            source_message: request.source_message,
            timestamp: request.timestamp,
        })?;
        let confirmation = PendingConfirmation {
            confirmation_id: confirmation_id.clone(),
            workflow_id: transition.workflow.id().clone(),
            workflow_revision: transition.workflow.revision(),
            owner: transition.workflow.owner(),
            topic,
            preview_digest,
            mutation_target,
            action,
            expires_at: deadline,
        };
        let precondition = ConfirmationIssuePrecondition {
            expected_workflow_revision: old_revision,
            confirmation_id_must_not_exist: confirmation_id,
        };

        Ok(ConfirmationIssueOutcome {
            transition,
            confirmation,
            precondition,
        })
    }
}
