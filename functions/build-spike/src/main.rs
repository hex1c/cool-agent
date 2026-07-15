use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct PingRequest {
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct PingResponse {
    ok: bool,
    message: String,
}

async fn handler(event: LambdaEvent<PingRequest>) -> Result<PingResponse, Error> {
    let message = event
        .payload
        .message
        .unwrap_or_else(|| String::from("pong"));

    Ok(PingResponse { ok: true, message })
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}
