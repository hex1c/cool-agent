#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use std::fmt::{self, Display, Formatter};
use std::sync::{Arc, Mutex};

use application::cost_guard::{
    CostGuardError, CostGuardService, CostPolicyEvidence, ManualReviewRequest,
    OperationEnvelopeProvider, ReserveRequest, SettleMode, SettleRequest, TimeProvider,
};
use application::observability::{
    CostGuardFailureReason, CostGuardMetric, EnvironmentLabel, ManualReviewNotice,
    ObservabilitySink, StageOutcome, StageProgress,
};
use application::ports::StorageRecordId;
use application::repositories::{
    ConditionalWriteOutcome, InvoiceMonth, UsageOperationClass, UsageRepository, UsageReservation,
    UsageReservationState, UsageSnapshot,
};
use domain::identity::WorkflowId;
use domain::workflow::{WorkflowStateKind, WorkflowTimestamp};
use domain::{BudgetAuthorization, BudgetBand, BudgetDenialReason, BudgetThresholds};

#[derive(Debug, Clone, Copy)]
struct FixedEnvelope;

impl OperationEnvelopeProvider for FixedEnvelope {
    fn estimate(&self, operation_class: UsageOperationClass) -> Option<u64> {
        Some(match operation_class {
            UsageOperationClass::NewWorkflow => 5_000_000,
            UsageOperationClass::Status => 1_000,
            _ => 10_000_000,
        })
    }

    fn reservation_horizon_seconds(&self, _operation_class: UsageOperationClass) -> u64 {
        300
    }
}

#[derive(Debug, Clone, Copy)]
struct MissingEnvelope;

impl OperationEnvelopeProvider for MissingEnvelope {
    fn estimate(&self, _operation_class: UsageOperationClass) -> Option<u64> {
        None
    }

    fn reservation_horizon_seconds(&self, _operation_class: UsageOperationClass) -> u64 {
        300
    }
}

#[derive(Debug, Clone, Copy)]
struct FixedTime;

impl TimeProvider for FixedTime {
    fn now(&self) -> WorkflowTimestamp {
        WorkflowTimestamp::from_unix_seconds(1_786_752_000)
    }
}

#[derive(Debug, Clone, Copy)]
struct EndOfMonthTime;

impl TimeProvider for EndOfMonthTime {
    fn now(&self) -> WorkflowTimestamp {
        WorkflowTimestamp::from_unix_seconds(1_788_220_740)
    }
}

#[derive(Debug, Clone, Copy)]
struct FakeError;

impl Display for FakeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("fake usage error")
    }
}

#[derive(Debug)]
struct FakeUsageRepository {
    snapshot: Mutex<Option<UsageSnapshot>>,
    unavailable: Mutex<bool>,
    write_outcome: Mutex<ConditionalWriteOutcome>,
    reserved: Mutex<Vec<UsageReservation>>,
    updated: Mutex<Vec<(UsageReservation, Option<u64>)>>,
    loaded_reservation: Mutex<Option<UsageReservation>>,
}

impl FakeUsageRepository {
    fn with_snapshot(snapshot: UsageSnapshot) -> Self {
        Self {
            snapshot: Mutex::new(Some(snapshot)),
            unavailable: Mutex::new(false),
            write_outcome: Mutex::new(ConditionalWriteOutcome::Committed),
            reserved: Mutex::new(Vec::new()),
            updated: Mutex::new(Vec::new()),
            loaded_reservation: Mutex::new(None),
        }
    }

    fn uninitialized() -> Self {
        Self {
            snapshot: Mutex::new(None),
            unavailable: Mutex::new(false),
            write_outcome: Mutex::new(ConditionalWriteOutcome::Committed),
            reserved: Mutex::new(Vec::new()),
            updated: Mutex::new(Vec::new()),
            loaded_reservation: Mutex::new(None),
        }
    }
}

impl UsageRepository for FakeUsageRepository {
    type Error = FakeError;

    async fn load(&self, _month: &InvoiceMonth) -> Result<Option<UsageSnapshot>, Self::Error> {
        if *self.unavailable.lock().unwrap() {
            return Err(FakeError);
        }
        Ok(self.snapshot.lock().unwrap().clone())
    }

    async fn initialize(
        &self,
        snapshot: &UsageSnapshot,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        let mut stored = self.snapshot.lock().unwrap();
        if stored.is_some() {
            Ok(ConditionalWriteOutcome::Conflict)
        } else {
            *stored = Some(snapshot.clone());
            Ok(ConditionalWriteOutcome::Committed)
        }
    }

