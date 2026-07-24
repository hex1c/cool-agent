use std::sync::Arc;

use application::external_operation::ProviderOutcome;
use external_actions_functions::runtime_auth::SsmOwnerTokenSource;
use external_actions_functions::{
    CalendarActionEvent, CalendarActionRunner, process_calendar_action,
};
use google::calendar::{
    CalendarEventRequest, ConfirmedCalendarProof, CreateEventError, GoogleCalendarService,
};
use google::reqwest_clients::ReqwestGoogleClient;
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde_json::Value;

struct CalendarRunner {
    service: GoogleCalendarService<ReqwestGoogleClient>,
    token_source: SsmOwnerTokenSource,
}

impl CalendarActionRunner for CalendarRunner {
    async fn run(
        &self,
        proof: &ConfirmedCalendarProof,
        request: &CalendarEventRequest,
    ) -> Result<ProviderOutcome, CreateEventError> {
        self.service
            .create_event(&self.token_source, proof, request)
            .await
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let runner = Arc::new(CalendarRunner {
        service: GoogleCalendarService::new(ReqwestGoogleClient::new()?),
        token_source: SsmOwnerTokenSource::from_environment().await?,
    });
    run(service_fn(move |event| {
        let runner = Arc::clone(&runner);
        async move { handle(event, runner.as_ref()).await }
    }))
    .await
}

async fn handle(event: LambdaEvent<Value>, runner: &CalendarRunner) -> Result<Value, Error> {
    let parsed: CalendarActionEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(error_value("invalid_event")),
    };
    match process_calendar_action(parsed, runner).await {
        Ok(result) => serde_json::to_value(result).map_err(Into::into),
        Err(_) => Ok(error_value("calendar_action_failed")),
    }
}

fn error_value(code: &'static str) -> Value {
    serde_json::json!({ "error": code })
}
