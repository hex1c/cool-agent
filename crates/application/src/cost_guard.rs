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
    /// Maximum duration during which the operation can incur new cost.
    fn reservation_horizon_seconds(&self, operation_class: UsageOperationClass) -> u64;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostPolicyEvidence {
    pub pricing_version: StorageRecordId,
    pub pricing_approval_id: StorageRecordId,
    pub pricing_approved_at: WorkflowTimestamp,
    pub pricing_max_age_seconds: u64,
    pub attribution_complete: bool,
    pub reconciliation_max_age_seconds: u64,
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
    pub workflow_id: WorkflowId,
    pub invoice_month: InvoiceMonth,
    pub reservation_id: StorageRecordId,
    pub mode: SettleMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserveRequest {
    pub workflow_id: WorkflowId,
    pub invoice_month: InvoiceMonth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualReviewRequest {
    pub workflow_id: WorkflowId,
    pub invoice_month: InvoiceMonth,
    pub reservation_id: StorageRecordId,
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
    ReconciliationStale,
    AttributionIncomplete,
    PricingUnapproved,
    MonthRolloverUnsupported,
    ConditionalConflict,
    ReservationUnavailable,
    ReservationBindingMismatch,
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
            Self::ReconciliationStale
            | Self::AttributionIncomplete
            | Self::PricingUnapproved
            | Self::MonthRolloverUnsupported => CostGuardFailureReason::InvalidProjection,
            Self::ConditionalConflict => CostGuardFailureReason::ConditionalConflict,
            Self::ReservationUnavailable
            | Self::ReservationBindingMismatch
            | Self::InvalidReservationState => CostGuardFailureReason::InvalidProjection,
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
            Self::ReconciliationStale => "billing reconciliation is stale",
            Self::AttributionIncomplete => "shared-cost attribution is incomplete",
            Self::PricingUnapproved => "pricing configuration is not approved",
            Self::MonthRolloverUnsupported => {
                "operation could cross an invoice month without dual-month capacity"
            }
            Self::ConditionalConflict => "budget reservation changed concurrently",
            Self::ReservationUnavailable => "budget reservation is unavailable",
            Self::ReservationBindingMismatch => "budget reservation workflow binding mismatch",
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
    evidence: CostPolicyEvidence,
}

impl CostGuardService {
    pub fn new(
        thresholds: BudgetThresholds,
        safety_margin_basis_points: u16,
        envelope: Arc<dyn OperationEnvelopeProvider>,
        environment: EnvironmentLabel,
        sink: Arc<dyn ObservabilitySink>,
        time: Arc<dyn TimeProvider>,
        evidence: CostPolicyEvidence,
    ) -> Self {
        Self {
            thresholds,
            safety_margin_basis_points,
            envelope,
            environment,
            sink,
            time,
            evidence,
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
        if request.pricing_version != self.evidence.pricing_version {
            return Err(CostGuardError::PricingUnapproved);
        }
        self.ensure_operation_stays_in_month(request)?;
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
        if let Some(existing) = repository
            .load_reservation(&request.invoice_month, &request.reservation_id)
            .await
            .map_err(|_| CostGuardError::UsageUnavailable)?
        {
            if !same_reservation_inputs(&existing, request, estimate_with_margin)
                || existing.state != UsageReservationState::Reserved
            {
                return Err(CostGuardError::ConditionalConflict);
            }
            let decision = evaluate_budget(
                self.thresholds,
                BudgetEvaluation::new(
                    BudgetAction::ConfirmedOperation {
                        funding: ConfirmedOperationFunding::ExistingReservation,
                    },
                    current,
                    current,
                )?,
            );
            let metric = self.metric(request, decision, CostGuardOutcome::Permitted);
            self.sink.emit_cost_metric(&metric);
            return Ok(ReserveOutcome {
                decision,
                reservation: Some(existing),
                metric,
            });
        }
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
                let persisted = repository
                    .load_reservation(&request.invoice_month, &request.reservation_id)
                    .await
                    .map_err(|_| CostGuardError::UsageUnavailable)?
                    .ok_or(CostGuardError::ReservationUnavailable)?;
                if !same_logical_reservation(&persisted, &reservation) {
                    return Err(CostGuardError::ConditionalConflict);
                }
                metric.outcome = CostGuardOutcome::Permitted;
                self.sink.emit_cost_metric(&metric);
                Ok(ReserveOutcome {
                    decision,
                    reservation: Some(persisted),
                    metric,
                })
            }
            ConditionalWriteOutcome::Conflict => {
                let persisted = repository
                    .load_reservation(&request.invoice_month, &request.reservation_id)
                    .await
                    .map_err(|_| CostGuardError::UsageUnavailable)?
                    .ok_or(CostGuardError::ConditionalConflict)?;
                if !same_logical_reservation(&persisted, &reservation) {
                    return Err(CostGuardError::ConditionalConflict);
                }
                metric.outcome = CostGuardOutcome::Permitted;
                self.sink.emit_cost_metric(&metric);
                Ok(ReserveOutcome {
                    decision,
                    reservation: Some(persisted),
                    metric,
                })
            }
        }
    }

    pub async fn observe<R: UsageRepository>(
        &self,
        repository: &R,
        request: &ObserveRequest,
    ) -> Result<CostGuardMetric, CostGuardError> {
        let result = async {
            let snapshot = self
                .load_snapshot(repository, &request.invoice_month)
                .await?;
            let projection = effective_projection(&snapshot)?;
            let decision = evaluate_budget(
                self.thresholds,
                BudgetEvaluation::new(BudgetAction::Status, projection, projection)?,
            );
            let metric = CostGuardMetric {
                environment: self.environment.clone(),
                workflow_id: request.workflow_id.clone(),
                operation_class: UsageOperationClass::Status,
                band: decision.band(),
                outcome: CostGuardOutcome::Permitted,
                warning_crossed: false,
                projected_micro_inr: projection,
            };
            self.sink.emit_cost_metric(&metric);
            Ok(metric)
        }
        .await;
        if let Err(error) = result {
            self.emit_failure(&request.workflow_id, error);
        }
        result
    }

    pub async fn settle<R: UsageRepository>(
        &self,
        repository: &R,
        request: &SettleRequest,
    ) -> Result<SettleOutcome, CostGuardError> {
        let result = self.settle_inner(repository, request).await;
        if let Err(error) = result {
            self.emit_failure(&request.workflow_id, error);
        }
        result
    }

    async fn settle_inner<R: UsageRepository>(
        &self,
        repository: &R,
        request: &SettleRequest,
    ) -> Result<SettleOutcome, CostGuardError> {
        let reservation = self.load_reservation(repository, request).await?;
        let snapshot = self
            .load_snapshot(repository, &request.invoice_month)
            .await?;
        let (state, measured) = match request.mode {
            SettleMode::Settled => (
                UsageReservationState::Settled,
                Some(reservation.estimate_micro_inr),
            ),
            SettleMode::Released => (UsageReservationState::Released, Some(0)),
        };
        if reservation.state == state {
            return Ok(SettleOutcome {
                metric: self.settlement_metric(&reservation, &snapshot)?,
                reservation,
            });
        }
        if reservation.state != UsageReservationState::Reserved {
            return Err(CostGuardError::InvalidReservationState);
        }
        let updated = UsageReservation {
            state,
            ..reservation
        };
        match repository
            .update_reservation(snapshot.optimistic_version, &updated, measured)
            .await
            .map_err(|_| CostGuardError::UsageUnavailable)?
        {
            ConditionalWriteOutcome::Committed => {}
            ConditionalWriteOutcome::Conflict => return Err(CostGuardError::ConditionalConflict),
        }
        let updated_snapshot = self
            .load_snapshot(repository, &request.invoice_month)
            .await?;
        let metric = self.settlement_metric(&updated, &updated_snapshot)?;
        Ok(SettleOutcome {
            reservation: updated,
            metric,
        })
    }

    pub async fn mark_manual_review<R: UsageRepository>(
        &self,
        repository: &R,
        request: &ManualReviewRequest,
    ) -> Result<(), CostGuardError> {
        let result = self.mark_manual_review_inner(repository, request).await;
        if let Err(error) = result {
            self.emit_failure(&request.workflow_id, error);
        }
        result
    }

    async fn mark_manual_review_inner<R: UsageRepository>(
        &self,
        repository: &R,
        request: &ManualReviewRequest,
    ) -> Result<(), CostGuardError> {
        let reservation = repository
            .load_reservation(&request.invoice_month, &request.reservation_id)
            .await
            .map_err(|_| CostGuardError::UsageUnavailable)?
            .ok_or(CostGuardError::ReservationUnavailable)?;
        if reservation.workflow_id != request.workflow_id {
            return Err(CostGuardError::ReservationBindingMismatch);
        }
        if reservation.state == UsageReservationState::ManualReview {
            self.sink.emit_manual_review(&ManualReviewNotice {
                environment: self.environment.clone(),
                workflow_id: reservation.workflow_id,
                operation_class: reservation.operation_class,
            });
            return Ok(());
        }
        if reservation.state != UsageReservationState::Reserved {
            return Err(CostGuardError::InvalidReservationState);
        }
        let snapshot = self
            .load_snapshot(repository, &request.invoice_month)
            .await?;
        let updated = UsageReservation {
            state: UsageReservationState::ManualReview,
            ..reservation
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
            workflow_id: updated.workflow_id.clone(),
            operation_class: updated.operation_class,
        });
        Ok(())
    }

    async fn load_reservation<R: UsageRepository>(
        &self,
        repository: &R,
        request: &SettleRequest,
    ) -> Result<UsageReservation, CostGuardError> {
        let reservation = repository
            .load_reservation(&request.invoice_month, &request.reservation_id)
            .await
            .map_err(|_| CostGuardError::UsageUnavailable)?
            .ok_or(CostGuardError::ReservationUnavailable)?;
        if reservation.workflow_id != request.workflow_id {
            return Err(CostGuardError::ReservationBindingMismatch);
        }
        Ok(reservation)
    }

    async fn load_snapshot<R: UsageRepository>(
        &self,
        repository: &R,
        invoice_month: &InvoiceMonth,
    ) -> Result<UsageSnapshot, CostGuardError> {
        let snapshot = match repository
            .load(invoice_month)
            .await
            .map_err(|_| CostGuardError::UsageUnavailable)?
        {
            Some(snapshot) => snapshot,
            None => {
                let now = self.time.now();
                let initial = UsageSnapshot {
                    invoice_month: invoice_month.clone(),
                    settled_micro_inr: 0,
                    reserved_micro_inr: 0,
                    reconciled_micro_inr: 0,
                    reconciliation_observed_at: now,
                    attribution_complete: self.evidence.attribution_complete,
                    pricing_version: self.evidence.pricing_version.clone(),
                    pricing_approval_id: self.evidence.pricing_approval_id.clone(),
                    pricing_approved_at: self.evidence.pricing_approved_at,
                    optimistic_version: 0,
                };
                repository
                    .initialize(&initial)
                    .await
                    .map_err(|_| CostGuardError::UsageUnavailable)?;
                repository
                    .load(invoice_month)
                    .await
                    .map_err(|_| CostGuardError::UsageUnavailable)?
                    .ok_or(CostGuardError::UsageUninitialized)?
            }
        };
        if &snapshot.invoice_month != invoice_month {
            return Err(CostGuardError::InvoiceMonthMismatch);
        }
        let now = self.time.now().as_unix_seconds();
        let observed = snapshot.reconciliation_observed_at.as_unix_seconds();
        if observed > now || now - observed > self.evidence.reconciliation_max_age_seconds {
            return Err(CostGuardError::ReconciliationStale);
        }
        if !snapshot.attribution_complete {
            return Err(CostGuardError::AttributionIncomplete);
        }
        let approved = snapshot.pricing_approved_at.as_unix_seconds();
        if snapshot.pricing_version != self.evidence.pricing_version
            || snapshot.pricing_approval_id != self.evidence.pricing_approval_id
            || snapshot.pricing_approved_at != self.evidence.pricing_approved_at
            || approved > now
            || now - approved > self.evidence.pricing_max_age_seconds
        {
            return Err(CostGuardError::PricingUnapproved);
        }
        Ok(snapshot)
    }

    fn ensure_operation_stays_in_month(
        &self,
        request: &ReserveRequest,
    ) -> Result<(), CostGuardError> {
        let now = self.time.now().as_unix_seconds();
        let end = now
            .checked_add(
                self.envelope
                    .reservation_horizon_seconds(request.operation_class),
            )
            .ok_or(CostGuardError::ProjectionOverflow)?;
        let now = i64::try_from(now).map_err(|_| CostGuardError::ProjectionOverflow)?;
        let end = i64::try_from(end).map_err(|_| CostGuardError::ProjectionOverflow)?;
        let now = time::OffsetDateTime::from_unix_timestamp(now)
            .map_err(|_| CostGuardError::ProjectionOverflow)?;
        let end = time::OffsetDateTime::from_unix_timestamp(end)
            .map_err(|_| CostGuardError::ProjectionOverflow)?;
        let current_month = format!("{:04}-{:02}", now.year(), now.month() as u8);
        if request.invoice_month.as_str() != current_month {
            return Err(CostGuardError::InvoiceMonthMismatch);
        }
        if now.year() != end.year() || now.month() != end.month() {
            return Err(CostGuardError::MonthRolloverUnsupported);
        }
        Ok(())
    }

    fn settlement_metric(
        &self,
        reservation: &UsageReservation,
        snapshot: &UsageSnapshot,
    ) -> Result<CostGuardMetric, CostGuardError> {
        let projection = effective_projection(snapshot)?;
        let decision = evaluate_budget(
            self.thresholds,
            BudgetEvaluation::new(BudgetAction::Status, projection, projection)?,
        );
        let metric = CostGuardMetric {
            environment: self.environment.clone(),
            workflow_id: reservation.workflow_id.clone(),
            operation_class: reservation.operation_class,
            band: decision.band(),
            outcome: CostGuardOutcome::Permitted,
            warning_crossed: false,
            projected_micro_inr: decision.projected_after_action_micro_inr(),
        };
        self.sink.emit_cost_metric(&metric);
        Ok(metric)
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

fn same_reservation_inputs(
    reservation: &UsageReservation,
    request: &ReserveRequest,
    estimate_micro_inr: u64,
) -> bool {
    reservation.reservation_id == request.reservation_id
        && reservation.workflow_id == request.workflow_id
        && reservation.invoice_month == request.invoice_month
        && reservation.operation_class == request.operation_class
        && reservation.estimate_micro_inr == estimate_micro_inr
        && reservation.pricing_version == request.pricing_version
}

fn same_logical_reservation(left: &UsageReservation, right: &UsageReservation) -> bool {
    left.reservation_id == right.reservation_id
        && left.workflow_id == right.workflow_id
        && left.invoice_month == right.invoice_month
        && left.operation_class == right.operation_class
        && left.estimate_micro_inr == right.estimate_micro_inr
        && left.state == right.state
        && left.pricing_version == right.pricing_version
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