    async fn load_reservation(
        &self,
        _month: &InvoiceMonth,
        _reservation_id: &StorageRecordId,
    ) -> Result<Option<UsageReservation>, Self::Error> {
        if *self.unavailable.lock().unwrap() {
            return Err(FakeError);
        }
        Ok(self.loaded_reservation.lock().unwrap().clone())
    }

    async fn reserve(
        &self,
        _expected_version: u64,
        reservation: &UsageReservation,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        if *self.unavailable.lock().unwrap() {
            return Err(FakeError);
        }
        let outcome = *self.write_outcome.lock().unwrap();
        if outcome == ConditionalWriteOutcome::Committed {
            *self.loaded_reservation.lock().unwrap() = Some(reservation.clone());
            self.reserved.lock().unwrap().push(reservation.clone());
        }
        Ok(outcome)
    }

    async fn update_reservation(
        &self,
        _expected_version: u64,
        reservation: &UsageReservation,
        trusted_measured_micro_inr: Option<u64>,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        if *self.unavailable.lock().unwrap() {
            return Err(FakeError);
        }
        let outcome = *self.write_outcome.lock().unwrap();
        if outcome == ConditionalWriteOutcome::Committed {
            *self.loaded_reservation.lock().unwrap() = Some(reservation.clone());
            if let Some(measured) = trusted_measured_micro_inr
                && let Some(snapshot) = self.snapshot.lock().unwrap().as_mut()
            {
                snapshot.reserved_micro_inr -= reservation.estimate_micro_inr;
                snapshot.settled_micro_inr += measured;
                snapshot.optimistic_version += 1;
            }
            self.updated
                .lock()
                .unwrap()
                .push((reservation.clone(), trusted_measured_micro_inr));
        }
        Ok(outcome)
    }
}

#[derive(Debug, Default)]
struct RecordingSink {
    metrics: Mutex<Vec<CostGuardMetric>>,
    reviews: Mutex<Vec<ManualReviewNotice>>,
    failures: Mutex<Vec<CostGuardFailureReason>>,
    progress: Mutex<Vec<StageProgress>>,
}

impl ObservabilitySink for RecordingSink {
    fn emit_cost_metric(&self, metric: &CostGuardMetric) {
        self.metrics.lock().unwrap().push(metric.clone());
    }

    fn emit_stage_progress(&self, progress: &StageProgress) {
        self.progress.lock().unwrap().push(progress.clone());
    }

    fn emit_manual_review(&self, notice: &ManualReviewNotice) {
        self.reviews.lock().unwrap().push(notice.clone());
    }

    fn emit_cost_guard_failure(
        &self,
        _environment: &EnvironmentLabel,
        _workflow_id: &WorkflowId,
        reason: CostGuardFailureReason,
    ) {
        self.failures.lock().unwrap().push(reason);
    }
}

fn month() -> InvoiceMonth {
    InvoiceMonth::new("2026-08").expect("valid month")
}

fn workflow_id() -> WorkflowId {
    WorkflowId::new("wf-cost-01").expect("valid workflow")
}

fn snapshot(settled: u64, reserved: u64, reconciled: u64) -> UsageSnapshot {
    UsageSnapshot {
        invoice_month: month(),
        settled_micro_inr: settled,
        reserved_micro_inr: reserved,
        reconciled_micro_inr: reconciled,
        reconciliation_observed_at: WorkflowTimestamp::from_unix_seconds(1_786_752_000),
        attribution_complete: true,
        pricing_version: StorageRecordId::new("pricing-v1").expect("pricing version"),
        pricing_approval_id: StorageRecordId::new("approval-v1").expect("approval id"),
        pricing_approved_at: WorkflowTimestamp::from_unix_seconds(1_786_752_000),
        optimistic_version: 7,
    }
}

fn request(operation_class: UsageOperationClass) -> ReserveRequest {
    ReserveRequest {
        workflow_id: workflow_id(),
        operation_class,
        invoice_month: month(),
        reservation_id: StorageRecordId::new("reservation-01").expect("valid reservation id"),
        pricing_version: StorageRecordId::new("pricing-v1").expect("valid pricing version"),
    }
}

fn thresholds() -> BudgetThresholds {
    BudgetThresholds::new(240_000_000, 270_000_000, 300_000_000, 10_000_000)
        .expect("valid thresholds")
}

