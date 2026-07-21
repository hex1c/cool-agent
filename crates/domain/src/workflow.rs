use serde::{Deserialize, Serialize};

use crate::identity::{ParticipantId, TopicSessionId, WorkflowId};

/// Monotonically increasing optimistic-lock revision of a workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowRevision(u64);

impl WorkflowRevision {
    pub const INITIAL: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Domain timestamp represented as Unix epoch seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowTimestamp(u64);

impl WorkflowTimestamp {
    pub const fn from_unix_seconds(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_unix_seconds(self) -> u64 {
        self.0
    }
}

/// A deadline after which a human wait can no longer be resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WaitDeadline(WorkflowTimestamp);

impl WaitDeadline {
    pub const fn at(timestamp: WorkflowTimestamp) -> Self {
        Self(timestamp)
    }

    pub const fn timestamp(self) -> WorkflowTimestamp {
        self.0
    }

    pub const fn has_elapsed(self, now: WorkflowTimestamp) -> bool {
        now.0 >= self.0.0
    }
}

/// Processing stage to resume after a clarification is supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClarificationResume {
    AttachmentCollection,
    Extraction,
    CalculationOrDrafting,
}

/// Every user-visible progress stage and terminal outcome required by the PRD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum WorkflowState {
    RequestAccepted,
    CollectingAttachments {
        deadline: WaitDeadline,
    },
    ExtractionStarted,
    ExtractionCompleted,
    CalculationOrDraftingStarted,
    CalculationOrDraftingCompleted,
    WaitingForClarification {
        deadline: WaitDeadline,
        resume: ClarificationResume,
    },
    WaitingForConfirmation {
        deadline: WaitDeadline,
    },
    SheetOrDocWriteStarted,
    SheetOrDocWriteCompleted,
    PdfGenerationStarted,
    PdfGenerationCompleted,
    CalendarOrEmailActionStarted,
    CalendarOrEmailActionCompleted,
    ArtifactDeliveryCompleted,
    Failed,
    Expired,
    Stopped,
    Completed,
}

impl WorkflowState {
    pub const fn kind(&self) -> WorkflowStateKind {
        match self {
            Self::RequestAccepted => WorkflowStateKind::RequestAccepted,
            Self::CollectingAttachments { .. } => WorkflowStateKind::CollectingAttachments,
            Self::ExtractionStarted => WorkflowStateKind::ExtractionStarted,
            Self::ExtractionCompleted => WorkflowStateKind::ExtractionCompleted,
            Self::CalculationOrDraftingStarted => WorkflowStateKind::CalculationOrDraftingStarted,
            Self::CalculationOrDraftingCompleted => {
                WorkflowStateKind::CalculationOrDraftingCompleted
            }
            Self::WaitingForClarification { .. } => WorkflowStateKind::WaitingForClarification,
            Self::WaitingForConfirmation { .. } => WorkflowStateKind::WaitingForConfirmation,
            Self::SheetOrDocWriteStarted => WorkflowStateKind::SheetOrDocWriteStarted,
            Self::SheetOrDocWriteCompleted => WorkflowStateKind::SheetOrDocWriteCompleted,
            Self::PdfGenerationStarted => WorkflowStateKind::PdfGenerationStarted,
            Self::PdfGenerationCompleted => WorkflowStateKind::PdfGenerationCompleted,
            Self::CalendarOrEmailActionStarted => WorkflowStateKind::CalendarOrEmailActionStarted,
            Self::CalendarOrEmailActionCompleted => {
                WorkflowStateKind::CalendarOrEmailActionCompleted
            }
            Self::ArtifactDeliveryCompleted => WorkflowStateKind::ArtifactDeliveryCompleted,
            Self::Failed => WorkflowStateKind::Failed,
            Self::Expired => WorkflowStateKind::Expired,
            Self::Stopped => WorkflowStateKind::Stopped,
            Self::Completed => WorkflowStateKind::Completed,
        }
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Failed | Self::Expired | Self::Stopped | Self::Completed
        )
    }
}

/// Data-free state discriminator used in audit records and errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStateKind {
    RequestAccepted,
    CollectingAttachments,
    ExtractionStarted,
    ExtractionCompleted,
    CalculationOrDraftingStarted,
    CalculationOrDraftingCompleted,
    WaitingForClarification,
    WaitingForConfirmation,
    SheetOrDocWriteStarted,
    SheetOrDocWriteCompleted,
    PdfGenerationStarted,
    PdfGenerationCompleted,
    CalendarOrEmailActionStarted,
    CalendarOrEmailActionCompleted,
    ArtifactDeliveryCompleted,
    Failed,
    Expired,
    Stopped,
    Completed,
}

/// Pure aggregate root for one topic-bound workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    id: WorkflowId,
    topic: TopicSessionId,
    owner: ParticipantId,
    state: WorkflowState,
    revision: WorkflowRevision,
    updated_at: WorkflowTimestamp,
}

impl Workflow {
    pub fn new(
        id: WorkflowId,
        topic: TopicSessionId,
        owner: ParticipantId,
        accepted_at: WorkflowTimestamp,
    ) -> Self {
        Self {
            id,
            topic,
            owner,
            state: WorkflowState::RequestAccepted,
            revision: WorkflowRevision::INITIAL,
            updated_at: accepted_at,
        }
    }

    pub fn id(&self) -> &WorkflowId {
        &self.id
    }

    pub const fn topic(&self) -> TopicSessionId {
        self.topic
    }

    pub const fn owner(&self) -> ParticipantId {
        self.owner
    }

    pub const fn state(&self) -> &WorkflowState {
        &self.state
    }

    pub const fn revision(&self) -> WorkflowRevision {
        self.revision
    }

    pub const fn updated_at(&self) -> WorkflowTimestamp {
        self.updated_at
    }

    pub(crate) fn with_transition(
        &self,
        state: WorkflowState,
        revision: WorkflowRevision,
        updated_at: WorkflowTimestamp,
    ) -> Self {
        Self {
            id: self.id.clone(),
            topic: self.topic,
            owner: self.owner,
            state,
            revision,
            updated_at,
        }
    }
}
