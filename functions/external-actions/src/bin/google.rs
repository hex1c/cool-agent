use std::sync::Arc;

use application::external_operation::ProviderOutcome;
use external_actions_functions::runtime_auth::SsmOwnerTokenSource;
use external_actions_functions::{GoogleCreateEvent, GoogleCreateRunner, process_google_create};
use google::auth::OwnerTokenSource;
use google::reqwest_clients::ReqwestGoogleClient;
use google::sheets_docs::{
    ConfirmedCreateProof, CreateFileError, GoogleCreateService, NewFileRequest,
};
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde_json::Value;

struct CreateRunner {
    service: GoogleCreateService<ReqwestGoogleClient>,
    token_source: SsmOwnerTokenSource,
}

impl GoogleCreateRunner for CreateRunner {
    async fn run(
        &self,
        proof: &ConfirmedCreateProof,
        request: &NewFileRequest,
    ) -> Result<ProviderOutcome, CreateFileError> {
        let token = match self.token_source.access_token(proof.owner()).await {
            Ok(token) => token,
            Err(_) => {
                return Ok(ProviderOutcome::RetryableFailure(
                    application::external_operation::OperationFailure::new(
                        application::external_operation::FailureCode::new(
                            "google_owner_token_unavailable",
                        )
                        .map_err(|_| CreateFileError::Sanitization)?,
                        application::external_operation::SanitizedSummary::new(
                            "workflow owner google token is unavailable",
                        )
                        .map_err(|_| CreateFileError::Sanitization)?,
                    ),
                ));
            }
        };
        self.service.create_file(&token, proof, request).await
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let runner = Arc::new(CreateRunner {
        service: GoogleCreateService::new(ReqwestGoogleClient::new()?),
        token_source: SsmOwnerTokenSource::from_environment().await?,
    });
    run(service_fn(move |event| {
        let runner = Arc::clone(&runner);
        async move { handle(event, runner.as_ref()).await }
    }))
    .await
}

async fn handle(event: LambdaEvent<Value>, runner: &CreateRunner) -> Result<Value, Error> {
    let parsed: GoogleCreateEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(error_value("invalid_event")),
    };
    match process_google_create(parsed, runner).await {
        Ok(result) => serde_json::to_value(result).map_err(Into::into),
        Err(_) => Ok(error_value("google_action_failed")),
    }
}

fn error_value(code: &'static str) -> Value {
    serde_json::json!({ "error": code })
}
