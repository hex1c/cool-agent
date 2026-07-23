#![deny(unsafe_code)]

use std::sync::Arc;

use application::cost_guard::{
    CostGuardError, CostGuardService, CostPolicyEvidence, ManualReviewRequest,
    OperationEnvelopeProvider, ReserveRequest, SettleMode, SettleRequest, TimeProvider,
};
use application::observability::{EnvironmentLabel, StdoutObservabilitySink};
use application::ports::StorageRecordId;
use application::repositories::{UsageOperationClass, UsageRepository};
use domain::BudgetThresholds;
use domain::identity::WorkflowId;
use domain::workflow::WorkflowTimestamp;
use serde::{Deserialize, Serialize};

use crate::EVENT_SCHEMA_VERSION;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CostGuardEvent {
    Reserve {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        #[serde(rename = "workflowId")]
        workflow_id: String,
        #[serde(rename = "reservationId")]
        reservation_id: String,
    },
    Settle {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        #[serde(rename = "workflowId")]
        workflow_id: String,
        #[serde(rename = "invoiceMonth")]
        invoice_month: String,
        #[serde(rename = "reservationId")]
        reservation_id: String,
    },
    ManualReview {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        #[serde(rename = "workflowId")]
        workflow_id: String,
        #[serde(rename = "invoiceMonth")]
        invoice_month: String,
        #[serde(rename = "reservationId")]
        reservation_id: String,
    },
    Snapshot {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
    },
}

#[derive(Debug, Serialize)]
pub struct CostGuardResult {
    pub authorized: bool,
    pub outcome: &'static str,
    #[serde(rename = "budgetBand", skip_serializing_if = "Option::is_none")]
    pub budget_band: Option<&'static str>,
    #[serde(rename = "warningCrossed")]
    pub warning_crossed: bool,
    #[serde(rename = "projectedMicroInr", skip_serializing_if = "Option::is_none")]
    pub projected_micro_inr: Option<u64>,
    #[serde(rename = "reservationState", skip_serializing_if = "Option::is_none")]
    pub reservation_state: Option<&'static str>,
    #[serde(rename = "invoiceMonth", skip_serializing_if = "Option::is_none")]
    pub invoice_month: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostGuardHandlerError {
    SchemaVersion,
    InvalidEvent,
    CostGuard,
}

impl CostGuardHandlerError {
    pub const fn result(self) -> CostGuardResult {
        CostGuardResult {
            authorized: false,
            outcome: match self {
                Self::SchemaVersion => "schema_version_mismatch",
                Self::InvalidEvent => "invalid_event",
                Self::CostGuard => "cost_guard_unavailable",
            },
            budget_band: None,
            warning_crossed: false,
            projected_micro_inr: None,
            reservation_state: None,
            invoice_month: None,
        }
    }
}

/// Code-reviewed whole-operation maxima. Monetary values never enter events.
///
/// `AiCall` covers every bounded extraction/drafting/clarification invocation
/// in one workflow. `ExternalWrite` covers all three permitted provider
/// attempts. Retries therefore reuse the same durable reservation rather than
/// incrementing an unbounded caller-supplied amount.
#[derive(Debug, Clone, Copy)]
pub struct ApprovedOperationEnvelopes;

impl OperationEnvelopeProvider for ApprovedOperationEnvelopes {
    fn estimate(&self, operation_class: UsageOperationClass) -> Option<u64> {
        Some(match operation_class {
            UsageOperationClass::NewWorkflow => 5_000_000,
            UsageOperationClass::AiCall => 50_000_000,
            UsageOperationClass::ExternalWrite => 10_000_000,
            UsageOperationClass::Retry => 5_000_000,
            UsageOperationClass::Deployment => 100_000,
            UsageOperationClass::Status => 1_000,
            UsageOperationClass::Cancellation => 10_000,
            UsageOperationClass::FailureReporting => 10_000,
            UsageOperationClass::ArtifactRetrieval => 50_000,
        })
    }