fn service(sink: Arc<RecordingSink>) -> CostGuardService {
    CostGuardService::new(
        thresholds(),
        0,
        Arc::new(FixedEnvelope),
        EnvironmentLabel::new("development").expect("valid environment"),
        sink,
        Arc::new(FixedTime),
        evidence(),
    )
}

fn evidence() -> CostPolicyEvidence {
    CostPolicyEvidence {
        pricing_version: StorageRecordId::new("pricing-v1").expect("pricing version"),
        pricing_approval_id: StorageRecordId::new("approval-v1").expect("approval id"),
        pricing_approved_at: WorkflowTimestamp::from_unix_seconds(1_786_752_000),
        pricing_max_age_seconds: 2_678_400,
        attribution_complete: true,
        reconciliation_max_age_seconds: 86_400,
    }
}

fn reserved_usage() -> UsageReservation {
    UsageReservation {
        reservation_id: StorageRecordId::new("reservation-01").expect("valid reservation id"),
        workflow_id: workflow_id(),
        invoice_month: month(),
        operation_class: UsageOperationClass::NewWorkflow,
        estimate_micro_inr: 5_000_000,
        state: UsageReservationState::Reserved,
        pricing_version: StorageRecordId::new("pricing-v1").expect("valid pricing version"),
        created_at: WorkflowTimestamp::from_unix_seconds(1_786_752_000),
    }
}

#[tokio::test]
async fn cost_observability_permits_and_persists_below_warning() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(10_000_000, 0, 0));
    let sink = Arc::new(RecordingSink::default());
    let outcome = service(Arc::clone(&sink))
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await
        .expect("reservation succeeds");

    assert_eq!(
        outcome.decision.authorization(),
        BudgetAuthorization::Permit
    );
    assert!(outcome.reservation.is_some());
    assert_eq!(repository.reserved.lock().unwrap().len(), 1);
    assert_eq!(sink.metrics.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cost_observability_warns_at_80_percent_inclusive() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(235_000_000, 0, 0));
    let sink = Arc::new(RecordingSink::default());
    let outcome = service(sink)
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await
        .expect("decision succeeds");

    assert_eq!(outcome.decision.band(), BudgetBand::Warning);
    assert!(outcome.decision.warning_crossed());
    assert_eq!(
        outcome.decision.authorization(),
        BudgetAuthorization::Permit
    );
}

#[tokio::test]
async fn cost_observability_suspends_intake_at_90_percent_inclusive() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(265_000_000, 0, 0));
    let sink = Arc::new(RecordingSink::default());
    let outcome = service(sink)
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await
        .expect("denial is a valid decision");

    assert_eq!(outcome.decision.band(), BudgetBand::IntakeSuspended);
    assert_eq!(
        outcome.decision.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::IntakeSuspended)
    );
    assert!(outcome.reservation.is_none());
    assert!(repository.reserved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cost_observability_denies_projection_equal_to_hard_cap() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(295_000_000, 0, 0));
    let sink = Arc::new(RecordingSink::default());
    let outcome = service(sink)
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await
        .expect("denial is a valid decision");

    assert_eq!(outcome.decision.band(), BudgetBand::HardCap);
    assert_eq!(
        outcome.decision.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::HardCapReached)
    );
    assert!(outcome.reservation.is_none());
}

#[tokio::test]
async fn cost_observability_uses_higher_reconciled_projection() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(100_000_000, 0, 265_000_000));
    let sink = Arc::new(RecordingSink::default());
    let outcome = service(sink)
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await
        .expect("decision succeeds");

    assert_eq!(outcome.decision.band(), BudgetBand::IntakeSuspended);
}

#[tokio::test]
async fn cost_observability_keeps_bounded_status_available_during_suspension() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(270_000_000, 0, 0));
    let sink = Arc::new(RecordingSink::default());
    let outcome = service(sink)
        .reserve(&repository, &request(UsageOperationClass::Status))
        .await
        .expect("status decision succeeds");

    assert_eq!(outcome.decision.band(), BudgetBand::IntakeSuspended);
    assert_eq!(
        outcome.decision.authorization(),
        BudgetAuthorization::Permit
    );
}

#[tokio::test]
async fn cost_observability_denies_only_operations_that_cross_month_boundary() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(0, 0, 0));
    let sink = Arc::new(RecordingSink::default());
    let service = CostGuardService::new(
        thresholds(),
        0,
        Arc::new(FixedEnvelope),
        EnvironmentLabel::new("development").expect("environment"),
        sink,
        Arc::new(EndOfMonthTime),
        evidence(),
    );
    let result = service
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await;
    assert_eq!(result, Err(CostGuardError::MonthRolloverUnsupported));
}

