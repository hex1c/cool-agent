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

use std::fmt::{Debug, Display, Formatter};

use crate::ports::SecretValue;
use serde::Serialize;
use zeroize::Zeroizing;

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
    EmptySecretProvenance,
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
            Self::EmptySecretProvenance => formatter.write_str(
                "from_secret_values requires at least one secret; use no_secrets with NoSecretsUsed attestation for conversations without secrets",
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
#[derive(Clone, PartialEq, Eq, Serialize)]
struct SanitizedMessage {
    role: SanitizedRole,
    content: String,
}

/// A conversation history that has been sanitized by the producer before
/// storage. The producer redacts known secret values and excludes tool
/// messages at construction time. The storage layer's regex scanning
/// provides additional defense-in-depth.
///
/// `Debug` does not expose message content.
#[derive(Clone, PartialEq, Eq)]
pub struct SanitizedHistory {
    messages: Vec<SanitizedMessage>,
}

impl Debug for SanitizedHistory {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SanitizedHistory")
            .field("message_count", &self.messages.len())
            .finish()
    }
}

/// Explicit attestation that no secrets were used in a conversation.
/// This type can only be constructed within the application crate via
/// [`NoSecretsUsed::attest`], preventing external callers from forging
/// a no-secret claim. The caller takes responsibility for verifying
/// that no secret values appear in the message content.
#[derive(Debug, Clone, Copy)]
pub struct NoSecretsUsed {
    _private: (),
}

impl NoSecretsUsed {
    /// Explicitly attest that no secrets were used. This is `pub(crate)`
    /// so only trusted application-crate code can produce this attestation;
    /// external callers must obtain it from a trusted operation context.
    /// Explicitly attest that no secrets were used. This is `pub(crate)`
    /// so only trusted application-crate code can produce this attestation;
    /// external callers must obtain it from a trusted operation context.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) const fn attest() -> Self {
        Self { _private: () }
    }
}

/// Trusted sanitizer context that owns the known secret values for a
/// conversation. This is the **only** way to construct a
/// [`SanitizedHistory`] — the constructor is private so callers cannot
/// bypass redaction by passing an incomplete or empty secret list without
/// explicitly acknowledging it through one of the factory methods.
///
/// Secret values are zeroized when the sanitizer is dropped via
/// `Zeroizing<String>`.
pub struct HistorySanitizer {
    secrets: Vec<Zeroizing<String>>,
}

impl HistorySanitizer {
    /// Create a sanitizer from the typed secret values that were used
    /// during the conversation. Every occurrence of each secret in message
    /// content will be replaced with `[REDACTED]`. Rejects an empty list —
    /// use [`HistorySanitizer::no_secrets`] with a [`NoSecretsUsed`]
    /// attestation for conversations where no secrets were used.
    pub fn from_secret_values(secrets: Vec<SecretValue>) -> Result<Self, SanitizedHistoryError> {
        if secrets.is_empty() {
            return Err(SanitizedHistoryError::EmptySecretProvenance);
        }
        Ok(Self {
            secrets: secrets
                .iter()
                .map(|value| Zeroizing::new(String::from_utf8_lossy(value.expose()).into_owned()))
                .collect(),
        })
    }

    /// Create a sanitizer for conversations where no secrets were used.
    /// Requires an explicit [`NoSecretsUsed`] attestation — only
    /// application-crate code can produce this attestation.
    pub fn no_secrets(_attestation: NoSecretsUsed) -> Self {
        Self { secrets: vec![] }
    }

    /// Sanitize raw messages, redacting all known secret values.
    pub fn sanitize(
        &self,
        raw_messages: Vec<(SanitizedRole, String)>,
    ) -> Result<SanitizedHistory, SanitizedHistoryError> {
        let secret_refs: Vec<&str> = self.secrets.iter().map(|s| s.as_str()).collect();
        SanitizedHistory::new(raw_messages, &secret_refs)
    }
}

