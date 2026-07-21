use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde_json::Value;
use workflow_actions_functions::{PdfRenderError, PdfRenderEvent, pdf::process_pdf};

/// Lambda entry for the `pdf` handler.
///
/// Deserializes a versioned [`PdfRenderEvent`], renders the Version 1 PDF, and
/// returns the serialized [`workflow_actions_functions::PdfRenderResult`].
/// Pure compute — no AWS credentials or adapters required.
#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handle)).await
}

async fn handle(event: LambdaEvent<Value>) -> Result<Value, Error> {
    let parsed: PdfRenderEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(to_error(&PdfRenderError::InvalidEvent)),
    };
    match process_pdf(&parsed) {
        Ok(result) => Ok(serde_json::to_value(&result)
            .unwrap_or_else(|_| to_error(&PdfRenderError::SerializeFailed))),
        Err(err) => Ok(to_error(&err)),
    }
}

fn to_error(err: &PdfRenderError) -> Value {
    serde_json::json!({ "error": err.to_string() })
}
