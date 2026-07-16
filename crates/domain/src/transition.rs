use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::identity::{MessageId, ParticipantId, TopicSessionId};
use crate::workflow::{
    ClarificationResume, WaitDeadline, Workflow, WorkflowRevision, WorkflowState,
    WorkflowStateKind, WorkflowTimestamp,
};

/// A requested state-machine action. Authorization is intentionally handled by Task 16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTransition {
    BeginAttachmentCollection {
        deadline: WaitDeadline,
    },
    FinishAttachmentCollection,
    AttachmentCollectionTimedOut {
        clarification_deadline: WaitDeadline,
    },
    CompleteExtraction,
    StartCalculationOrDrafting,
    CompleteCalculationOrDrafting,
    RequestClarification {
        deadline: WaitDeadline,
    },
    ResumeAttachmentCollection {
        deadline: WaitDeadline,
    },
    ProvideClarification,
    RequestConfirmation {
        deadline: WaitDeadline,
    },
    ApplyCorrection,
    StartSheetOrDocWrite,
    CompleteSheetOrDocWrite,
    StartPdfGeneration,
    CompletePdfGeneration,
    StartCalendarOrEmailAction,
    CompleteCalendarOrEmailAction,
    CompleteArtifactDelivery,
    CompleteWorkflow,
    FailWorkflow,
    ExpireWorkflow,
    StopWorkflow,
}

impl WorkflowTransition {
    pub const fn kind(self) -> WorkflowTransitionKind {
        match self {
            Self::BeginAttachmentCollection { .. } => {
                WorkflowTransitionKind::BeginAttachmentCollection
            }
            Self::FinishAttachmentCollection => WorkflowTransitionKind::FinishAttachmentCollection,
            Self::AttachmentCollectionTimedOut { .. } => {
                WorkflowTransitionKind::AttachmentCollectionTimedOut
            }
            Self::CompleteExtraction => WorkflowTransitionKind::CompleteExtraction,
            Self::StartCalculationOrDrafting => WorkflowTransitionKind::StartCalculationOrDrafting,
            Self::CompleteCalculationOrDrafting => {
                WorkflowTransitionKind::CompleteCalculationOrDrafting
            }
            Self::RequestClarification { .. } => WorkflowTransitionKind::RequestClarification,
            Self::ResumeAttachmentCollection { .. } => {
                WorkflowTransitionKind::ResumeAttachmentCollection
            }
            Self::ProvideClarification => WorkflowTransitionKind::ProvideClarification,
            Self::RequestConfirmation { .. } => WorkflowTransitionKind::RequestConfirmation,
            Self::ApplyCorrection => WorkflowTransitionKind::ApplyCorrection,
            Self::StartSheetOrDocWrite => WorkflowTransitionKind::StartSheetOrDocWrite,
            Self::CompleteSheetOrDocWrite => WorkflowTransitionKind::CompleteSheetOrDocWrite,
            Self::StartPdfGeneration => WorkflowTransitionKind::StartPdfGeneration,
            Self::CompletePdfGeneration => WorkflowTransitionKind::CompletePdfGeneration,
            Self::StartCalendarOrEmailAction => WorkflowTransitionKind::StartCalendarOrEmailAction,
            Self::CompleteCalendarOrEmailAction => {
                WorkflowTransitionKind::CompleteCalendarOrEmailAction
            }
            Self::CompleteArtifactDelivery => WorkflowTransitionKind::CompleteArtifactDelivery,
            Self::CompleteWorkflow => WorkflowTransitionKind::CompleteWorkflow,
            Self::FailWorkflow => WorkflowTransitionKind::FailWorkflow,
            Self::ExpireWorkflow => WorkflowTransitionKind::ExpireWorkflow,
            Self::StopWorkflow => WorkflowTransitionKind::StopWorkflow,
        }
    }
}

/// Data-free transition discriminator used in typed errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkflowTransitionKind {
    BeginAttachmentCollection,
    FinishAttachmentCollection,
    AttachmentCollectionTimedOut,
    CompleteExtraction,
    StartCalculationOrDrafting,
    CompleteCalculationOrDrafting,
    RequestClarification,
    ResumeAttachmentCollection,
    ProvideClarification,
    RequestConfirmation,
    ApplyCorrection,
    StartSheetOrDocWrite,
    CompleteSheetOrDocWrite,
    StartPdfGeneration,
    CompletePdfGeneration,
    StartCalendarOrEmailAction,
    CompleteCalendarOrEmailAction,
    CompleteArtifactDelivery,
    CompleteWorkflow,
    FailWorkflow,
    ExpireWorkflow,
    StopWorkflow,
}

