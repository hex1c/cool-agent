#![allow(clippy::expect_used, clippy::panic)]

use std::fmt::{self, Display, Formatter};
use std::sync::Mutex;

use application::ports::StorageRecordId;
use application::repositories::{
    ConditionalWriteOutcome, InvoiceMonth, UsageRepository, UsageReservation, UsageSnapshot,
};
use workflow_actions_functions::EVENT_SCHEMA_VERSION;
use workflow_actions_functions::cost_guard::{CostGuardEvent, build_service, process_cost_guard};

#[derive(Debug, Clone, Copy)]
struct FakeError;

impl Display for FakeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("fake repository error")
    }
}

struct FakeRepository {
    snapshot: UsageSnapshot,
    reserved_month: Mutex<Option<InvoiceMonth>>,
    reservation: Mutex<Option<UsageReservation>>,
}

impl UsageRepository for FakeRepository {
    type Error = FakeError;

    async fn load(&self, _month: &InvoiceMonth) -> Result<Option<UsageSnapshot>, Self::Error> {
        Ok(Some(self.snapshot.clone()))
    }

    async fn initialize(
        &self,
        _snapshot: &UsageSnapshot,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        Ok(ConditionalWriteOutcome::Conflict)
    }

    async fn load_reservation(
        &self,
        _month: &InvoiceMonth,
        _reservation_id: &StorageRecordId,
    ) -> Result<Option<UsageReservation>, Self::Error> {
        Ok(self.reservation.lock().expect("lock").clone())
    }

    async fn reserve(
        &self,
        _expected_version: u64,
        reservation: &UsageReservation,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        *self.reserved_month.lock().expect("lock") = Some(reservation.invoice_month.clone());
        *self.reservation.lock().expect("lock") = Some(reservation.clone());
        Ok(ConditionalWriteOutcome::Committed)
    }

    async fn update_reservation(
        &self,
        _expected_version: u64,
        _reservation: &UsageReservation,
        _trusted_measured_micro_inr: Option<u64>,
    ) -> Result<ConditionalWriteOutcome, Self::Error> {
        Ok(ConditionalWriteOutcome::Committed)
    }
}

fn month() -> InvoiceMonth {
    let now = time::OffsetDateTime::now_utc();
    InvoiceMonth::new(format!("{:04}-{:02}", now.year(), now.month() as u8)).expect("valid month")
}

fn now_timestamp() -> domain::workflow::WorkflowTimestamp {
    let seconds = u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp())
        .expect("non-negative timestamp");
    domain::workflow::WorkflowTimestamp::from_unix_seconds(seconds)
}

#[tokio::test]
async fn cost_guard_handler_uses_server_supplied_invoice_month() {
    let repository = FakeRepository {
        snapshot: UsageSnapshot {
            invoice_month: month(),
            settled_micro_inr: 0,
            reserved_micro_inr: 0,
            reconciled_micro_inr: 0,
            reconciliation_observed_at: now_timestamp(),
            attribution_complete: true,
            pricing_version: StorageRecordId::new("task41-2026-07-23").expect("pricing"),
            pricing_approval_id: StorageRecordId::new("task41-2026-07-23").expect("approval"),
            pricing_approved_at: domain::workflow::WorkflowTimestamp::from_unix_seconds(
                1_784_764_800,
            ),
            optimistic_version: 1,
        },
        reserved_month: Mutex::new(None),
        reservation: Mutex::new(None),
    };
    let service =
        build_service("development", "task41-2026-07-23", "task41-2026-07-23").expect("service");
    let event: CostGuardEvent = serde_json::from_value(serde_json::json!({
        "action": "reserve",
        "schemaVersion": EVENT_SCHEMA_VERSION,
        "workflowId": "wf-handler-01",
        "reservationId": "reserve-handler-01"
    }))
    .expect("valid event");

    let result = process_cost_guard(
        &service,
        &repository,
        &month(),
        &StorageRecordId::new("task41-2026-07-23").expect("pricing"),
        Some(application::repositories::UsageOperationClass::NewWorkflow),
        &event,
    )
    .await
    .expect("handler result");
    assert!(result.authorized);
    assert_eq!(
        repository
            .reserved_month
            .lock()
            .expect("lock")
            .as_ref()
            .expect("reservation month")
            .as_str(),
        month().as_str()
    );
}

#[test]
fn cost_guard_handler_rejects_caller_supplied_monetary_values() {
    let parsed = serde_json::from_value::<CostGuardEvent>(serde_json::json!({
        "action": "reserve",
        "schemaVersion": EVENT_SCHEMA_VERSION,
        "workflowId": "wf-handler-01",
        "reservationId": "reserve-handler-01",
        "estimateMicroInr": 1
    }));
    assert!(parsed.is_err());
}
