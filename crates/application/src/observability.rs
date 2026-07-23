#![deny(unsafe_code)]

//! Redacted structured observability contracts for Task 41.
//!
//! Metric dimensions are deliberately low-cardinality. Opaque workflow IDs are
//! included as log properties for correlation, never as CloudWatch dimensions.

use std::fmt::{self, Display, Formatter, Write};

use domain::BudgetBand;
use domain::identity::WorkflowId;
use domain::workflow::WorkflowStateKind;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::repositories::UsageOperationClass;

/// Environment label read from immutable function configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentLabel(String);

impl EnvironmentLabel {
    pub fn new(value: impl Into<String>) -> Result<Self, ObservabilityError> {
        let value = value.into();
        if !matches!(value.as_str(), "development" | "staging" | "production") {
            return Err(ObservabilityError::InvalidEnvironmentLabel);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostGuardOutcome {
    Permitted,
    Denied,
}

impl CostGuardOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Permitted => "permitted",
            Self::Denied => "denied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOutcome {
    Started,
    Completed,
    Failed,
}

impl StageOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostGuardFailureReason {
    InvalidEnvelope,
    InvalidProjection,
    UsageUnavailable,
    UsageUninitialized,
    InvoiceMonthMismatch,
    ConditionalConflict,
}

impl CostGuardFailureReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEnvelope => "invalid_envelope",
            Self::InvalidProjection => "invalid_projection",
            Self::UsageUnavailable => "usage_unavailable",
            Self::UsageUninitialized => "usage_uninitialized",
            Self::InvoiceMonthMismatch => "invoice_month_mismatch",
            Self::ConditionalConflict => "conditional_conflict",
        }
    }
}

/// One cost decision. `workflow_id` is a structured log property only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostGuardMetric {
    pub environment: EnvironmentLabel,
    pub workflow_id: WorkflowId,
    pub operation_class: UsageOperationClass,
    pub band: BudgetBand,
    pub outcome: CostGuardOutcome,
    pub warning_crossed: bool,
    pub projected_micro_inr: u64,
}

