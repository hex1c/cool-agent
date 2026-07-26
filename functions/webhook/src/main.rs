#[cfg(test)]
use std::collections::HashMap;
use std::sync::Arc;

use aws_sdk_sfn::Client as StepFunctionsClient;
use domain::WorkflowTimestamp;
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use storage::dynamodb::DynamoDbStore;
use telegram::webhook::WebhookVerifier;
#[cfg(test)]
use webhook_function::WebhookResponse;
use webhook_function::dispatch::{
    DispatchOutcome, WorkflowDispatchService, WorkflowKind, WorkflowStartRequest, WorkflowStarter,
};
use webhook_function::{ApiGatewayEvent, ApiGatewayResponse, process_webhook};

struct AwsWorkflowStarter {
    client: StepFunctionsClient,
    quotation_arn: String,
    calendar_arn: String,
    email_arn: String,
}

impl WorkflowStarter for AwsWorkflowStarter {
    type Error = String;

    async fn start(
        &self,
        kind: WorkflowKind,
        request: &WorkflowStartRequest,
    ) -> Result<(), Self::Error> {
        let state_machine_arn = match kind {
            WorkflowKind::Quotation => &self.quotation_arn,
            WorkflowKind::Calendar => &self.calendar_arn,
            WorkflowKind::Email => &self.email_arn,
        };
        let input = serde_json::to_string(request)
            .map_err(|_| "workflow start request serialization failed".to_owned())?;
        self.client
            .start_execution()
            .state_machine_arn(state_machine_arn)
            .name(&request.workflow_id)
            .input(input)
            .send()
            .await
            .map_err(|_| "Step Functions start_execution failed".to_owned())?;
        Ok(())
    }
}

type AwsDispatcher = WorkflowDispatchService<DynamoDbStore, AwsWorkflowStarter>;

async fn handler(
    event: LambdaEvent<ApiGatewayEvent>,
    verifier: &Arc<WebhookVerifier>,
    dispatcher: &AwsDispatcher,
) -> Result<ApiGatewayResponse, Error> {
    let response = process_webhook(&event.payload, verifier.as_ref());
    if let webhook_function::WebhookResponse::Accepted(accepted) = &response {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        match dispatcher
            .dispatch(
                &accepted.normalized,
                WorkflowTimestamp::from_unix_seconds(timestamp),
            )
            .await
        {
            Ok(DispatchOutcome::Started | DispatchOutcome::Existing | DispatchOutcome::Ignored) => {
            }
            Err(error) => {
                eprintln!("webhook dispatch failed: {error}");
                let response = ApiGatewayResponse {
                    status_code: 503,
                    headers: [("Content-Type".to_owned(), "application/json".to_owned())]
                        .into_iter()
                        .collect(),
                    body: r#"{"error":"dispatch_unavailable"}"#.to_owned(),
                };
                emit_edge_metric("Webhook", response.status_code);
                return Ok(response);
            }
        }
    }
    let response = response.into_api_gateway();
    emit_edge_metric("Webhook", response.status_code);
    Ok(response)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let secret_token = std::env::var("TELEGRAM_SECRET_TOKEN")
        .map_err(|_| "TELEGRAM_SECRET_TOKEN environment variable is not set")?;
    let verifier = Arc::new(WebhookVerifier::new(secret_token));
    let dispatcher = Arc::new(build_dispatcher().await?);

    run(service_fn(move |event| {
        let verifier = Arc::clone(&verifier);
        let dispatcher = Arc::clone(&dispatcher);
        async move { handler(event, &verifier, &dispatcher).await }
    }))
    .await
}

