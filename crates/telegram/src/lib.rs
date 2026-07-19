#![deny(unsafe_code)]

//! Telegram webhook verification and update normalization.
//!
//! Converts authenticated Telegram webhook updates into typed internal domain
//! events, deduplicates by `update_id`, and rejects unsupported chat types.

pub mod callbacks;
pub mod client;
pub mod commands;
pub mod delivery;
pub mod normalize;
pub mod privacy;
pub mod webhook;

pub use callbacks::{CallbackData, CallbackValidationError, validate_callback_against_pending};
pub use client::TelegramBot;
pub use commands::{CommandParseError, TopicCommand, parse_topic_command};
pub use delivery::{DeliveryOutcome, PrivacyViolation, PrivateDelivery, TopicDelivery};
pub use normalize::{EventKind, MediaKind, NormalizeError, NormalizedUpdate};
pub use privacy::{PayloadClassification, classify_payload};
pub use webhook::{WebhookError, WebhookVerifier};
