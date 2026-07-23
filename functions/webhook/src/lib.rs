#![deny(unsafe_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use telegram::WebhookError;
use telegram::normalize::NormalizeError;
pub use telegram::webhook::WebhookVerifier;

/// API Gateway request fields consumed by the Telegram webhook handler.
#[derive(Debug, Deserialize)]
pub struct ApiGatewayEvent {
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
}

/// Bounded API Gateway response emitted by the Telegram webhook handler.
#[derive(Debug, Serialize)]
pub struct ApiGatewayResponse {
    #[serde(rename = "statusCode")]
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
}

/// Bounded, typed HTTP response for the webhook handler.
///
/// Every variant maps to a single HTTP status code and a stable body label.
/// Raw provider/Telegram strings are never echoed verbatim.
#[derive(Debug)]
pub enum WebhookResponse {
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
pub struct AcceptedUpdate {
    pub update_id: i64,
    pub route: &'static str,
    pub event_kind: &'static str,
}

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

impl WebhookResponse {
    /// Convert the bounded response to the API Gateway wire format.
    pub fn into_api_gateway(self) -> ApiGatewayResponse {
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

/// Authenticate and normalize one API Gateway Telegram webhook event.
pub fn process_webhook(event: &ApiGatewayEvent, verifier: &WebhookVerifier) -> WebhookResponse {
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
        Some(body) if !body.is_empty() => body.as_bytes(),
        _ => {
            return WebhookResponse::Malformed {
                reason: "empty_body",
            };
        }
    };

    let update = match telegram::normalize::normalize(body_bytes) {
        Ok(update) => update,
        Err(error) => match error {
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

    WebhookResponse::Accepted(AcceptedUpdate {
        update_id: update.update_id,
        route: route_label(&update),
        event_kind: event_kind_label(&update),
    })
}