/// Attribution and optimistic-lock inputs required for every transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionRequest {
    pub transition: WorkflowTransition,
    pub expected_revision: WorkflowRevision,
    pub actor: ParticipantId,
    pub source_message: MessageId,
    pub timestamp: WorkflowTimestamp,
}

/// Audit metadata emitted atomically with every accepted transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionAudit {
    pub owner: ParticipantId,
    pub actor: ParticipantId,
    pub source_message: MessageId,
    pub from: WorkflowStateKind,
    pub to: WorkflowStateKind,
    pub old_revision: WorkflowRevision,
    pub new_revision: WorkflowRevision,
    pub timestamp: WorkflowTimestamp,
}

/// The new immutable aggregate and its corresponding audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionOutcome {
    pub workflow: Workflow,
    pub audit: TransitionAudit,
}

/// Typed rejection reasons for pure workflow transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    RevisionConflict {
        expected: WorkflowRevision,
        actual: WorkflowRevision,
    },
    TerminalState {
        state: WorkflowStateKind,
    },
    WaitDeadlineElapsed {
        deadline: WaitDeadline,
        attempted_at: WorkflowTimestamp,
    },
    DeadlineNotReached {
        deadline: WaitDeadline,
        attempted_at: WorkflowTimestamp,
    },
    InvalidDeadline {
        deadline: WaitDeadline,
        timestamp: WorkflowTimestamp,
    },
    TimestampBeforeLastTransition {
        previous: WorkflowTimestamp,
        attempted: WorkflowTimestamp,
    },
    IllegalTransition {
        from: WorkflowStateKind,
        transition: WorkflowTransitionKind,
    },
    ConfirmationBoundaryRequired {
        transition: WorkflowTransitionKind,
    },
    TopicMismatch {
        expected: TopicSessionId,
        actual: TopicSessionId,
    },
    RevisionExhausted,
}

impl Display for TransitionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => write!(
                formatter,
                "workflow revision conflict: expected {}, actual {}",
                expected.get(),
                actual.get()
            ),
            Self::TerminalState { state } => {
                write!(formatter, "terminal workflow state {state:?} is immutable")
            }
            Self::WaitDeadlineElapsed {
                deadline,
                attempted_at,
            } => write!(
                formatter,
                "wait deadline {} elapsed before transition at {}",
                deadline.timestamp().as_unix_seconds(),
                attempted_at.as_unix_seconds()
            ),
            Self::DeadlineNotReached {
                deadline,
                attempted_at,
            } => write!(
                formatter,
                "deadline {} has not elapsed at {}",
                deadline.timestamp().as_unix_seconds(),
                attempted_at.as_unix_seconds()
            ),
            Self::InvalidDeadline {
                deadline,
                timestamp,
            } => write!(
                formatter,
                "deadline {} must be after transition timestamp {}",
                deadline.timestamp().as_unix_seconds(),
                timestamp.as_unix_seconds()
            ),
            Self::TimestampBeforeLastTransition {
                previous,
                attempted,
            } => write!(
                formatter,
                "transition timestamp {} precedes previous timestamp {}",
                attempted.as_unix_seconds(),
                previous.as_unix_seconds()
            ),
            Self::IllegalTransition { from, transition } => {
                write!(
                    formatter,
                    "transition {transition:?} is illegal from {from:?}"
                )
            }
            Self::ConfirmationBoundaryRequired { transition } => write!(
                formatter,
                "transition {transition:?} requires the confirmation boundary"
            ),
            Self::TopicMismatch { expected, actual } => write!(
                formatter,
                "workflow topic {actual:?} does not match expected topic {expected:?}"
            ),
            Self::RevisionExhausted => formatter.write_str("workflow revision exhausted"),
        }
    }
}

impl std::error::Error for TransitionError {}

impl Workflow {
    /// Apply one pure, optimistic transition without mutating the current aggregate.
    pub fn transition(
        &self,
        request: TransitionRequest,
    ) -> Result<TransitionOutcome, TransitionError> {
        if matches!(
            request.transition,
            WorkflowTransition::RequestConfirmation { .. }
        ) || matches!(self.state(), WorkflowState::WaitingForConfirmation { .. })
            && matches!(
                request.transition,
                WorkflowTransition::StartSheetOrDocWrite
                    | WorkflowTransition::StartPdfGeneration
                    | WorkflowTransition::StartCalendarOrEmailAction
            )
        {
            return Err(TransitionError::ConfirmationBoundaryRequired {
                transition: request.transition.kind(),
            });
        }
        self.transition_with_confirmation_boundary(request)
    }