#[tokio::test]
async fn cost_observability_initializes_an_approved_empty_month() {
    let repository = FakeUsageRepository::uninitialized();
    let sink = Arc::new(RecordingSink::default());
    let outcome = service(Arc::clone(&sink))
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await
        .expect("approved month initializes");

    assert_eq!(
        outcome.decision.authorization(),
        BudgetAuthorization::Permit
    );
    let initialized = repository
        .snapshot
        .lock()
        .unwrap()
        .clone()
        .expect("initialized snapshot");
    assert_eq!(initialized.invoice_month, month());
    assert_eq!(initialized.pricing_version, evidence().pricing_version);
    assert!(sink.failures.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cost_observability_fails_closed_when_reconciliation_is_stale() {
    let mut usage = snapshot(0, 0, 0);
    usage.reconciliation_observed_at = WorkflowTimestamp::from_unix_seconds(1_700_000_000);
    let repository = FakeUsageRepository::with_snapshot(usage);
    let sink = Arc::new(RecordingSink::default());
    let result = service(Arc::clone(&sink))
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await;

    assert_eq!(result, Err(CostGuardError::ReconciliationStale));
    assert_eq!(
        sink.failures.lock().unwrap().as_slice(),
        &[CostGuardFailureReason::InvalidProjection]
    );
}

#[tokio::test]
async fn cost_observability_fails_closed_when_pricing_approval_expires() {
    let approved_at = WorkflowTimestamp::from_unix_seconds(1_700_000_000);
    let mut usage = snapshot(0, 0, 0);
    usage.pricing_approved_at = approved_at;
    let repository = FakeUsageRepository::with_snapshot(usage);
    let mut policy_evidence = evidence();
    policy_evidence.pricing_approved_at = approved_at;
    let service = CostGuardService::new(
        thresholds(),
        0,
        Arc::new(FixedEnvelope),
        EnvironmentLabel::new("development").expect("environment"),
        Arc::new(RecordingSink::default()),
        Arc::new(FixedTime),
        policy_evidence,
    );

    let result = service
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await;
    assert_eq!(result, Err(CostGuardError::PricingUnapproved));
}

#[tokio::test]
async fn cost_observability_fails_closed_when_envelope_is_missing() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(0, 0, 0));
    let sink = Arc::new(RecordingSink::default());
    let service = CostGuardService::new(
        thresholds(),
        0,
        Arc::new(MissingEnvelope),
        EnvironmentLabel::new("staging").expect("valid environment"),
        Arc::clone(&sink) as Arc<dyn ObservabilitySink>,
        Arc::new(FixedTime),
        evidence(),
    );

    let result = service
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await;
    assert_eq!(result, Err(CostGuardError::InvalidEnvelope));
    assert_eq!(
        sink.failures.lock().unwrap().as_slice(),
        &[CostGuardFailureReason::InvalidEnvelope]
    );
}

#[tokio::test]
async fn cost_observability_replay_returns_the_matching_reservation() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(285_000_000, 5_000_000, 0));
    *repository.loaded_reservation.lock().unwrap() = Some(reserved_usage());
    *repository.write_outcome.lock().unwrap() = ConditionalWriteOutcome::Conflict;
    let sink = Arc::new(RecordingSink::default());
    let outcome = service(Arc::clone(&sink))
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await
        .expect("matching replay succeeds without another charge");

    assert_eq!(outcome.reservation, Some(reserved_usage()));
    assert!(repository.reserved.lock().unwrap().is_empty());
    assert_eq!(sink.metrics.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cost_observability_conditional_conflict_never_authorizes() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(0, 0, 0));
    *repository.write_outcome.lock().unwrap() = ConditionalWriteOutcome::Conflict;
    let mut conflicting = reserved_usage();
    conflicting.operation_class = UsageOperationClass::AiCall;
    *repository.loaded_reservation.lock().unwrap() = Some(conflicting);
    let sink = Arc::new(RecordingSink::default());
    let result = service(Arc::clone(&sink))
        .reserve(&repository, &request(UsageOperationClass::NewWorkflow))
        .await;

    assert_eq!(result, Err(CostGuardError::ConditionalConflict));
    assert!(repository.reserved.lock().unwrap().is_empty());
    assert_eq!(
        sink.failures.lock().unwrap().as_slice(),
        &[CostGuardFailureReason::ConditionalConflict]
    );
}

