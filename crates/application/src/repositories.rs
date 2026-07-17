use std::fmt::{Display, Formatter};

use domain::identity::{ConfirmationId, ParticipantId, TopicSessionId, WorkflowId};
use domain::{
    ConfirmationConsumption, ConfirmationCorrectionOutcome, ConfirmationIssueOutcome,
    ConfirmationRecord, TransitionOutcome, Workflow, WorkflowTimestamp,
};

use crate::ports::{
    OAuthStateDigest, OAuthStateRecord, ObjectClass, Page, PageToken, PortValueError,
    StorageRecordId, StoredObject,
};

pub const MAX_PAGE_SIZE: u16 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryValueError {
    InvalidPageSize { requested: u16 },
    InvalidInvoiceMonth,
    InvalidHistoryVersion { kind: &'static str },
    Port(PortValueError),
}

impl Display for RepositoryValueError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid repository value: {self:?}")
    }
}

impl std::error::Error for RepositoryValueError {}

impl From<PortValueError> for RepositoryValueError {
    fn from(value: PortValueError) -> Self {
        Self::Port(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    pub limit: u16,
    pub token: Option<PageToken>,
}

impl PageRequest {
    pub fn new(limit: u16, token: Option<PageToken>) -> Result<Self, RepositoryValueError> {
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(RepositoryValueError::InvalidPageSize { requested: limit });
        }
        Ok(Self { limit, token })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalWriteOutcome {
    Committed,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HistorySequence(u64);

impl HistorySequence {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCheckpoint {
    pub workflow_id: WorkflowId,
    pub sequence: HistorySequence,
    pub object: StoredObject,
    pub model_version: String,
    pub prompt_version: String,
    pub created_at: WorkflowTimestamp,
}

impl HistoryCheckpoint {
    pub fn validate(&self) -> Result<(), RepositoryValueError> {
        self.object.validate()?;
        if self.object.class != ObjectClass::SanitizedHistory {
            return Err(RepositoryValueError::Port(
                PortValueError::InvalidCharacters {
                    kind: "history object class",
                },
            ));
        }
        validate_history_version("model version", &self.model_version)?;
        validate_history_version("prompt version", &self.prompt_version)?;
        if self.object.workflow_id != self.workflow_id {
            return Err(RepositoryValueError::Port(
                PortValueError::InvalidCharacters {
                    kind: "history workflow binding",
                },
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InvoiceMonth(String);

impl InvoiceMonth {
    pub fn new(value: impl Into<String>) -> Result<Self, RepositoryValueError> {
        let value = value.into();
        let valid = match value.as_bytes() {
            [year_1, year_2, year_3, year_4, b'-', month_1, month_2] => {
                [year_1, year_2, year_3, year_4]
                    .into_iter()
                    .all(u8::is_ascii_digit)
                    && ((*month_1 == b'0' && (b'1'..=b'9').contains(month_2))
                        || (*month_1 == b'1' && (b'0'..=b'2').contains(month_2)))
            }
            _ => false,
        };
        if !valid {
            return Err(RepositoryValueError::InvalidInvoiceMonth);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageOperationClass {
    NewWorkflow,
    AiCall,
    ExternalWrite,
    Retry,
    Deployment,
    Status,
    Cancellation,
    FailureReporting,
    ArtifactRetrieval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageReservationState {
    Reserved,
    Settled,
    Released,
    ManualReview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageReservation {
    pub reservation_id: StorageRecordId,
    pub workflow_id: WorkflowId,
    pub invoice_month: InvoiceMonth,
    pub operation_class: UsageOperationClass,
    pub estimate_micro_inr: u64,
    pub state: UsageReservationState,
    pub pricing_version: StorageRecordId,
    pub created_at: WorkflowTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSnapshot {
    pub invoice_month: InvoiceMonth,
    pub settled_micro_inr: u64,
    pub reserved_micro_inr: u64,
    pub reconciled_micro_inr: u64,
    pub optimistic_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthStateConsumeOutcome {
    Consumed(OAuthStateRecord),
    MissingExpiredOrConsumed,
    ParticipantMismatch,
}

#[allow(async_fn_in_trait)]
pub trait WorkflowRepository {
    type Error: Display;

    async fn load(&self, workflow_id: &WorkflowId) -> Result<Option<Workflow>, Self::Error>;
    async fn load_by_topic(&self, topic: TopicSessionId) -> Result<Option<Workflow>, Self::Error>;
    async fn create(&self, workflow: &Workflow) -> Result<ConditionalWriteOutcome, Self::Error>;
    async fn commit_transition(
        &self,
        transition: &TransitionOutcome,
    ) -> Result<ConditionalWriteOutcome, Self::Error>;
}

#[allow(async_fn_in_trait)]
pub trait ConfirmationRepository {
    type Error: Display;

    async fn load(
        &self,
        workflow_id: &WorkflowId,
        confirmation_id: &ConfirmationId,
    ) -> Result<Option<ConfirmationRecord>, Self::Error>;
    async fn issue(
        &self,
        outcome: &ConfirmationIssueOutcome,
    ) -> Result<ConditionalWriteOutcome, Self::Error>;
    async fn consume(
        &self,
        outcome: &ConfirmationConsumption,
    ) -> Result<ConditionalWriteOutcome, Self::Error>;
    async fn correct(
        &self,
        outcome: &ConfirmationCorrectionOutcome,
    ) -> Result<ConditionalWriteOutcome, Self::Error>;
}

#[allow(async_fn_in_trait)]
pub trait HistoryRepository {
    type Error: Display;

    async fn append(
        &self,
        checkpoint: &HistoryCheckpoint,
    ) -> Result<ConditionalWriteOutcome, Self::Error>;
    async fn page(
        &self,
        workflow_id: &WorkflowId,
        request: &PageRequest,
    ) -> Result<Page<HistoryCheckpoint>, Self::Error>;
}

#[allow(async_fn_in_trait)]
pub trait ObjectMetadataRepository {
    type Error: Display;

    async fn record(&self, object: &StoredObject) -> Result<ConditionalWriteOutcome, Self::Error>;
    async fn page(
        &self,
        workflow_id: &WorkflowId,
        request: &PageRequest,
    ) -> Result<Page<StoredObject>, Self::Error>;
}

#[allow(async_fn_in_trait)]
pub trait OAuthStateRepository {
    type Error: Display;

    async fn create(
        &self,
        state: &OAuthStateRecord,
    ) -> Result<ConditionalWriteOutcome, Self::Error>;
    async fn consume(
        &self,
        digest: OAuthStateDigest,
        participant: ParticipantId,
        now: WorkflowTimestamp,
    ) -> Result<OAuthStateConsumeOutcome, Self::Error>;
}

#[allow(async_fn_in_trait)]
pub trait UsageRepository {
    type Error: Display;

    async fn load(&self, month: &InvoiceMonth) -> Result<Option<UsageSnapshot>, Self::Error>;
    async fn reserve(
        &self,
        expected_version: u64,
        reservation: &UsageReservation,
    ) -> Result<ConditionalWriteOutcome, Self::Error>;
    async fn update_reservation(
        &self,
        expected_version: u64,
        reservation: &UsageReservation,
        trusted_measured_micro_inr: Option<u64>,
    ) -> Result<ConditionalWriteOutcome, Self::Error>;
}

fn validate_history_version(kind: &'static str, value: &str) -> Result<(), RepositoryValueError> {
    if value.is_empty() || value.len() > 128 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(RepositoryValueError::InvalidHistoryVersion { kind });
    }
    Ok(())
}