    fn reservation_horizon_seconds(&self, operation_class: UsageOperationClass) -> u64 {
        match operation_class {
            UsageOperationClass::NewWorkflow => 300,
            UsageOperationClass::AiCall => 900,
            UsageOperationClass::ExternalWrite | UsageOperationClass::Retry => 600,
            UsageOperationClass::Deployment => 1_800,
            UsageOperationClass::Status
            | UsageOperationClass::Cancellation
            | UsageOperationClass::FailureReporting
            | UsageOperationClass::ArtifactRetrieval => 60,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SystemTime;

impl TimeProvider for SystemTime {
    fn now(&self) -> WorkflowTimestamp {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        WorkflowTimestamp::from_unix_seconds(seconds)
    }
}

pub fn build_service(
    environment: &str,
    pricing_version: &str,
    pricing_approval_id: &str,
) -> Result<CostGuardService, CostGuardHandlerError> {
    let environment =
        EnvironmentLabel::new(environment).map_err(|_| CostGuardHandlerError::InvalidEvent)?;
    let thresholds = BudgetThresholds::new(240_000_000, 270_000_000, 300_000_000, 10_000_000)
        .map_err(|_| CostGuardHandlerError::InvalidEvent)?;
    Ok(CostGuardService::new(
        thresholds,
        2_000,
        Arc::new(ApprovedOperationEnvelopes),
        environment,
        Arc::new(StdoutObservabilitySink),
        Arc::new(SystemTime),
        CostPolicyEvidence {
            pricing_version: StorageRecordId::new(pricing_version)
                .map_err(|_| CostGuardHandlerError::InvalidEvent)?,
            pricing_approval_id: StorageRecordId::new(pricing_approval_id)
                .map_err(|_| CostGuardHandlerError::InvalidEvent)?,
            pricing_approved_at: WorkflowTimestamp::from_unix_seconds(1_784_764_800),
            pricing_max_age_seconds: 2_678_400,
            attribution_complete: true,
            reconciliation_max_age_seconds: 86_400,
        },
    ))
}

pub async fn process_cost_guard<R: UsageRepository>(
    service: &CostGuardService,
    repository: &R,
    current_invoice_month: &application::repositories::InvoiceMonth,
    pricing_version: &StorageRecordId,
    configured_operation_class: Option<UsageOperationClass>,
    event: &CostGuardEvent,
) -> Result<CostGuardResult, CostGuardHandlerError> {
    verify_schema(event)?;
    match event {
        CostGuardEvent::Reserve {
            workflow_id,
            reservation_id,
            ..
        } => {
            let request = ReserveRequest {
                workflow_id: parse_workflow_id(workflow_id)?,
                operation_class: configured_operation_class
                    .ok_or(CostGuardHandlerError::InvalidEvent)?,
                invoice_month: current_invoice_month.clone(),
                reservation_id: parse_record_id(reservation_id)?,
                pricing_version: pricing_version.clone(),
            };
            let outcome = service
                .reserve(repository, &request)
                .await
                .map_err(map_cost_error)?;
            Ok(CostGuardResult {
                authorized: matches!(
                    outcome.decision.authorization(),
                    domain::BudgetAuthorization::Permit
                ) && outcome.reservation.is_some(),
                outcome: if outcome.reservation.is_some() {
                    "reserved"
                } else {
                    "denied"
                },
                budget_band: Some(band_label(outcome.decision.band())),
                warning_crossed: outcome.decision.warning_crossed(),
                projected_micro_inr: Some(outcome.decision.projected_after_action_micro_inr()),
                reservation_state: outcome.reservation.as_ref().map(|_| "reserved"),
                invoice_month: Some(current_invoice_month.as_str().to_owned()),
            })
        }
        CostGuardEvent::Settle {
            workflow_id,
            invoice_month,
            reservation_id,
            ..
        } => {
            let outcome = service
                .settle(
                    repository,
                    &SettleRequest {
                        workflow_id: parse_workflow_id(workflow_id)?,
                        invoice_month: parse_month(invoice_month)?,
                        reservation_id: parse_record_id(reservation_id)?,
                        mode: SettleMode::Settled,
                    },
                )
                .await
                .map_err(map_cost_error)?;
            Ok(CostGuardResult {
                authorized: true,
                outcome: "settled",
                budget_band: Some(band_label(outcome.metric.band)),
                warning_crossed: false,
                projected_micro_inr: Some(outcome.metric.projected_micro_inr),
                reservation_state: Some(match outcome.reservation.state {
                    application::repositories::UsageReservationState::Settled => "settled",
                    application::repositories::UsageReservationState::Released => "released",
                    _ => "invalid",
                }),
                invoice_month: Some(outcome.reservation.invoice_month.as_str().to_owned()),
            })
        }
        CostGuardEvent::Snapshot { .. } => {
            let metric = service
                .observe(
                    repository,
                    &application::cost_guard::ObserveRequest {
                        workflow_id: WorkflowId::new("budget-heartbeat")
                            .map_err(|_| CostGuardHandlerError::InvalidEvent)?,
                        invoice_month: current_invoice_month.clone(),
                    },
                )
                .await
                .map_err(map_cost_error)?;
            Ok(CostGuardResult {
                authorized: true,
                outcome: "observed",
                budget_band: Some(band_label(metric.band)),
                warning_crossed: false,
                projected_micro_inr: Some(metric.projected_micro_inr),
                reservation_state: None,
                invoice_month: Some(current_invoice_month.as_str().to_owned()),
            })
        }
        CostGuardEvent::ManualReview {
            workflow_id,
            invoice_month,
            reservation_id,
            ..
        } => {
            service
                .mark_manual_review(
                    repository,
                    &ManualReviewRequest {
                        workflow_id: parse_workflow_id(workflow_id)?,
                        invoice_month: parse_month(invoice_month)?,
                        reservation_id: parse_record_id(reservation_id)?,
                    },
                )
                .await
                .map_err(map_cost_error)?;
            Ok(CostGuardResult {
                authorized: false,
                outcome: "manual_review",
                budget_band: None,
                warning_crossed: false,
                projected_micro_inr: None,
                reservation_state: Some("manual_review"),
                invoice_month: Some(invoice_month.clone()),
            })
        }
    }
}

fn verify_schema(event: &CostGuardEvent) -> Result<(), CostGuardHandlerError> {
    let schema = match event {
        CostGuardEvent::Reserve { schema_version, .. }
        | CostGuardEvent::Settle { schema_version, .. }
        | CostGuardEvent::ManualReview { schema_version, .. }
        | CostGuardEvent::Snapshot { schema_version } => schema_version,
    };
    if schema == EVENT_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(CostGuardHandlerError::SchemaVersion)
    }
}

fn parse_workflow_id(value: &str) -> Result<WorkflowId, CostGuardHandlerError> {
    if value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CostGuardHandlerError::InvalidEvent);
    }
    WorkflowId::new(value).map_err(|_| CostGuardHandlerError::InvalidEvent)
}

fn parse_month(
    value: &str,
) -> Result<application::repositories::InvoiceMonth, CostGuardHandlerError> {
    application::repositories::InvoiceMonth::new(value)
        .map_err(|_| CostGuardHandlerError::InvalidEvent)
}

fn parse_record_id(value: &str) -> Result<StorageRecordId, CostGuardHandlerError> {
    StorageRecordId::new(value).map_err(|_| CostGuardHandlerError::InvalidEvent)
}

pub fn configured_operation_class(
    value: Option<&str>,
) -> Result<Option<UsageOperationClass>, CostGuardHandlerError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let operation_class = match value {
        "new_workflow" => UsageOperationClass::NewWorkflow,
        "ai_call" => UsageOperationClass::AiCall,
        "external_write" => UsageOperationClass::ExternalWrite,
        "retry" => UsageOperationClass::Retry,
        "deployment" => UsageOperationClass::Deployment,
        "status" => UsageOperationClass::Status,
        "cancellation" => UsageOperationClass::Cancellation,
        "failure_reporting" => UsageOperationClass::FailureReporting,
        "artifact_retrieval" => UsageOperationClass::ArtifactRetrieval,
        _ => return Err(CostGuardHandlerError::InvalidEvent),
    };
    Ok(Some(operation_class))
}

fn map_cost_error(_error: CostGuardError) -> CostGuardHandlerError {
    CostGuardHandlerError::CostGuard
}

const fn band_label(band: domain::BudgetBand) -> &'static str {
    match band {
        domain::BudgetBand::Normal => "normal",
        domain::BudgetBand::Warning => "warning",
        domain::BudgetBand::IntakeSuspended => "intake_suspended",
        domain::BudgetBand::OperationalReserve => "operational_reserve",
        domain::BudgetBand::HardCap => "hard_cap",
    }
}
