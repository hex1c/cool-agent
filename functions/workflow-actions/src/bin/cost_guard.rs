use std::env;
use std::io;
use std::sync::Arc;

use aws_config::BehaviorVersion;
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde_json::Value;
use storage::dynamodb::DynamoDbStore;
use time::macros::format_description;
use workflow_actions_functions::cost_guard::{
    CostGuardEvent, CostGuardHandlerError, build_service, configured_operation_class,
    process_cost_guard,
};

struct Runtime {
    service: application::cost_guard::CostGuardService,
    repository: DynamoDbStore,
    pricing_version: application::ports::StorageRecordId,
    operation_class: Option<application::repositories::UsageOperationClass>,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let environment = required_env("ENVIRONMENT")?;
    let table_name = required_env("BUDGET_TABLE")?;
    let pricing_version = required_env("PRICING_VERSION")?;
    let pricing_approval = required_env("PRICING_APPROVAL_ID")?;
    let operation_class = configured_operation_class(env::var("OPERATION_CLASS").ok().as_deref())
        .map_err(|_| io::Error::other("invalid configured operation class"))?;
    let sdk_config = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let client = aws_sdk_dynamodb::Client::new(&sdk_config);
    let repository = DynamoDbStore::new(client, table_name, &environment, [0x41; 32])
        .map_err(|error| io::Error::other(error.to_string()))?;
    let service = build_service(&environment, &pricing_version, &pricing_approval)
        .map_err(|_| io::Error::other("invalid cost guard configuration"))?;
    let pricing_version = application::ports::StorageRecordId::new(pricing_version)
        .map_err(|_| io::Error::other("invalid pricing version"))?;
    let runtime = Arc::new(Runtime {
        service,
        repository,
        pricing_version,
        operation_class,
    });

    run(service_fn(move |event| {
        let runtime = Arc::clone(&runtime);
        async move { handle(runtime, event).await }
    }))
    .await
}

async fn handle(runtime: Arc<Runtime>, event: LambdaEvent<Value>) -> Result<Value, Error> {
    let parsed: CostGuardEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return serialize(CostGuardHandlerError::InvalidEvent.result()),
    };
    let current_month = current_invoice_month()?;
    let bookkeeping_action = matches!(
        &parsed,
        CostGuardEvent::Settle { .. } | CostGuardEvent::ManualReview { .. }
    );
    let result = process_cost_guard(
        &runtime.service,
        &runtime.repository,
        &current_month,
        &runtime.pricing_version,
        runtime.operation_class,
        &parsed,
    )
    .await;
    match result {
        Ok(result) => serialize(result),
        Err(_) if bookkeeping_action => {
            Err(io::Error::other("cost guard bookkeeping failed closed").into())
        }
        Err(error) => serialize(error.result()),
    }
}

fn serialize(result: impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(result)
        .map_err(|_| io::Error::other("cost guard response serialization failed").into())
}

fn required_env(name: &'static str) -> Result<String, io::Error> {
    env::var(name).map_err(|_| io::Error::other(format!("missing {name}")))
}

fn current_invoice_month() -> Result<application::repositories::InvoiceMonth, Error> {
    let value = time::OffsetDateTime::now_utc()
        .format(format_description!("[year]-[month]"))
        .map_err(|_| io::Error::other("invoice month formatting failed"))?;
    application::repositories::InvoiceMonth::new(value)
        .map_err(|_| io::Error::other("invoice month validation failed").into())
}
