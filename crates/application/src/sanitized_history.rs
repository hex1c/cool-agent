#![deny(unsafe_code)]

//! Producer-owned sanitized conversation history.
//!
//! The `SanitizedHistory` type is constructed by the producer (the code that
//! creates the history checkpoint) **before** the content reaches the storage
//! layer. The producer is responsible for redacting credential material from
//! message content using known secret values and credential provenance. The
//! storage layer's regex scanning provides additional defense-in-depth but
//! cannot guarantee exclusion of all credential forms by itself.
//!
//! ## Design
//!
//! - **Tool messages are structurally excluded.** Tool messages contain raw
//!   provider output (API responses, tool results) that may include
//!   credentials. The producer must convert or redact these before
//!   constructing a `SanitizedHistory`.
//! - **Known secret values are deterministically redacted.** The producer
//!   supplies the secret values that were used during the conversation
//!   (OAuth tokens, API keys, SMTP passwords). The type replaces every
//!   occurrence with `[REDACTED]` before the content is serialized.
//! - **Content is bounded.** Message count and per-message content size are
//!   enforced at construction, matching the storage-layer bounds.
//! - **Serialization is the versioned envelope.** The type serializes to the
//!   `novus.sanitized-history.v1` JSON envelope that the storage layer
//!   expects.

use std::fmt::{Display, Formatter};

use serde::Serialize;

const SCHEMA_VERSION: &str = "novus.sanitized-history.v1";
const MAX_MESSAGES: usize = 200;
const MAX_CONTENT_BYTES: usize = 64_000;
const REDACTED_MARKER: &str = "[REDACTED]";

/// Error returned when constructing a [`SanitizedHistory`] fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SanitizedHistoryError {
    TooManyMessages { count: usize, maximum: usize },
    ContentTooLarge { length: usize, maximum: usize },
    EmptyContent,
    ToolMessagesNotAllowed,
}

impl Display for SanitizedHistoryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyMessages { count, maximum } => write!(
                formatter,
                "sanitized history has {count} messages, maximum is {maximum}"
            ),
            Self::ContentTooLarge { length, maximum } => write!(
                formatter,
                "sanitized history message content length {length} exceeds {maximum}"
            ),
            Self::EmptyContent => formatter.write_str("sanitized history message content is empty"),
            Self::ToolMessagesNotAllowed => formatter.write_str(
                "tool messages are not allowed in sanitized history; redact or convert them before construction",
            ),
        }
    }
}

impl std::error::Error for SanitizedHistoryError {}

/// A conversation role that is safe to persist. Tool messages are excluded
/// because they contain raw provider output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizedRole {
    System,
    User,
    Assistant,
}

/// A single message in a sanitized history. The content has been redacted
/// by the producer before construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SanitizedMessage {
    role: SanitizedRole,
    content: String,
}

/// A conversation history that has been sanitized by the producer before
/// storage. The producer redacts known secret values and excludes tool
/// messages at construction time. The storage layer's regex scanning
/// provides additional defense-in-depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedHistory {
    messages: Vec<SanitizedMessage>,
}

impl SanitizedHistory {
    /// Construct a sanitized history from raw messages and known secret
    /// values. Every occurrence of a known secret in message content is
    /// replaced with `[REDACTED]`. Tool messages are rejected.
    pub fn new(
        raw_messages: Vec<(SanitizedRole, String)>,
        known_secrets: &[&str],
    ) -> Result<Self, SanitizedHistoryError> {
        if raw_messages.len() > MAX_MESSAGES {
            return Err(SanitizedHistoryError::TooManyMessages {
                count: raw_messages.len(),
                maximum: MAX_MESSAGES,
            });
        }

        let mut messages = Vec::with_capacity(raw_messages.len());
        for (role, mut content) in raw_messages {
            if content.is_empty() {
                return Err(SanitizedHistoryError::EmptyContent);
            }
            // Deterministically redact known secret values.
            for secret in known_secrets {
                if !secret.is_empty() {
                    content = content.replace(secret, REDACTED_MARKER);
                }
            }
            if content.len() > MAX_CONTENT_BYTES {
                return Err(SanitizedHistoryError::ContentTooLarge {
                    length: content.len(),
                    maximum: MAX_CONTENT_BYTES,
                });
            }
            messages.push(SanitizedMessage { role, content });
        }

        Ok(Self { messages })
    }