#[tokio::test]
async fn cost_observability_settlement_uses_server_side_reservation_amount() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(0, 5_000_000, 0));
    *repository.loaded_reservation.lock().unwrap() = Some(reserved_usage());
    let sink = Arc::new(RecordingSink::default());
    let outcome = service(sink)
        .settle(
            &repository,
            &SettleRequest {
                workflow_id: workflow_id(),
                invoice_month: month(),
                reservation_id: StorageRecordId::new("reservation-01")
                    .expect("valid reservation id"),
                mode: SettleMode::Settled,
            },
        )
        .await
        .expect("settlement succeeds");

    assert_eq!(outcome.reservation.state, UsageReservationState::Settled);
    {
        let updates = repository.updated.lock().unwrap();
        assert_eq!(updates[0].0.state, UsageReservationState::Settled);
        assert_eq!(updates[0].1, Some(5_000_000));
    }

    service(Arc::new(RecordingSink::default()))
        .settle(
            &repository,
            &SettleRequest {
                workflow_id: workflow_id(),
                invoice_month: month(),
                reservation_id: StorageRecordId::new("reservation-01")
                    .expect("valid reservation id"),
                mode: SettleMode::Settled,
            },
        )
        .await
        .expect("settlement replay is idempotent");
    assert_eq!(repository.updated.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cost_observability_manual_review_stays_reserved_and_emits_notice() {
    let repository = FakeUsageRepository::with_snapshot(snapshot(0, 5_000_000, 0));
    *repository.loaded_reservation.lock().unwrap() = Some(reserved_usage());
    let sink = Arc::new(RecordingSink::default());
    service(Arc::clone(&sink))
        .mark_manual_review(
            &repository,
            &ManualReviewRequest {
                workflow_id: workflow_id(),
                invoice_month: month(),
                reservation_id: StorageRecordId::new("reservation-01")
                    .expect("valid reservation id"),
            },
        )
        .await
        .expect("manual review succeeds");

    {
        let updates = repository.updated.lock().unwrap();
        assert_eq!(updates[0].0.state, UsageReservationState::ManualReview);
        assert_eq!(updates[0].1, None);
    }
    service(Arc::clone(&sink))
        .mark_manual_review(
            &repository,
            &ManualReviewRequest {
                workflow_id: workflow_id(),
                invoice_month: month(),
                reservation_id: StorageRecordId::new("reservation-01")
                    .expect("valid reservation id"),
            },
        )
        .await
        .expect("manual-review replay is idempotent");
    assert_eq!(repository.updated.lock().unwrap().len(), 1);
    assert_eq!(sink.reviews.lock().unwrap().len(), 2);
}

#[test]
fn cost_observability_json_is_redacted_and_workflow_is_not_a_metric_dimension() {
    let metric = CostGuardMetric {
        environment: EnvironmentLabel::new("production").expect("valid environment"),
        workflow_id: workflow_id(),
        operation_class: UsageOperationClass::AiCall,
        band: BudgetBand::Warning,
        outcome: application::observability::CostGuardOutcome::Permitted,
        warning_crossed: true,
        projected_micro_inr: 240_000_000,
    };
    let value = metric.to_json();
    let serialized = value.to_string();
    for forbidden in ["oauth", "token", "secret", "telegram", "document_content"] {
        assert!(!serialized.to_ascii_lowercase().contains(forbidden));
    }
    let dimensions = &value["_aws"]["CloudWatchMetrics"][0]["Dimensions"][0];
    assert!(
        !dimensions
            .as_array()
            .expect("dimensions array")
            .iter()
            .any(|item| item == "workflow_ref")
    );
    assert_eq!(value["workflow_id"], serde_json::Value::Null);
    assert_eq!(value["workflow_ref"].as_str().expect("reference").len(), 16);
    assert!(!serialized.contains("wf-cost-01"));
    assert_eq!(value["Environment"], "production");
}

#[test]
fn cost_observability_stage_progress_identifies_stage_and_outcome() {
    let progress = StageProgress {
        environment: EnvironmentLabel::new("development").expect("valid environment"),
        workflow_id: workflow_id(),
        stage: WorkflowStateKind::ExtractionCompleted,
        outcome: StageOutcome::Completed,
    };
    let value = progress.to_json();
    assert_eq!(value["stage"], "extraction_completed");
    assert_eq!(value["outcome"], "completed");
    assert_eq!(value["workflow_id"], serde_json::Value::Null);
    assert_eq!(value["workflow_ref"].as_str().expect("reference").len(), 16);
}
