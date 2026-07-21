use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde_json::Value;
use workflow_actions_functions::DeliveryEvent;

/// Lambda entry for the `delivery` handler.
///
/// Deserializes a versioned [`DeliveryEvent`] and rebuilds the validated
/// `DeliveryRequest`. Full adapter wiring (S3/Drive/Telegram clients +
/// operation journal) is deferred to Task 39 (SAM resource packaging); until
/// then this entry validates the event and reports a typed
/// `adapter_not_configured` outcome so the contract is exercisable without AWS
/// credentials. The pure preprocessing, runner injection, and outcome mapping
/// are unit-tested in `tests/handlers.rs` via `process_delivery`.
#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handle)).await
}

async fn handle(event: LambdaEvent<Value>) -> Result<Value, Error> {
    let parsed: DeliveryEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(adapter_not_configured()),
    };
    match parsed.build() {
        Ok(_) => Ok(adapter_not_configured()),
        Err(err) => Ok(serde_json::json!({ "error": err.to_string() })),
    }
}

fn adapter_not_configured() -> Value {
    serde_json::json!({ "error": "delivery adapter wiring pending (Task 39)" })
}
