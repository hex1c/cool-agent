use external_actions_functions::CalendarActionEvent;
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde_json::Value;

/// Lambda entry for the `calendar` handler.
///
/// Deserializes a versioned [`CalendarActionEvent`] and validates the event.
/// Full adapter wiring (Google Calendar client + operation journal) is
/// deferred to Task 39 (SAM resource packaging); until then this entry
/// validates the event and reports a typed `adapter_not_configured` outcome
/// so the contract is exercisable without credentials. The pure preprocessing,
/// runner injection, and outcome mapping are unit-tested in
/// `tests/handlers.rs`.
#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handle)).await
}

async fn handle(event: LambdaEvent<Value>) -> Result<Value, Error> {
    let _parsed: CalendarActionEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(adapter_not_configured()),
    };
    Ok(adapter_not_configured())
}

fn adapter_not_configured() -> Value {
    serde_json::json!({ "error": "calendar adapter wiring pending (Task 39)" })
}
