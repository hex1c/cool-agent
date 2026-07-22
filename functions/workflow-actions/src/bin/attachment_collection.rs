use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde_json::Value;
use workflow_actions_functions::EVENT_SCHEMA_VERSION;

/// Lambda entry for the `attachment_collection` handler.
///
/// Deserializes a versioned attachment-collection event (emitted by the
/// quotation/calendar/email Step Functions after webhook intake) and validates
/// its schema version. Full adapter wiring (constructing the S3 object store
/// and DynamoDB metadata repository and driving the
/// `AttachmentCollectionService` from `crates/application`) is deferred to a
/// follow-up task; until then this entry validates the event and reports a
/// typed `adapter_not_configured` outcome so the Step Functions contract is
/// exercisable without AWS credentials. The pure preprocessing and outcome
/// mapping remain unit-testable through the application service.
///
/// # SAM handler requirements (Task 39)
///
/// | Handler                 | Runtime         | Memory (MB) | Timeout (s) | IAM needs |
/// |-------------------------|-----------------|------------:|------------:|-----------|
/// | `attachment_collection` | provided.al2023 | 512         | 60          | S3 `PutObject`/`HeadObject` on `raw/` prefix; DynamoDB `PutItem`/`UpdateItem` on object-metadata + workflow tables; `ssm:GetParameter` for presigning key |
#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handle)).await
}

async fn handle(event: LambdaEvent<Value>) -> Result<Value, Error> {
    Ok(classify(&event.payload))
}

/// Pure event classification, extracted for unit testing.
///
/// Returns a typed `adapter_not_configured` outcome when the schema version
/// matches, or a typed validation error otherwise. No adapter work is
/// performed until the S3/DynamoDB wiring is connected.
fn classify(payload: &Value) -> Value {
    let schema = payload.get("schemaVersion").and_then(|v| v.as_str());
    match schema {
        Some(v) if v == EVENT_SCHEMA_VERSION => adapter_not_configured(),
        _ => serde_json::json!({
            "error": "attachment_collection: invalid or missing schemaVersion"
        }),
    }
}

fn adapter_not_configured() -> Value {
    serde_json::json!({ "error": "attachment_collection adapter wiring pending" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_valid_schema_version_as_adapter_pending() {
        let payload = serde_json::json!({
            "schemaVersion": EVENT_SCHEMA_VERSION,
            "workflowId": "wf-1",
            "attachments": []
        });
        let result = classify(&payload);
        assert_eq!(
            result.get("error").and_then(|v| v.as_str()),
            Some("attachment_collection adapter wiring pending")
        );
    }

    #[test]
    fn rejects_missing_schema_version() {
        let payload = serde_json::json!({ "workflowId": "wf-1" });
        let result = classify(&payload);
        assert_eq!(
            result.get("error").and_then(|v| v.as_str()),
            Some("attachment_collection: invalid or missing schemaVersion")
        );
    }

    #[test]
    fn rejects_mismatched_schema_version() {
        let payload = serde_json::json!({ "schemaVersion": "novus.workflow-actions.v0" });
        let result = classify(&payload);
        assert_eq!(
            result.get("error").and_then(|v| v.as_str()),
            Some("attachment_collection: invalid or missing schemaVersion")
        );
    }
}
