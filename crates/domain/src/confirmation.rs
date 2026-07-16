use serde::{Deserialize, Serialize};

use crate::identity::{ConfirmationId, MessageId, ParticipantId, TopicSessionId, WorkflowId};
use crate::transition::{
    TransitionError, TransitionOutcome, TransitionRequest, WorkflowTransition,
};
use crate::workflow::{WaitDeadline, Workflow, WorkflowRevision, WorkflowTimestamp};

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
