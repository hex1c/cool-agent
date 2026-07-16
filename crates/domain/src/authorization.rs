use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::identity::{ChatId, MessageId, ParticipantId, TopicSessionId};
use crate::transition::{
    TransitionError, TransitionOutcome, TransitionRequest, WorkflowTransition,
};
use crate::workflow::{Workflow, WorkflowRevision, WorkflowTimestamp};

/// Cached approval expires at this age; evidence must be strictly younger.
pub const MAX_CACHED_MEMBERSHIP_AGE_SECONDS: u64 = 30 * 60;

/// Normalized result of a live membership lookup in the approved forum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MembershipStatus {
    Approved,
    NotApproved,
}

/// Ephemeral evidence returned by the live Telegram membership boundary.
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

/// Persistable positive result from a prior live membership lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedMembershipApproval {
    forum: ChatId,
    participant: ParticipantId,
    live_observed_at: WorkflowTimestamp,
}

impl CachedMembershipApproval {
    pub fn from_live(evidence: &LiveMembershipEvidence) -> Result<Self, AuthorizationError> {
        if evidence.status != MembershipStatus::Approved {
            return Err(AuthorizationError::NotApproved);
        }
        Ok(Self {
            forum: evidence.forum,
            participant: evidence.participant,
            live_observed_at: evidence.observed_at,
        })
    }
}

/// Connection-level Telegram membership lookup failures eligible for fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipLookupOutageKind {
    Timeout,
    ConnectionFailure,
}

/// Evidence that a live membership lookup was unresponsive at action time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MembershipLookupOutage {
    forum: ChatId,
    participant: ParticipantId,
    kind: MembershipLookupOutageKind,
    observed_at: WorkflowTimestamp,
}

impl MembershipLookupOutage {
    pub const fn new(
        forum: ChatId,
        participant: ParticipantId,
        kind: MembershipLookupOutageKind,
        observed_at: WorkflowTimestamp,
    ) -> Self {
        Self {
            forum,
            participant,
            kind,
            observed_at,
        }
    }
}

/// Membership evidence source retained for authorization audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MembershipAuthorizationSource {
    Live,
    OutageCache {
        live_observed_at: WorkflowTimestamp,
        outage: MembershipLookupOutageKind,
    },
}

/// A participant capability produced only from accepted, group-bound evidence.
#[derive(Debug, PartialEq, Eq)]
pub struct AuthorizedParticipant {
    actor: ParticipantId,
    approved_forum: ChatId,
    authorized_at: WorkflowTimestamp,
    authorization_source: MembershipAuthorizationSource,
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
            authorization_source: self.authorization_source,
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

    pub const fn authorization_source(&self) -> MembershipAuthorizationSource {
        self.authorization_source
    }
}

/// Authorized workflow action with Google access fixed to the workflow owner.
#[derive(Debug, PartialEq, Eq)]
pub struct AuthorizedWorkflowAction {
    actor: ParticipantId,
    google_principal: ParticipantId,
    approved_forum: ChatId,
    authorized_at: WorkflowTimestamp,
    authorization_source: MembershipAuthorizationSource,
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

    pub const fn authorization_source(&self) -> MembershipAuthorizationSource {
        self.authorization_source
    }
}

/// Topic-qualified optimistic transition inputs for an approved participant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedTransitionRequest {
    pub expected_workflow_revision: WorkflowRevision,
    pub topic: TopicSessionId,
    pub source_message: MessageId,
    pub timestamp: WorkflowTimestamp,
}

/// Durable membership attribution emitted for an authorized participant action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedActionAudit {
    pub actor: ParticipantId,
    pub topic: TopicSessionId,
    pub source_message: MessageId,
    pub authorized_at: WorkflowTimestamp,
    pub membership_source: MembershipAuthorizationSource,
}

/// State transition and membership attribution that must be persisted together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedTransitionOutcome {
    pub transition: TransitionOutcome,
    pub authorization: AuthorizedActionAudit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizedActionError {
    AuthorizationNotCurrent {
        authorized_at: WorkflowTimestamp,
        attempted_at: WorkflowTimestamp,
    },
    ForumMismatch {
        expected: ChatId,
        actual: ChatId,
    },
    TopicMismatch {
        expected: TopicSessionId,
        actual: TopicSessionId,
    },
    Transition(TransitionError),
}

impl From<TransitionError> for AuthorizedActionError {
    fn from(value: TransitionError) -> Self {
        Self::Transition(value)
    }
}

impl Display for AuthorizedActionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "authorized action rejected: {self:?}")
    }
}

impl std::error::Error for AuthorizedActionError {}

impl Workflow {
    /// Stop a workflow using a current participant capability bound to its topic.
    pub fn stop_authorized(
        &self,
        authorization: &AuthorizedParticipant,
        request: AuthorizedTransitionRequest,
    ) -> Result<AuthorizedTransitionOutcome, AuthorizedActionError> {
        validate_authorized_action(
            self,
            authorization.approved_forum,
            authorization.authorized_at,
            request.topic,
            request.timestamp,
        )?;
        let transition = self.transition_with_confirmation_boundary(TransitionRequest {
            transition: WorkflowTransition::StopWorkflow,
            expected_revision: request.expected_workflow_revision,
            actor: authorization.actor,
            source_message: request.source_message,
            timestamp: request.timestamp,
        })?;
        let audit = AuthorizedActionAudit {
            actor: authorization.actor,
            topic: request.topic,
            source_message: request.source_message,
            authorized_at: authorization.authorized_at,
            membership_source: authorization.authorization_source,
        };
        Ok(AuthorizedTransitionOutcome {
            transition,
            authorization: audit,
        })
    }
}

