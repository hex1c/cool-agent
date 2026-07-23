#![cfg(feature = "integration")]
#![allow(clippy::expect_used)]

use std::error::Error;
use std::sync::Arc;

use application::cost_guard::{
    CostGuardService, CostPolicyEvidence, OperationEnvelopeProvider, ReserveRequest, TimeProvider,
};
use application::observability::{EnvironmentLabel, NoOpSink};
use application::ports::StorageRecordId;
use application::repositories::{InvoiceMonth, UsageOperationClass, UsageRepository};
use aws_sdk_dynamodb::config::{Credentials, Region};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, KeySchemaElement, KeyType, ScalarAttributeType,
};
use domain::identity::WorkflowId;
use domain::workflow::WorkflowTimestamp;
use domain::{BudgetAuthorization, BudgetDenialReason, BudgetThresholds};
use storage::dynamodb::DynamoDbStore;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
struct ThirtyFiveRupeeEnvelope;

impl OperationEnvelopeProvider for ThirtyFiveRupeeEnvelope {
    fn estimate(&self, _operation_class: UsageOperationClass) -> Option<u64> {
        Some(35_000_000)
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

fn client() -> aws_sdk_dynamodb::Client {
    let endpoint =
        std::env::var("DYNAMODB_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:8000".to_owned());
    let config = aws_sdk_dynamodb::Config::builder()
        .behavior_version_latest()
        .endpoint_url(endpoint)
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new("test", "test", None, None, "integration"))
        .build();
    aws_sdk_dynamodb::Client::from_conf(config)
}

fn request(id: &str, workflow: &str, month: &InvoiceMonth) -> ReserveRequest {
    ReserveRequest {
        workflow_id: WorkflowId::new(workflow).expect("workflow id"),
        operation_class: UsageOperationClass::NewWorkflow,
        invoice_month: month.clone(),
        reservation_id: StorageRecordId::new(id).expect("reservation id"),
        pricing_version: StorageRecordId::new("task41-2026-07-23").expect("pricing id"),
    }
}

#[tokio::test]
async fn concurrent_budget_reservations_never_cross_hard_cap() -> Result<(), Box<dyn Error>> {
    let client = client();
    let table = format!("novus-development-budget-test-{}", Uuid::new_v4().simple());
    client
        .create_table()
        .table_name(&table)
        .billing_mode(aws_sdk_dynamodb::types::BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("pk")
                .attribute_type(ScalarAttributeType::S)
                .build()?,
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("sk")
                .attribute_type(ScalarAttributeType::S)
                .build()?,
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("pk")
                .key_type(KeyType::Hash)
                .build()?,
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("sk")
                .key_type(KeyType::Range)
                .build()?,
        )
        .send()
        .await?;

    let month = InvoiceMonth::new("2026-08")?;
    client
        .put_item()
        .table_name(&table)
        .item("pk", AttributeValue::S("MONTH#2026-08".to_owned()))
        .item("sk", AttributeValue::S("AGGREGATE".to_owned()))
        .item("entity", AttributeValue::S("budget_aggregate".to_owned()))
        .item("invoice_month", AttributeValue::S("2026-08".to_owned()))
        .item(
            "settled_micro_inr",
            AttributeValue::N("230000000".to_owned()),
        )
        .item("reserved_micro_inr", AttributeValue::N("0".to_owned()))
        .item("reconciled_micro_inr", AttributeValue::N("0".to_owned()))
        .item(
            "reconciliation_observed_at",
            AttributeValue::N("1786752000".to_owned()),
        )
        .item("attribution_complete", AttributeValue::Bool(true))
        .item(
            "pricing_version",
            AttributeValue::S("task41-2026-07-23".to_owned()),
        )
        .item(
            "pricing_approval_id",
            AttributeValue::S("task41-2026-07-23".to_owned()),
        )
        .item(
            "pricing_approved_at",
            AttributeValue::N("1784764800".to_owned()),
        )
        .item("optimistic_version", AttributeValue::N("0".to_owned()))
        .send()
        .await?;

    let repository = DynamoDbStore::new(client.clone(), &table, "development", [0x41; 32])?;
    let service = CostGuardService::new(
        BudgetThresholds::new(240_000_000, 270_000_000, 300_000_000, 0)?,
        0,
        Arc::new(ThirtyFiveRupeeEnvelope),
        EnvironmentLabel::new("development")?,
        Arc::new(NoOpSink),
        Arc::new(FixedTime),
        CostPolicyEvidence {
            pricing_version: StorageRecordId::new("task41-2026-07-23")?,
            pricing_approval_id: StorageRecordId::new("task41-2026-07-23")?,
            pricing_approved_at: WorkflowTimestamp::from_unix_seconds(1_784_764_800),
            pricing_max_age_seconds: 2_678_400,
            attribution_complete: true,
            reconciliation_max_age_seconds: 86_400,
        },
    );
    let first_request = request("reservation-a", "workflow-a", &month);
    let second_request = request("reservation-b", "workflow-b", &month);
    let (first, second) = tokio::join!(
        service.reserve(&repository, &first_request),
        service.reserve(&repository, &second_request)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let accepted_request = if first.is_ok() {
        &first_request
    } else {
        &second_request
    };
    let replay = service.reserve(&repository, accepted_request).await?;
    assert_eq!(replay.decision.authorization(), BudgetAuthorization::Permit);

    let snapshot = repository.load(&month).await?.expect("aggregate");
    assert_eq!(
        snapshot.settled_micro_inr + snapshot.reserved_micro_inr,
        265_000_000
    );

    let retry = service
        .reserve(&repository, &request("reservation-c", "workflow-c", &month))
        .await?;
    assert_eq!(
        retry.decision.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::HardCapReached)
    );

    client.delete_table().table_name(&table).send().await?;
    Ok(())
}
