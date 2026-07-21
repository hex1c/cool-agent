use std::collections::HashMap;
use std::sync::Arc;

use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde::{Deserialize, Serialize};
use telegram::WebhookError;
use telegram::normalize::NormalizeError;
use telegram::webhook::WebhookVerifier;

// ── API Gateway event ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ApiGatewayEvent {
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

// ── API Gateway response ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ApiGatewayResponse {
    #[serde(rename = "statusCode")]
    status_code: u16,
    headers: HashMap<String, String>,
    body: String,
}

// ── Typed webhook response ─────────────────────────────────────────────

/// Bounded, typed HTTP response for the webhook handler.
///
/// Every variant maps to a single HTTP status code and a stable body label.
/// Raw provider/Telegram strings are never echoed verbatim.
#[derive(Debug)]
enum WebhookResponse {
    /// Update accepted for downstream processing.
    Accepted(AcceptedUpdate),
    /// Secret token missing or mismatched.
    InvalidToken,
    /// Body missing, unparseable, or missing sender.
    Malformed { reason: &'static str },
    /// Chat type not supported (group, channel, non-forum supergroup).
    UnsupportedChat,
}

/// Typed handoff emitted when a webhook update is accepted.
///
/// Downstream workflow processing consumes this handoff; the webhook handler
/// itself returns 200 immediately without waiting for long-running work.
#[derive(Debug, Serialize)]
struct AcceptedUpdate {
    update_id: i64,
    route: &'static str,
    event_kind: &'static str,
}

// ── Label helpers ──────────────────────────────────────────────────────

fn route_label(route: &telegram::normalize::NormalizedUpdate) -> &'static str {
    use domain::routing::Route;
    match &route.route {
        Route::ForumTopic { .. } => "forum_topic",
        Route::OAuthPrivate { .. } => "oauth_private",
        Route::Unsupported => "unsupported",
    }
}

fn event_kind_label(route: &telegram::normalize::NormalizedUpdate) -> &'static str {
    use telegram::normalize::EventKind;
    match &route.event {
        EventKind::Mention { .. } => "mention",
        EventKind::Reply { .. } => "reply",
        EventKind::Callback { .. } => "callback",
        EventKind::Media { .. } => "media",
        EventKind::Command { .. } => "command",
    }
}

// ── Conversion to API Gateway format ───────────────────────────────────

impl WebhookResponse {
    fn into_api_gateway(self) -> ApiGatewayResponse {
        let mut headers = HashMap::new();
        headers.insert("Content-Type".into(), "application/json".into());

        match self {
            Self::Accepted(update) => {
                let body = serde_json::to_string(&update).unwrap_or_else(|_| {
                    r#"{"update_id":0,"route":"error","event_kind":"error"}"#.into()
                });
                ApiGatewayResponse {
                    status_code: 200,
                    headers,
                    body,
                }
            }
            Self::InvalidToken => ApiGatewayResponse {
                status_code: 401,
                headers,
                body: r#"{"error":"invalid_token"}"#.into(),
            },
            Self::Malformed { reason } => {
                let body = serde_json::json!({ "error": reason }).to_string();
                ApiGatewayResponse {
                    status_code: 400,
                    headers,
                    body,
                }
            }
            Self::UnsupportedChat => ApiGatewayResponse {
                status_code: 422,
                headers,
                body: r#"{"error":"unsupported_chat"}"#.into(),
            },
        }
    }
}

// ── Handler logic ──────────────────────────────────────────────────────

fn process_webhook(event: &ApiGatewayEvent, verifier: &WebhookVerifier) -> WebhookResponse {
    let header_value = event
        .headers
        .get("x-telegram-bot-api-secret-token")
        .map(String::as_str);

    match verifier.verify(header_value) {
        Ok(()) => {}
        Err(WebhookError::MissingToken | WebhookError::InvalidToken) => {
            return WebhookResponse::InvalidToken;
        }
    }

    let body_bytes = match event.body.as_deref() {
        Some(b) if !b.is_empty() => b.as_bytes(),
        _ => {
            return WebhookResponse::Malformed {
                reason: "empty_body",
            };
        }
    };

    let update = match telegram::normalize::normalize(body_bytes) {
        Ok(u) => u,
        Err(e) => match e {
            NormalizeError::ParseError(_) => {
                return WebhookResponse::Malformed {
                    reason: "parse_error",
                };
            }
            NormalizeError::MissingSender | NormalizeError::EmptyUpdate => {
                return WebhookResponse::Malformed {
                    reason: "missing_sender",
                };
            }
            NormalizeError::UnsupportedChat { .. } | NormalizeError::MissingThreadId { .. } => {
                return WebhookResponse::UnsupportedChat;
            }
        },
    };

    let route = route_label(&update);
    let event_kind = event_kind_label(&update);

    WebhookResponse::Accepted(AcceptedUpdate {
        update_id: update.update_id,
        route,
        event_kind,
    })
}

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