impl CostGuardMetric {
    pub fn to_json(&self) -> Value {
        json!({
            "_aws": emf_metadata("ProjectedMonthlyCostMicroInr"),
            "Environment": self.environment.as_str(),
            "OperationClass": operation_class_label(self.operation_class),
            "BudgetBand": budget_band_label(self.band),
            "Decision": self.outcome.as_str(),
            "ProjectedMonthlyCostMicroInr": self.projected_micro_inr,
            "event": "cost_guard_decision",
            "workflow_ref": workflow_reference(&self.workflow_id),
            "warning_crossed": self.warning_crossed,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageProgress {
    pub environment: EnvironmentLabel,
    pub workflow_id: WorkflowId,
    pub stage: WorkflowStateKind,
    pub outcome: StageOutcome,
}

impl StageProgress {
    pub fn to_json(&self) -> Value {
        let failed = u64::from(self.outcome == StageOutcome::Failed);
        json!({
            "_aws": emf_metadata("WorkflowStageFailureCount"),
            "Environment": self.environment.as_str(),
            "OperationClass": "workflow",
            "BudgetBand": "not_applicable",
            "Decision": self.outcome.as_str(),
            "WorkflowStageFailureCount": failed,
            "event": "workflow_stage_progress",
            "workflow_ref": workflow_reference(&self.workflow_id),
            "stage": workflow_stage_label(self.stage),
            "outcome": self.outcome.as_str(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualReviewNotice {
    pub environment: EnvironmentLabel,
    pub workflow_id: WorkflowId,
    pub operation_class: UsageOperationClass,
}

impl ManualReviewNotice {
    pub fn to_json(&self) -> Value {
        json!({
            "_aws": emf_metadata("ManualReviewCount"),
            "Environment": self.environment.as_str(),
            "OperationClass": operation_class_label(self.operation_class),
            "BudgetBand": "not_applicable",
            "Decision": "manual_review",
            "ManualReviewCount": 1,
            "event": "manual_review_required",
            "workflow_ref": workflow_reference(&self.workflow_id),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservabilityError {
    InvalidEnvironmentLabel,
}

impl Display for ObservabilityError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid environment label")
    }
}

impl std::error::Error for ObservabilityError {}

pub trait ObservabilitySink: Send + Sync {
    fn emit_cost_metric(&self, metric: &CostGuardMetric);
    fn emit_stage_progress(&self, progress: &StageProgress);
    fn emit_manual_review(&self, notice: &ManualReviewNotice);
    fn emit_cost_guard_failure(
        &self,
        environment: &EnvironmentLabel,
        workflow_id: &WorkflowId,
        reason: CostGuardFailureReason,
    );
}

#[derive(Debug, Clone, Copy)]
pub struct NoOpSink;

impl ObservabilitySink for NoOpSink {
    fn emit_cost_metric(&self, _metric: &CostGuardMetric) {}
    fn emit_stage_progress(&self, _progress: &StageProgress) {}
    fn emit_manual_review(&self, _notice: &ManualReviewNotice) {}
    fn emit_cost_guard_failure(
        &self,
        _environment: &EnvironmentLabel,
        _workflow_id: &WorkflowId,
        _reason: CostGuardFailureReason,
    ) {
    }
}

/// Emits CloudWatch Embedded Metric Format JSON without provider payloads.
#[derive(Debug, Clone, Copy)]
pub struct StdoutObservabilitySink;

impl ObservabilitySink for StdoutObservabilitySink {
    fn emit_cost_metric(&self, metric: &CostGuardMetric) {
        emit_json(&metric.to_json());
    }

    fn emit_stage_progress(&self, progress: &StageProgress) {
        emit_json(&progress.to_json());
    }

    fn emit_manual_review(&self, notice: &ManualReviewNotice) {
        emit_json(&notice.to_json());
    }

    fn emit_cost_guard_failure(
        &self,
        environment: &EnvironmentLabel,
        workflow_id: &WorkflowId,
        reason: CostGuardFailureReason,
    ) {
        emit_json(&json!({
            "_aws": emf_metadata("CostGuardFailureCount"),
            "Environment": environment.as_str(),
            "OperationClass": "cost_guard",
            "BudgetBand": "fail_closed",
            "Decision": "denied",
            "CostGuardFailureCount": 1,
            "event": "cost_guard_failure",
            "workflow_ref": workflow_reference(workflow_id),
            "reason": reason.as_str(),
        }));
    }
}

fn workflow_reference(workflow_id: &WorkflowId) -> String {
    let digest = Sha256::digest(workflow_id.as_str().as_bytes());
    let mut reference = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        let _ = write!(reference, "{byte:02x}");
    }
    reference
}

fn emit_json(value: &Value) {
    if let Ok(line) = serde_json::to_string(value) {
        println!("{line}");
    }
}

fn emf_metadata(metric_name: &'static str) -> Value {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    json!({
        "Timestamp": timestamp,
        "CloudWatchMetrics": [{
            "Namespace": "Novus/Operations",
            "Dimensions": [
                ["Environment"],
                ["Environment", "OperationClass", "BudgetBand", "Decision"]
            ],
            "Metrics": [{"Name": metric_name}],
        }],
    })
}

const fn operation_class_label(value: UsageOperationClass) -> &'static str {
    match value {
        UsageOperationClass::NewWorkflow => "new_workflow",
        UsageOperationClass::AiCall => "ai_call",
        UsageOperationClass::ExternalWrite => "external_write",
        UsageOperationClass::Retry => "retry",
        UsageOperationClass::Deployment => "deployment",
        UsageOperationClass::Status => "status",
        UsageOperationClass::Cancellation => "cancellation",
        UsageOperationClass::FailureReporting => "failure_reporting",
        UsageOperationClass::ArtifactRetrieval => "artifact_retrieval",
    }
}

const fn budget_band_label(value: BudgetBand) -> &'static str {
    match value {
        BudgetBand::Normal => "normal",
        BudgetBand::Warning => "warning",
        BudgetBand::IntakeSuspended => "intake_suspended",
        BudgetBand::OperationalReserve => "operational_reserve",
        BudgetBand::HardCap => "hard_cap",
    }
}

const fn workflow_stage_label(value: WorkflowStateKind) -> &'static str {
    match value {
        WorkflowStateKind::RequestAccepted => "request_accepted",
        WorkflowStateKind::CollectingAttachments => "collecting_attachments",
        WorkflowStateKind::ExtractionStarted => "extraction_started",
        WorkflowStateKind::ExtractionCompleted => "extraction_completed",
        WorkflowStateKind::CalculationOrDraftingStarted => "calculation_or_drafting_started",
        WorkflowStateKind::CalculationOrDraftingCompleted => "calculation_or_drafting_completed",
        WorkflowStateKind::WaitingForClarification => "waiting_for_clarification",
        WorkflowStateKind::WaitingForConfirmation => "waiting_for_confirmation",
        WorkflowStateKind::SheetOrDocWriteStarted => "sheet_or_doc_write_started",
        WorkflowStateKind::SheetOrDocWriteCompleted => "sheet_or_doc_write_completed",
        WorkflowStateKind::PdfGenerationStarted => "pdf_generation_started",
        WorkflowStateKind::PdfGenerationCompleted => "pdf_generation_completed",
        WorkflowStateKind::CalendarOrEmailActionStarted => "calendar_or_email_action_started",
        WorkflowStateKind::CalendarOrEmailActionCompleted => "calendar_or_email_action_completed",
        WorkflowStateKind::ArtifactDeliveryCompleted => "artifact_delivery_completed",
        WorkflowStateKind::Failed => "failed",
        WorkflowStateKind::Expired => "expired",
        WorkflowStateKind::Stopped => "stopped",
        WorkflowStateKind::Completed => "completed",
    }
}
