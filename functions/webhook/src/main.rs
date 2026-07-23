#[cfg(test)]
use std::collections::HashMap;
use std::sync::Arc;

use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use telegram::webhook::WebhookVerifier;
#[cfg(test)]
use webhook_function::WebhookResponse;
use webhook_function::{ApiGatewayEvent, ApiGatewayResponse, process_webhook};

// ── Lambda entry point ─────────────────────────────────────────────────

async fn handler(
    event: LambdaEvent<ApiGatewayEvent>,
    verifier: &Arc<WebhookVerifier>,
) -> Result<ApiGatewayResponse, Error> {
    let response = process_webhook(&event.payload, verifier.as_ref());
    Ok(response.into_api_gateway())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let secret_token = std::env::var("TELEGRAM_SECRET_TOKEN")
        .map_err(|_| "TELEGRAM_SECRET_TOKEN environment variable is not set")?;
    let verifier = Arc::new(WebhookVerifier::new(secret_token));

    run(service_fn(move |event| {
        let verifier = Arc::clone(&verifier);
        async move { handler(event, &verifier).await }
    }))
    .await
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