async fn build_dispatcher() -> Result<AwsDispatcher, Error> {
    let environment = required_env("ENVIRONMENT")?;
    let application_table = required_env("APPLICATION_TABLE")?;
    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let page_token_key = load_page_token_key(&sdk_config).await?;

    let mut dynamodb_config = aws_sdk_dynamodb::config::Builder::from(&sdk_config);
    if let Ok(endpoint) = std::env::var("DYNAMODB_ENDPOINT") {
        dynamodb_config = dynamodb_config.endpoint_url(endpoint);
    }
    let repository = DynamoDbStore::new(
        aws_sdk_dynamodb::Client::from_conf(dynamodb_config.build()),
        application_table,
        environment,
        page_token_key,
    )?;

    let mut sfn_config = aws_sdk_sfn::config::Builder::from(&sdk_config);
    if let Ok(endpoint) = std::env::var("STEPFUNCTIONS_ENDPOINT") {
        sfn_config = sfn_config.endpoint_url(endpoint);
    }
    let sfn_client = StepFunctionsClient::from_conf(sfn_config.build());
    let starter = AwsWorkflowStarter {
        client: sfn_client.clone(),
        quotation_arn: required_env("QUOTATION_STATE_MACHINE_ARN")?,
        calendar_arn: required_env("CALENDAR_STATE_MACHINE_ARN")?,
        email_arn: required_env("EMAIL_STATE_MACHINE_ARN")?,
    };
    Ok(WorkflowDispatchService::new(repository.clone(), starter)
        .with_task_token_resume(sfn_client, repository))
}

fn required_env(name: &str) -> Result<String, Error> {
    std::env::var(name).map_err(|_| format!("{name} environment variable is not set").into())
}

async fn load_page_token_key(sdk_config: &aws_config::SdkConfig) -> Result<[u8; 32], Error> {
    if let Ok(value) = std::env::var("PAGE_TOKEN_SIGNING_KEY_HEX") {
        return decode_page_token_key(&value);
    }
    let parameter_name = required_env("PAGE_TOKEN_SIGNING_KEY_PARAMETER")?;
    let output = aws_sdk_ssm::Client::new(sdk_config)
        .get_parameter()
        .name(parameter_name)
        .with_decryption(true)
        .send()
        .await
        .map_err(|_| "page-token signing key parameter could not be loaded")?;
    let value = output
        .parameter()
        .and_then(|parameter| parameter.value())
        .ok_or("page-token signing key parameter is empty")?;
    decode_page_token_key(value)
}

fn decode_page_token_key(value: &str) -> Result<[u8; 32], Error> {
    let decoded = hex::decode(value).map_err(|_| "page-token signing key must be hex")?;
    decoded
        .try_into()
        .map_err(|_| "page-token signing key must encode exactly 32 bytes".into())
}