    pub(crate) fn transition_with_confirmation_boundary(
        &self,
        request: TransitionRequest,
    ) -> Result<TransitionOutcome, TransitionError> {
        if request.expected_revision != self.revision() {
            return Err(TransitionError::RevisionConflict {
                expected: request.expected_revision,
                actual: self.revision(),
            });
        }
        if self.state().is_terminal() {
            return Err(TransitionError::TerminalState {
                state: self.state().kind(),
            });
        }
        if request.timestamp < self.updated_at() {
            return Err(TransitionError::TimestampBeforeLastTransition {
                previous: self.updated_at(),
                attempted: request.timestamp,
            });
        }

        reject_elapsed_wait(self.state(), request.transition, request.timestamp)?;
        let next_state = next_state(self.state(), request.transition, request.timestamp)?;
        let new_revision = self
            .revision()
            .next()
            .ok_or(TransitionError::RevisionExhausted)?;
        let workflow = self.with_transition(next_state, new_revision, request.timestamp);
        let audit = TransitionAudit {
            owner: self.owner(),
            actor: request.actor,
            source_message: request.source_message,
            from: self.state().kind(),
            to: workflow.state().kind(),
            old_revision: self.revision(),
            new_revision,
            timestamp: request.timestamp,
        };
        Ok(TransitionOutcome { workflow, audit })
    }
}

fn reject_elapsed_wait(
    state: &WorkflowState,
    transition: WorkflowTransition,
    timestamp: WorkflowTimestamp,
) -> Result<(), TransitionError> {
    let deadline = match state {
        WorkflowState::CollectingAttachments { deadline }
            if !matches!(
                transition,
                WorkflowTransition::AttachmentCollectionTimedOut { .. }
                    | WorkflowTransition::StopWorkflow
                    | WorkflowTransition::FailWorkflow
            ) =>
        {
            Some(*deadline)
        }
        WorkflowState::WaitingForClarification { deadline, .. }
            if !matches!(
                transition,
                WorkflowTransition::ExpireWorkflow
                    | WorkflowTransition::StopWorkflow
                    | WorkflowTransition::FailWorkflow
            ) =>
        {
            Some(*deadline)
        }
        WorkflowState::WaitingForConfirmation { deadline }
            if !matches!(
                transition,
                WorkflowTransition::ExpireWorkflow
                    | WorkflowTransition::StopWorkflow
                    | WorkflowTransition::FailWorkflow
            ) =>
        {
            Some(*deadline)
        }
        _ => None,
    };

    if let Some(deadline) = deadline
        && deadline.has_elapsed(timestamp)
    {
        return Err(TransitionError::WaitDeadlineElapsed {
            deadline,
            attempted_at: timestamp,
        });
    }
    Ok(())
}

fn valid_future_deadline(
    deadline: WaitDeadline,
    timestamp: WorkflowTimestamp,
) -> Result<WaitDeadline, TransitionError> {
    if deadline.has_elapsed(timestamp) {
        Err(TransitionError::InvalidDeadline {
            deadline,
            timestamp,
        })
    } else {
        Ok(deadline)
    }
}

fn require_elapsed(
    deadline: WaitDeadline,
    timestamp: WorkflowTimestamp,
) -> Result<(), TransitionError> {
    if deadline.has_elapsed(timestamp) {
        Ok(())
    } else {
        Err(TransitionError::DeadlineNotReached {
            deadline,
            attempted_at: timestamp,
        })
    }
}