pub(crate) fn validate_authorized_workflow_action(
    workflow: &Workflow,
    authorization: &AuthorizedWorkflowAction,
    topic: TopicSessionId,
    attempted_at: WorkflowTimestamp,
) -> Result<(), AuthorizedActionError> {
    validate_authorized_action(
        workflow,
        authorization.approved_forum,
        authorization.authorized_at,
        topic,
        attempted_at,
    )
}

pub(crate) fn authorized_workflow_action_audit(
    authorization: &AuthorizedWorkflowAction,
    topic: TopicSessionId,
    source_message: MessageId,
) -> AuthorizedActionAudit {
    AuthorizedActionAudit {
        actor: authorization.actor,
        topic,
        source_message,
        authorized_at: authorization.authorized_at,
        membership_source: authorization.authorization_source,
    }
}

fn validate_authorized_action(
    workflow: &Workflow,
    approved_forum: ChatId,
    authorized_at: WorkflowTimestamp,
    topic: TopicSessionId,
    attempted_at: WorkflowTimestamp,
) -> Result<(), AuthorizedActionError> {
    if authorized_at != attempted_at {
        return Err(AuthorizedActionError::AuthorizationNotCurrent {
            authorized_at,
            attempted_at,
        });
    }
    if workflow.topic() != topic {
        return Err(AuthorizedActionError::TopicMismatch {
            expected: workflow.topic(),
            actual: topic,
        });
    }
    if approved_forum != topic.chat_id() {
        return Err(AuthorizedActionError::ForumMismatch {
            expected: topic.chat_id(),
            actual: approved_forum,
        });
    }
    Ok(())
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
    OutageNotCurrent {
        observed_at: WorkflowTimestamp,
        evaluated_at: WorkflowTimestamp,
    },
    CachedApprovalFromFuture {
        observed_at: WorkflowTimestamp,
        evaluated_at: WorkflowTimestamp,
    },
    CachedApprovalExpired {
        observed_at: WorkflowTimestamp,
        evaluated_at: WorkflowTimestamp,
        maximum_age_seconds: u64,
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
            Self::OutageNotCurrent {
                observed_at,
                evaluated_at,
            } => write!(
                formatter,
                "membership outage timestamp {} does not match evaluation timestamp {}",
                observed_at.as_unix_seconds(),
                evaluated_at.as_unix_seconds()
            ),
            Self::CachedApprovalFromFuture {
                observed_at,
                evaluated_at,
            } => write!(
                formatter,
                "cached membership approval timestamp {} is after evaluation timestamp {}",
                observed_at.as_unix_seconds(),
                evaluated_at.as_unix_seconds()
            ),
            Self::CachedApprovalExpired {
                observed_at,
                evaluated_at,
                maximum_age_seconds,
            } => write!(
                formatter,
                "cached membership approval from {} is not younger than {maximum_age_seconds} seconds at {}",
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
    validate_identity_binding(approved_forum, actor, evidence.forum, evidence.participant)?;
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
        authorization_source: MembershipAuthorizationSource::Live,
    })
}

/// Authorize from a prior positive lookup only during a current connection outage.
pub fn authorize_participant_from_cache(
    approved_forum: ChatId,
    actor: ParticipantId,
    cached: &CachedMembershipApproval,
    outage: &MembershipLookupOutage,
    evaluated_at: WorkflowTimestamp,
) -> Result<AuthorizedParticipant, AuthorizationError> {
    validate_identity_binding(approved_forum, actor, cached.forum, cached.participant)?;
    validate_identity_binding(approved_forum, actor, outage.forum, outage.participant)?;
    if outage.observed_at != evaluated_at {
        return Err(AuthorizationError::OutageNotCurrent {
            observed_at: outage.observed_at,
            evaluated_at,
        });
    }
    if cached.live_observed_at > evaluated_at {
        return Err(AuthorizationError::CachedApprovalFromFuture {
            observed_at: cached.live_observed_at,
            evaluated_at,
        });
    }
    let age_seconds = evaluated_at.as_unix_seconds() - cached.live_observed_at.as_unix_seconds();
    if age_seconds >= MAX_CACHED_MEMBERSHIP_AGE_SECONDS {
        return Err(AuthorizationError::CachedApprovalExpired {
            observed_at: cached.live_observed_at,
            evaluated_at,
            maximum_age_seconds: MAX_CACHED_MEMBERSHIP_AGE_SECONDS,
        });
    }

    Ok(AuthorizedParticipant {
        actor,
        approved_forum,
        authorized_at: evaluated_at,
        authorization_source: MembershipAuthorizationSource::OutageCache {
            live_observed_at: cached.live_observed_at,
            outage: outage.kind,
        },
    })
}

fn validate_identity_binding(
    approved_forum: ChatId,
    actor: ParticipantId,
    evidence_forum: ChatId,
    evidence_participant: ParticipantId,
) -> Result<(), AuthorizationError> {
    if evidence_forum != approved_forum {
        return Err(AuthorizationError::ForumMismatch {
            expected: approved_forum,
            actual: evidence_forum,
        });
    }
    if evidence_participant != actor {
        return Err(AuthorizationError::ParticipantMismatch {
            expected: actor,
            actual: evidence_participant,
        });
    }
    Ok(())
}