fn emit_edge_metric(kind: &'static str, status_code: u16) {
    let environment = std::env::var("ENVIRONMENT").unwrap_or_else(|_| "unknown".to_owned());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let error_count = u8::from(status_code >= 400);
    let record = serde_json::json!({
        "_aws": {
            "Timestamp": timestamp,
            "CloudWatchMetrics": [{
                "Namespace": "Novus/Edge",
                "Dimensions": [["Environment"]],
                "Metrics": [
                    {"Name": format!("{kind}RequestCount"), "Unit": "Count"},
                    {"Name": format!("{kind}ErrorCount"), "Unit": "Count"}
                ]
            }]
        },
        "Environment": environment,
        (format!("{kind}RequestCount")): 1,
        (format!("{kind}ErrorCount")): error_count,
        "event": "edge_request_outcome",
        "handler": kind.to_ascii_lowercase(),
        "outcome": if error_count == 0 { "accepted" } else { "rejected" }
    });
    if let Ok(line) = serde_json::to_string(&record) {
        println!("{line}");
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn make_event(headers: HashMap<String, String>, body: Option<String>) -> ApiGatewayEvent {
        ApiGatewayEvent { headers, body }
    }

    fn make_verifier() -> WebhookVerifier {
        WebhookVerifier::new("test-secret".into())
    }

    fn valid_body() -> String {
        r#"{
            "update_id": 1001,
            "message": {
                "message_id": 5,
                "from": {"id": 111, "is_bot": false, "first_name": "Test"},
                "chat": {"id": -1001234567890, "type": "supergroup"},
                "message_thread_id": 10,
                "is_topic_message": true,
                "text": "@bot hello",
                "entities": [{"type": "mention", "offset": 0, "length": 4}]
            }
        }"#
        .into()
    }

    // ── Success path ───────────────────────────────────────────────────

    #[test]
    fn valid_webhook_returns_accepted() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "test-secret".into(),
        );
        let event = make_event(headers, Some(valid_body()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        assert!(matches!(response, WebhookResponse::Accepted(_)));
    }

    #[test]
    fn accepted_includes_update_id_and_labels() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "test-secret".into(),
        );
        let event = make_event(headers, Some(valid_body()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        if let WebhookResponse::Accepted(update) = response {
            assert_eq!(update.update_id, 1001);
            assert_eq!(update.route, "forum_topic");
            assert_eq!(update.event_kind, "mention");
        } else {
            unreachable!();
        }
    }

    #[test]
    fn accepted_api_gateway_response_has_200() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "test-secret".into(),
        );
        let event = make_event(headers, Some(valid_body()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        let agw = response.into_api_gateway();
        assert_eq!(agw.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(&agw.body).expect("valid json body");
        assert_eq!(body.get("update_id").and_then(|v| v.as_i64()), Some(1001));
        assert_eq!(
            body.get("route").and_then(|v| v.as_str()),
            Some("forum_topic")
        );
        assert_eq!(
            body.get("event_kind").and_then(|v| v.as_str()),
            Some("mention")
        );
    }

    // ── Invalid token ──────────────────────────────────────────────────

    #[test]
    fn missing_token_returns_invalid_token() {
        let event = make_event(HashMap::new(), Some(valid_body()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        assert!(matches!(response, WebhookResponse::InvalidToken));
    }

    #[test]
    fn wrong_token_returns_invalid_token() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "wrong-token".into(),
        );
        let event = make_event(headers, Some(valid_body()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        assert!(matches!(response, WebhookResponse::InvalidToken));
    }

    #[test]
    fn invalid_token_api_gateway_response_has_401() {
        let event = make_event(HashMap::new(), Some(valid_body()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        let agw = response.into_api_gateway();
        assert_eq!(agw.status_code, 401);
        let body: serde_json::Value = serde_json::from_str(&agw.body).expect("valid json body");
        assert_eq!(
            body.get("error").and_then(|v| v.as_str()),
            Some("invalid_token")
        );
    }

    // ── Malformed ──────────────────────────────────────────────────────

    #[test]
    fn empty_body_returns_malformed() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "test-secret".into(),
        );
        let event = make_event(headers, None);
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        assert!(matches!(
            response,
            WebhookResponse::Malformed {
                reason: "empty_body"
            }
        ));
    }

    #[test]
    fn invalid_json_returns_malformed() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "test-secret".into(),
        );
        let event = make_event(headers, Some("not json".into()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        assert!(matches!(
            response,
            WebhookResponse::Malformed {
                reason: "parse_error"
            }
        ));
    }

    #[test]
    fn malformed_api_gateway_response_has_400() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "test-secret".into(),
        );
        let event = make_event(headers, None);
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        let agw = response.into_api_gateway();
        assert_eq!(agw.status_code, 400);
    }

    // ── Unsupported chat ───────────────────────────────────────────────

    #[test]
    fn unsupported_chat_type_returns_422() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "test-secret".into(),
        );
        let body = r#"{
            "update_id": 2001,
            "message": {
                "message_id": 1,
                "from": {"id": 111, "is_bot": false, "first_name": "Test"},
                "chat": {"id": -200, "type": "group"},
                "text": "hello"
            }
        }"#;
        let event = make_event(headers, Some(body.into()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        assert!(matches!(response, WebhookResponse::UnsupportedChat));
    }

    #[test]
    fn unsupported_chat_api_gateway_response_has_422() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "test-secret".into(),
        );
        let body = r#"{
            "update_id": 2001,
            "message": {
                "message_id": 1,
                "from": {"id": 111, "is_bot": false, "first_name": "Test"},
                "chat": {"id": -200, "type": "group"},
                "text": "hello"
            }
        }"#;
        let event = make_event(headers, Some(body.into()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        let agw = response.into_api_gateway();
        assert_eq!(agw.status_code, 422);
    }

    // ── Header case-insensitivity ──────────────────────────────────────

    #[test]
    fn header_key_is_case_sensitive_lookup_uses_lowercase() {
        // API Gateway v2 lowercases all header keys.
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".into(),
            "test-secret".into(),
        );
        let event = make_event(headers, Some(valid_body()));
        let verifier = make_verifier();

        let response = process_webhook(&event, &verifier);
        assert!(matches!(response, WebhookResponse::Accepted(_)));
    }
}