fn next_state(
    state: &WorkflowState,
    transition: WorkflowTransition,
    timestamp: WorkflowTimestamp,
) -> Result<WorkflowState, TransitionError> {
    let next = match (state, transition) {
        (
            WorkflowState::RequestAccepted,
            WorkflowTransition::BeginAttachmentCollection { deadline },
        ) => WorkflowState::CollectingAttachments {
            deadline: valid_future_deadline(deadline, timestamp)?,
        },
        (
            WorkflowState::CollectingAttachments { .. },
            WorkflowTransition::FinishAttachmentCollection,
        ) => WorkflowState::ExtractionStarted,
        (
            WorkflowState::CollectingAttachments { deadline },
            WorkflowTransition::AttachmentCollectionTimedOut {
                clarification_deadline,
            },
        ) => {
            require_elapsed(*deadline, timestamp)?;
            WorkflowState::WaitingForClarification {
                deadline: valid_future_deadline(clarification_deadline, timestamp)?,
                resume: ClarificationResume::AttachmentCollection,
            }
        }
        (WorkflowState::ExtractionStarted, WorkflowTransition::CompleteExtraction) => {
            WorkflowState::ExtractionCompleted
        }
        (WorkflowState::ExtractionCompleted, WorkflowTransition::StartCalculationOrDrafting) => {
            WorkflowState::CalculationOrDraftingStarted
        }
        (
            WorkflowState::CalculationOrDraftingStarted,
            WorkflowTransition::CompleteCalculationOrDrafting,
        ) => WorkflowState::CalculationOrDraftingCompleted,
        (
            WorkflowState::ExtractionCompleted,
            WorkflowTransition::RequestClarification { deadline },
        ) => WorkflowState::WaitingForClarification {
            deadline: valid_future_deadline(deadline, timestamp)?,
            resume: ClarificationResume::Extraction,
        },
        (
            WorkflowState::CalculationOrDraftingCompleted,
            WorkflowTransition::RequestClarification { deadline },
        ) => WorkflowState::WaitingForClarification {
            deadline: valid_future_deadline(deadline, timestamp)?,
            resume: ClarificationResume::CalculationOrDrafting,
        },
        (
            WorkflowState::WaitingForClarification {
                resume: ClarificationResume::AttachmentCollection,
                ..
            },
            WorkflowTransition::ResumeAttachmentCollection { deadline },
        ) => WorkflowState::CollectingAttachments {
            deadline: valid_future_deadline(deadline, timestamp)?,
        },
        (
            WorkflowState::WaitingForClarification {
                resume: ClarificationResume::Extraction,
                ..
            },
            WorkflowTransition::ProvideClarification,
        ) => WorkflowState::ExtractionStarted,
        (
            WorkflowState::WaitingForClarification {
                resume: ClarificationResume::CalculationOrDrafting,
                ..
            },
            WorkflowTransition::ProvideClarification,
        ) => WorkflowState::CalculationOrDraftingStarted,
        (
            WorkflowState::CalculationOrDraftingCompleted,
            WorkflowTransition::RequestConfirmation { deadline },
        ) => WorkflowState::WaitingForConfirmation {
            deadline: valid_future_deadline(deadline, timestamp)?,
        },
        (WorkflowState::WaitingForConfirmation { .. }, WorkflowTransition::ApplyCorrection) => {
            WorkflowState::CalculationOrDraftingStarted
        }
        (
            WorkflowState::WaitingForConfirmation { .. },
            WorkflowTransition::StartSheetOrDocWrite,
        ) => WorkflowState::SheetOrDocWriteStarted,
        (WorkflowState::SheetOrDocWriteStarted, WorkflowTransition::CompleteSheetOrDocWrite) => {
            WorkflowState::SheetOrDocWriteCompleted
        }
        (
            WorkflowState::WaitingForConfirmation { .. } | WorkflowState::SheetOrDocWriteCompleted,
            WorkflowTransition::StartPdfGeneration,
        ) => WorkflowState::PdfGenerationStarted,
        (WorkflowState::PdfGenerationStarted, WorkflowTransition::CompletePdfGeneration) => {
            WorkflowState::PdfGenerationCompleted
        }
        (
            WorkflowState::WaitingForConfirmation { .. },
            WorkflowTransition::StartCalendarOrEmailAction,
        ) => WorkflowState::CalendarOrEmailActionStarted,
        (
            WorkflowState::CalendarOrEmailActionStarted,
            WorkflowTransition::CompleteCalendarOrEmailAction,
        ) => WorkflowState::CalendarOrEmailActionCompleted,
        (
            WorkflowState::SheetOrDocWriteCompleted
            | WorkflowState::PdfGenerationCompleted
            | WorkflowState::CalendarOrEmailActionCompleted,
            WorkflowTransition::CompleteArtifactDelivery,
        ) => WorkflowState::ArtifactDeliveryCompleted,
        (WorkflowState::ArtifactDeliveryCompleted, WorkflowTransition::CompleteWorkflow) => {
            WorkflowState::Completed
        }
        (_, WorkflowTransition::FailWorkflow) => WorkflowState::Failed,
        (_, WorkflowTransition::StopWorkflow) => WorkflowState::Stopped,
        (
            WorkflowState::WaitingForClarification { deadline, .. }
            | WorkflowState::WaitingForConfirmation { deadline },
            WorkflowTransition::ExpireWorkflow,
        ) => {
            require_elapsed(*deadline, timestamp)?;
            WorkflowState::Expired
        }
        _ => {
            return Err(TransitionError::IllegalTransition {
                from: state.kind(),
                transition: transition.kind(),
            });
        }
    };
    Ok(next)
}
