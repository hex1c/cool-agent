#![deny(unsafe_code)]

//! Fail-closed application cost enforcement for Task 41 and ADR 0002.

use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use domain::identity::WorkflowId;
use domain::workflow::WorkflowTimestamp;
use domain::{
    BudgetAction, BudgetAuthorization, BudgetDecision, BudgetEvaluation, BudgetPolicyError,
    BudgetThresholds, ConfirmedOperationFunding, apply_safety_margin, evaluate_budget,
};

use crate::observability::{
    CostGuardFailureReason, CostGuardMetric, CostGuardOutcome, EnvironmentLabel,
    ManualReviewNotice, ObservabilitySink,
};
use crate::ports::StorageRecordId;
use crate::repositories::{
    ConditionalWriteOutcome, InvoiceMonth, UsageOperationClass, UsageRepository, UsageReservation,
    UsageReservationState, UsageSnapshot,
};

pub trait TimeProvider: Send + Sync {
    fn now(&self) -> WorkflowTimestamp;
}

pub trait OperationEnvelopeProvider: Send + Sync {
    /// Returns an approved, conservative estimate in micro-INR.
    fn estimate(&self, operation_class: UsageOperationClass) -> Option<u64>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReserveRequest {
    pub workflow_id: WorkflowId,
    pub operation_class: UsageOperationClass,
    pub invoice_month: InvoiceMonth,
    pub reservation_id: StorageRecordId,
    pub pricing_version: StorageRecordId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleMode {
    /// Conservatively consume the full approved reservation.
    Settled,
    /// Release only when controlled adapter evidence proves no cost occurred.
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleRequest {
    pub reservation: UsageReservation,
    pub mode: SettleMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReserveOutcome {
    pub decision: BudgetDecision,
    /// Present only after the conditional reservation write commits.
    pub reservation: Option<UsageReservation>,
    pub metric: CostGuardMetric,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleOutcome {
    pub reservation: UsageReservation,
    pub metric: CostGuardMetric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostGuardError {
    InvalidEnvelope,
    ProjectionOverflow,
    UsageUnavailable,
    UsageUninitialized,
    InvoiceMonthMismatch,
    ConditionalConflict,
    InvalidReservationState,
}

impl CostGuardError {
    const fn failure_reason(self) -> CostGuardFailureReason {
        match self {
            Self::InvalidEnvelope => CostGuardFailureReason::InvalidEnvelope,
            Self::ProjectionOverflow => CostGuardFailureReason::InvalidProjection,
            Self::UsageUnavailable => CostGuardFailureReason::UsageUnavailable,
            Self::UsageUninitialized => CostGuardFailureReason::UsageUninitialized,
            Self::InvoiceMonthMismatch => CostGuardFailureReason::InvoiceMonthMismatch,
            Self::ConditionalConflict => CostGuardFailureReason::ConditionalConflict,
            Self::InvalidReservationState => CostGuardFailureReason::InvalidProjection,
        }
    }
}

impl Display for CostGuardError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidEnvelope => "cost guard envelope is unavailable or invalid",
            Self::ProjectionOverflow => "cost guard projection overflowed",
            Self::UsageUnavailable => "authoritative usage is unavailable",
            Self::UsageUninitialized => "authoritative usage is not initialized",
            Self::InvoiceMonthMismatch => "usage snapshot invoice month mismatch",
            Self::ConditionalConflict => "budget reservation changed concurrently",
            Self::InvalidReservationState => "reservation state cannot be changed",
        })
    }
}

impl std::error::Error for CostGuardError {}

impl From<BudgetPolicyError> for CostGuardError {
    fn from(_error: BudgetPolicyError) -> Self {
        Self::ProjectionOverflow
    }
}

pub struct CostGuardService {
    thresholds: BudgetThresholds,
    safety_margin_basis_points: u16,
    envelope: Arc<dyn OperationEnvelopeProvider>,
    environment: EnvironmentLabel,
    sink: Arc<dyn ObservabilitySink>,
    time: Arc<dyn TimeProvider>,
}

impl CostGuardService {
    pub fn new(
        thresholds: BudgetThresholds,
        safety_margin_basis_points: u16,
        envelope: Arc<dyn OperationEnvelopeProvider>,
        environment: EnvironmentLabel,
        sink: Arc<dyn ObservabilitySink>,
        time: Arc<dyn TimeProvider>,
    ) -> Self {
        Self {
            thresholds,
            safety_margin_basis_points,
            envelope,
            environment,
            sink,
            time,
        }
    }

    pub async fn reserve<R: UsageRepository>(
        &self,
        repository: &R,
        request: &ReserveRequest,
    ) -> Result<ReserveOutcome, CostGuardError> {
        let result = self.reserve_inner(repository, request).await;
        if let Err(error) = result {
            self.emit_failure(&request.workflow_id, error);
        }
        result
    }

    async fn reserve_inner<R: UsageRepository>(
        &self,
        repository: &R,
        request: &ReserveRequest,
    ) -> Result<ReserveOutcome, CostGuardError> {
        let estimate = self
            .envelope
            .estimate(request.operation_class)
            .filter(|value| *value > 0)
            .ok_or(CostGuardError::InvalidEnvelope)?;
        let estimate_with_margin = apply_safety_margin(estimate, self.safety_margin_basis_points)?;
        let snapshot = self
            .load_snapshot(repository, &request.invoice_month)
            .await?;
        let current = effective_projection(&snapshot)?;
        let projected = current
            .checked_add(estimate_with_margin)
            .ok_or(CostGuardError::ProjectionOverflow)?;
        let evaluation =
            BudgetEvaluation::new(budget_action(request.operation_class), current, projected)?;
        let decision = evaluate_budget(self.thresholds, evaluation);

        let denied = matches!(decision.authorization(), BudgetAuthorization::Deny(_));
        let mut metric = self.metric(request, decision, CostGuardOutcome::Denied);
        if denied {
            self.sink.emit_cost_metric(&metric);
            return Ok(ReserveOutcome {
                decision,
                reservation: None,
                metric,
            });
        }

        let reservation = UsageReservation {
            reservation_id: request.reservation_id.clone(),
            workflow_id: request.workflow_id.clone(),
            invoice_month: request.invoice_month.clone(),
            operation_class: request.operation_class,
            estimate_micro_inr: estimate_with_margin,
            state: UsageReservationState::Reserved,
            pricing_version: request.pricing_version.clone(),
            created_at: self.time.now(),
        };
        match repository
            .reserve(snapshot.optimistic_version, &reservation)
            .await
            .map_err(|_| CostGuardError::UsageUnavailable)?
        {
            ConditionalWriteOutcome::Committed => {
                metric.outcome = CostGuardOutcome::Permitted;
                self.sink.emit_cost_metric(&metric);
                Ok(ReserveOutcome {
                    decision,
                    reservation: Some(reservation),
                    metric,
                })
            }
            ConditionalWriteOutcome::Conflict => Err(CostGuardError::ConditionalConflict),
        }
    }

    pub async fn settle<R: UsageRepository>(
        &self,
        repository: &R,
        request: &SettleRequest,
    ) -> Result<SettleOutcome, CostGuardError> {
        let result = self.settle_inner(repository, request).await;
        if let Err(error) = result {
            self.emit_failure(&request.reservation.workflow_id, error);
        }
        result
    }

    async fn settle_inner<R: UsageRepository>(
        &self,
        repository: &R,
        request: &SettleRequest,
    ) -> Result<SettleOutcome, CostGuardError> {
        if request.reservation.state != UsageReservationState::Reserved {
            return Err(CostGuardError::InvalidReservationState);
        }
        let snapshot = self
            .load_snapshot(repository, &request.reservation.invoice_month)
            .await?;
        let (state, measured) = match request.mode {
            SettleMode::Settled => (
                UsageReservationState::Settled,
                Some(request.reservation.estimate_micro_inr),
            ),
            SettleMode::Released => (UsageReservationState::Released, Some(0)),
        };
        let updated = UsageReservation {
            state,
            ..request.reservation.clone()
        };
        match repository
            .update_reservation(snapshot.optimistic_version, &updated, measured)
            .await
            .map_err(|_| CostGuardError::UsageUnavailable)?
        {
            ConditionalWriteOutcome::Committed => {}
            ConditionalWriteOutcome::Conflict => return Err(CostGuardError::ConditionalConflict),
        }
        let decision = evaluate_budget(
            self.thresholds,
            BudgetEvaluation::new(
                BudgetAction::Status,
                effective_projection(&snapshot)?,
                effective_projection(&snapshot)?,
            )?,
        );
        let metric = CostGuardMetric {
            environment: self.environment.clone(),
            workflow_id: updated.workflow_id.clone(),
            operation_class: updated.operation_class,
            band: decision.band(),
            outcome: CostGuardOutcome::Permitted,
            warning_crossed: false,
            projected_micro_inr: decision.projected_after_action_micro_inr(),
        };
        self.sink.emit_cost_metric(&metric);
        Ok(SettleOutcome {
            reservation: updated,
            metric,
        })
    }

    pub async fn mark_manual_review<R: UsageRepository>(
        &self,
        repository: &R,
        reservation: &UsageReservation,
    ) -> Result<(), CostGuardError> {
        let result = self.mark_manual_review_inner(repository, reservation).await;
        if let Err(error) = result {
            self.emit_failure(&reservation.workflow_id, error);
        }
        result
    }

    async fn mark_manual_review_inner<R: UsageRepository>(
        &self,
        repository: &R,
        reservation: &UsageReservation,
    ) -> Result<(), CostGuardError> {
        if reservation.state != UsageReservationState::Reserved {
            return Err(CostGuardError::InvalidReservationState);
        }
        let snapshot = self
            .load_snapshot(repository, &reservation.invoice_month)
            .await?;
        let updated = UsageReservation {
            state: UsageReservationState::ManualReview,
            ..reservation.clone()
        };
        match repository
            .update_reservation(snapshot.optimistic_version, &updated, None)
            .await
            .map_err(|_| CostGuardError::UsageUnavailable)?
        {
            ConditionalWriteOutcome::Committed => {}
            ConditionalWriteOutcome::Conflict => return Err(CostGuardError::ConditionalConflict),
        }
        self.sink.emit_manual_review(&ManualReviewNotice {
            environment: self.environment.clone(),
            workflow_id: reservation.workflow_id.clone(),
            operation_class: reservation.operation_class,
        });
        Ok(())
    }

    async fn load_snapshot<R: UsageRepository>(
        &self,
        repository: &R,
        invoice_month: &InvoiceMonth,
    ) -> Result<UsageSnapshot, CostGuardError> {
        let snapshot = repository
            .load(invoice_month)
            .await
            .map_err(|_| CostGuardError::UsageUnavailable)?
            .ok_or(CostGuardError::UsageUninitialized)?;
        if &snapshot.invoice_month != invoice_month {
            return Err(CostGuardError::InvoiceMonthMismatch);
        }
        Ok(snapshot)
    }

    fn metric(
        &self,
        request: &ReserveRequest,
        decision: BudgetDecision,
        outcome: CostGuardOutcome,
    ) -> CostGuardMetric {
        CostGuardMetric {
            environment: self.environment.clone(),
            workflow_id: request.workflow_id.clone(),
            operation_class: request.operation_class,
            band: decision.band(),
            outcome,
            warning_crossed: decision.warning_crossed(),
            projected_micro_inr: decision.projected_after_action_micro_inr(),
        }
    }

    fn emit_failure(&self, workflow_id: &WorkflowId, error: CostGuardError) {
        self.sink
            .emit_cost_guard_failure(&self.environment, workflow_id, error.failure_reason());
    }
}

fn effective_projection(snapshot: &UsageSnapshot) -> Result<u64, CostGuardError> {
    let application_projection = snapshot
        .settled_micro_inr
        .checked_add(snapshot.reserved_micro_inr)
        .ok_or(CostGuardError::ProjectionOverflow)?;
    Ok(application_projection.max(snapshot.reconciled_micro_inr))
}

const fn budget_action(operation_class: UsageOperationClass) -> BudgetAction {
    match operation_class {
        UsageOperationClass::NewWorkflow => BudgetAction::NewWorkflowIntake,
        UsageOperationClass::AiCall
        | UsageOperationClass::ExternalWrite
        | UsageOperationClass::Retry => BudgetAction::ConfirmedOperation {
            funding: ConfirmedOperationFunding::RequiresNewReservation,
        },
        UsageOperationClass::Deployment => BudgetAction::Deployment,
        UsageOperationClass::Status => BudgetAction::Status,
        UsageOperationClass::Cancellation => BudgetAction::Cancellation,
        UsageOperationClass::FailureReporting => BudgetAction::FailureReporting,
        UsageOperationClass::ArtifactRetrieval => BudgetAction::ArtifactRetrieval,
    }
}
