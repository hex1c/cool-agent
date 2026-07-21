use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde_json::Value;
use workflow_actions_functions::{
    WorkflowActionError, WorkflowActionEvent, workflow::process_workflow_action,
};

/// Lambda entry for the `workflow` action handler.
///
/// Deserializes a versioned [`WorkflowActionEvent`], consumes the pending
/// `StartDirectPdfGeneration` confirmation, and returns the revision-bound
/// proof plus resulting workflow. Pure domain transition — the caller persists
/// the consume via the confirmation repository.
#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handle)).await
}

async fn handle(event: LambdaEvent<Value>) -> Result<Value, Error> {
    let parsed: WorkflowActionEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(to_error(&WorkflowActionError::InvalidEvent)),
    };
    match process_workflow_action(&parsed) {
        Ok(result) => Ok(serde_json::to_value(&result)
            .unwrap_or_else(|_| to_error(&WorkflowActionError::InvalidEvent))),
        Err(err) => Ok(to_error(&err)),
    }
}

fn to_error(err: &WorkflowActionError) -> Value {
    serde_json::json!({ "error": err.to_string() })
}
