use std::fmt::{Display, Formatter};

use crate::identity::{ChatId, ParticipantId};
use crate::workflow::{Workflow, WorkflowTimestamp};

/// Normalized result of a live membership lookup in the approved forum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipStatus {
    Approved,
    NotApproved,
}

/// Ephemeral evidence returned by the live Telegram membership boundary.
///
/// This type is deliberately not serializable: Task 16 defaults to a fresh live
/// check for every sensitive action until an outage-cache duration is approved.
#[derive(Debug, PartialEq, Eq)]
pub struct LiveMembershipEvidence {
    forum: ChatId,
    participant: ParticipantId,
    status: MembershipStatus,
    observed_at: WorkflowTimestamp,
}

impl LiveMembershipEvidence {
    pub const fn new(
        forum: ChatId,
        participant: ParticipantId,
        status: MembershipStatus,
        observed_at: WorkflowTimestamp,
    ) -> Self {
        Self {
            forum,
            participant,
            status,
            observed_at,
        }
    }
}

/// A participant capability produced only from current, group-bound evidence.
#[derive(Debug, PartialEq, Eq)]
pub struct AuthorizedParticipant {
    actor: ParticipantId,
    approved_forum: ChatId,
    authorized_at: WorkflowTimestamp,
}

impl AuthorizedParticipant {
    /// Bind an authorized actor to a workflow without allowing the caller to
    /// select a different Google principal.
    pub const fn for_workflow(&self, workflow: &Workflow) -> AuthorizedWorkflowAction {
        AuthorizedWorkflowAction {
            actor: self.actor,
            google_principal: workflow.owner(),
            approved_forum: self.approved_forum,
            authorized_at: self.authorized_at,
        }
    }

    pub const fn actor(&self) -> ParticipantId {
        self.actor
    }

    pub const fn approved_forum(&self) -> ChatId {
        self.approved_forum
    }

    pub const fn authorized_at(&self) -> WorkflowTimestamp {
        self.authorized_at
    }
}

/// Authorized workflow action with Google access fixed to the workflow owner.
#[derive(Debug, PartialEq, Eq)]
pub struct AuthorizedWorkflowAction {
    actor: ParticipantId,
    google_principal: ParticipantId,
    approved_forum: ChatId,
    authorized_at: WorkflowTimestamp,
}

impl AuthorizedWorkflowAction {
    pub const fn actor(&self) -> ParticipantId {
        self.actor
    }

    pub const fn google_principal(&self) -> ParticipantId {
        self.google_principal
    }

    pub const fn approved_forum(&self) -> ChatId {
        self.approved_forum
    }

    pub const fn authorized_at(&self) -> WorkflowTimestamp {
        self.authorized_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationError {
    ForumMismatch {
        expected: ChatId,
        actual: ChatId,
    },
    ParticipantMismatch {
        expected: ParticipantId,
        actual: ParticipantId,
    },
    EvidenceNotCurrent {
        observed_at: WorkflowTimestamp,
        evaluated_at: WorkflowTimestamp,
    },
    NotApproved,
}

impl Display for AuthorizationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForumMismatch { expected, actual } => write!(
                formatter,
                "membership evidence is for forum {actual}, expected {expected}"
            ),
            Self::ParticipantMismatch { expected, actual } => write!(
                formatter,
                "membership evidence is for participant {actual}, expected {expected}"
            ),
            Self::EvidenceNotCurrent {
                observed_at,
                evaluated_at,
            } => write!(
                formatter,
                "membership evidence timestamp {} does not match evaluation timestamp {}",
                observed_at.as_unix_seconds(),
                evaluated_at.as_unix_seconds()
            ),
            Self::NotApproved => formatter.write_str("participant is not an approved forum member"),
        }
    }
}

impl std::error::Error for AuthorizationError {}

/// Validate one live membership result and mint a short-lived actor capability.
pub fn authorize_participant(
    approved_forum: ChatId,
    actor: ParticipantId,
    evidence: &LiveMembershipEvidence,
    evaluated_at: WorkflowTimestamp,
) -> Result<AuthorizedParticipant, AuthorizationError> {
    if evidence.forum != approved_forum {
        return Err(AuthorizationError::ForumMismatch {
            expected: approved_forum,
            actual: evidence.forum,
        });
    }
    if evidence.participant != actor {
        return Err(AuthorizationError::ParticipantMismatch {
            expected: actor,
            actual: evidence.participant,
        });
    }
    if evidence.observed_at != evaluated_at {
        return Err(AuthorizationError::EvidenceNotCurrent {
            observed_at: evidence.observed_at,
            evaluated_at,
        });
    }
    if evidence.status != MembershipStatus::Approved {
        return Err(AuthorizationError::NotApproved);
    }

    Ok(AuthorizedParticipant {
        actor,
        approved_forum,
        authorized_at: evaluated_at,
    })
}