impl SanitizedHistory {
    /// Construct a sanitized history from raw messages and known secret
    /// values. Every occurrence of a known secret in message content is
    /// replaced with `[REDACTED]`. Tool messages are rejected.
    ///
    /// **Private** — use [`HistorySanitizer::sanitize`] to construct.
    fn new(
        raw_messages: Vec<(SanitizedRole, String)>,
        known_secrets: &[&str],
    ) -> Result<Self, SanitizedHistoryError> {
        if raw_messages.len() > MAX_MESSAGES {
            return Err(SanitizedHistoryError::TooManyMessages {
                count: raw_messages.len(),
                maximum: MAX_MESSAGES,
            });
        }

        // Deduplicate and sort secrets by length (longest first) so that
        // overlapping secrets are fully redacted before shorter substrings
        // are applied. This prevents partial leakage when one secret is a
        // substring of another.
        let mut secrets: Vec<&str> = known_secrets
            .iter()
            .copied()
            .filter(|secret| !secret.is_empty())
            .collect();
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();

        let mut messages = Vec::with_capacity(raw_messages.len());
        for (role, mut content) in raw_messages {
            if content.is_empty() {
                return Err(SanitizedHistoryError::EmptyContent);
            }
            // Deterministically redact known secret values, longest first.
            for secret in &secrets {
                content = content.replace(secret, REDACTED_MARKER);
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
        // Assembled at runtime to avoid triggering secret scanners.
        let secret = format!("{}_{}", "placeholder", "value-12345");
        let sanitizer = HistorySanitizer::from_secret_values(vec![
            SecretValue::new(secret.clone().into_bytes()).expect("non-empty"),
        ])
        .expect("non-empty secrets");
        let history = sanitizer
            .sanitize(vec![
                (
                    SanitizedRole::User,
                    format!("use the key {secret} to connect"),
                ),
                (
                    SanitizedRole::Assistant,
                    format!("I see the key {secret} was used"),
                ),
            ])
            .expect("valid history");

        let serialized = history.serialize().expect("should serialize");
        let text = std::str::from_utf8(&serialized).expect("valid utf-8");
        assert!(!text.contains(&secret), "secret must not appear in output");
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn tool_role_is_structurally_excluded() {
        // SanitizedRole has no Tool variant, so tool messages cannot be
        // constructed. This is a compile-time guarantee, not a runtime
        // check. We verify that only System, User, and Assistant are
        // valid roles.
        let roles = [
            SanitizedRole::System,
            SanitizedRole::User,
            SanitizedRole::Assistant,
        ];
        for role in roles {
            let history = HistorySanitizer::no_secrets(NoSecretsUsed::attest())
                .sanitize(vec![(role, "content".to_owned())])
                .expect("valid role should be accepted");
            assert_eq!(history.message_count(), 1);
        }
    }

    #[test]
    fn enforces_message_limit() {
        let messages: Vec<(SanitizedRole, String)> = (0..=MAX_MESSAGES)
            .map(|_| (SanitizedRole::User, "hi".to_owned()))
            .collect();
        assert!(matches!(
            HistorySanitizer::no_secrets(NoSecretsUsed::attest()).sanitize(messages),
            Err(SanitizedHistoryError::TooManyMessages { .. })
        ));
    }

    #[test]
    fn enforces_content_size_limit() {
        let content = "x".repeat(MAX_CONTENT_BYTES + 1);
        assert!(matches!(
            HistorySanitizer::no_secrets(NoSecretsUsed::attest())
                .sanitize(vec![(SanitizedRole::User, content)]),
            Err(SanitizedHistoryError::ContentTooLarge { .. })
        ));
    }

    #[test]
    fn rejects_empty_content() {
        assert!(matches!(
            HistorySanitizer::no_secrets(NoSecretsUsed::attest())
                .sanitize(vec![(SanitizedRole::User, String::new())]),
            Err(SanitizedHistoryError::EmptyContent)
        ));
    }

    #[test]
    fn serialize_produces_versioned_envelope() {
        let history = HistorySanitizer::no_secrets(NoSecretsUsed::attest())
            .sanitize(vec![
                (SanitizedRole::System, "You are helpful".to_owned()),
                (SanitizedRole::User, "hello".to_owned()),
                (SanitizedRole::Assistant, "hi there".to_owned()),
            ])
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
        // Assembled at runtime to avoid triggering secret scanners.
        let oauth = format!("{}_{}", "oauth", "token-fragment");
        let smtp = format!("{}_{}", "smtp", "pass-fragment");
        let sanitizer = HistorySanitizer::from_secret_values(vec![
            SecretValue::new(oauth.clone().into_bytes()).expect("non-empty"),
            SecretValue::new(smtp.clone().into_bytes()).expect("non-empty"),
        ])
        .expect("non-empty secrets");
        let history = sanitizer
            .sanitize(vec![(
                SanitizedRole::User,
                format!("connect with {oauth} and {smtp}"),
            )])
            .expect("valid history");

        let serialized = history.serialize().expect("serialize");
        let text = std::str::from_utf8(&serialized).expect("utf-8");
        assert!(!text.contains(&oauth));
        assert!(!text.contains(&smtp));
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_overlapping_secrets_longest_first() {
        // When one secret is a substring of another, the longer secret
        // must be redacted first to prevent partial leakage.
        let long = format!("{}_{}_{}", "alpha", "beta", "gamma");
        let short = format!("{}_{}", "alpha", "beta");
        let sanitizer = HistorySanitizer::from_secret_values(vec![
            SecretValue::new(long.clone().into_bytes()).expect("non-empty"),
            SecretValue::new(short.clone().into_bytes()).expect("non-empty"),
        ])
        .expect("non-empty secrets");
        let history = sanitizer
            .sanitize(vec![(SanitizedRole::User, format!("value is {long} here"))])
            .expect("valid history");

        let serialized = history.serialize().expect("serialize");
        let text = std::str::from_utf8(&serialized).expect("utf-8");
        assert!(!text.contains(&long), "long secret must be fully redacted");
        assert!(
            !text.contains(&short),
            "short secret must be fully redacted"
        );
    }

    #[test]
    fn redaction_preserves_message_order() {
        let history = HistorySanitizer::no_secrets(NoSecretsUsed::attest())
            .sanitize(vec![
                (SanitizedRole::System, "system msg".to_owned()),
                (SanitizedRole::User, "user msg".to_owned()),
                (SanitizedRole::Assistant, "assistant msg".to_owned()),
            ])
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