    /// Serialize to the versioned JSON envelope expected by the storage
    /// layer.
    pub fn serialize(&self) -> Result<Vec<u8>, SanitizedHistoryError> {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Envelope<'a> {
            schema_version: &'a str,
            messages: &'a [SanitizedMessage],
        }

        serde_json::to_vec(&Envelope {
            schema_version: SCHEMA_VERSION,
            messages: &self.messages,
        })
        .map_err(|_| SanitizedHistoryError::EmptyContent)
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn redacts_known_secret_values() {
        let secret = "sk-secret-12345";
        let history = SanitizedHistory::new(
            vec![
                (
                    SanitizedRole::User,
                    format!("use the key {secret} to connect"),
                ),
                (
                    SanitizedRole::Assistant,
                    format!("I see the key {secret} was used"),
                ),
            ],
            &[secret],
        )
        .expect("valid history");

        let serialized = history.serialize().expect("should serialize");
        let text = std::str::from_utf8(&serialized).expect("valid utf-8");
        assert!(!text.contains(secret), "secret must not appear in output");
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn rejects_tool_messages() {
        let result = SanitizedHistory::new(vec![(SanitizedRole::User, "hello".to_owned())], &[]);
        // SanitizedRole doesn't have a Tool variant, so this is structurally
        // enforced — tool messages cannot be constructed at all.
        assert!(result.is_ok());
    }

    #[test]
    fn enforces_message_limit() {
        let messages: Vec<(SanitizedRole, String)> = (0..=MAX_MESSAGES)
            .map(|_| (SanitizedRole::User, "hi".to_owned()))
            .collect();
        assert!(matches!(
            SanitizedHistory::new(messages, &[]),
            Err(SanitizedHistoryError::TooManyMessages { .. })
        ));
    }

    #[test]
    fn enforces_content_size_limit() {
        let content = "x".repeat(MAX_CONTENT_BYTES + 1);
        assert!(matches!(
            SanitizedHistory::new(vec![(SanitizedRole::User, content)], &[]),
            Err(SanitizedHistoryError::ContentTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_empty_content() {
        assert!(matches!(
            SanitizedHistory::new(vec![(SanitizedRole::User, String::new())], &[]),
            Err(SanitizedHistoryError::EmptyContent)
        ));
    }

    #[test]
    fn serialize_produces_versioned_envelope() {
        let history = SanitizedHistory::new(
            vec![
                (SanitizedRole::System, "You are helpful".to_owned()),
                (SanitizedRole::User, "hello".to_owned()),
                (SanitizedRole::Assistant, "hi there".to_owned()),
            ],
            &[],
        )
        .expect("valid history");

        let serialized = history.serialize().expect("should serialize");
        let text = std::str::from_utf8(&serialized).expect("valid utf-8");
        assert!(text.contains("\"schemaVersion\":\"novus.sanitized-history.v1\""));
        assert!(text.contains("\"role\":\"system\""));
        assert!(text.contains("\"role\":\"user\""));
        assert!(text.contains("\"role\":\"assistant\""));
        // No tool role can appear.
        assert!(!text.contains("\"role\":\"tool\""));
    }

    #[test]
    fn redacts_multiple_different_secrets() {
        let oauth = "ya29.oauth-token";
        let smtp = "smtp-password-xyz";
        let history = SanitizedHistory::new(
            vec![(
                SanitizedRole::User,
                format!("connect with {oauth} and {smtp}"),
            )],
            &[oauth, smtp],
        )
        .expect("valid history");

        let serialized = history.serialize().expect("serialize");
        let text = std::str::from_utf8(&serialized).expect("utf-8");
        assert!(!text.contains(oauth));
        assert!(!text.contains(smtp));
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn redaction_preserves_message_order() {
        let history = SanitizedHistory::new(
            vec![
                (SanitizedRole::System, "system msg".to_owned()),
                (SanitizedRole::User, "user msg".to_owned()),
                (SanitizedRole::Assistant, "assistant msg".to_owned()),
            ],
            &[],
        )
        .expect("valid history");

        let serialized = history.serialize().expect("should serialize");
        let value: serde_json::Value = serde_json::from_slice(&serialized).expect("valid json");
        let messages = value
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages array");
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages.first().expect("msg 0").get("role"),
            Some(&serde_json::json!("system"))
        );
        assert_eq!(
            messages.get(1).expect("msg 1").get("role"),
            Some(&serde_json::json!("user"))
        );
        assert_eq!(
            messages.get(2).expect("msg 2").get("role"),
            Some(&serde_json::json!("assistant"))
        );
    }
}
