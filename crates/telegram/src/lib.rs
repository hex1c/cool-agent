#![deny(unsafe_code)]

//! Telegram webhook verification and update normalization.
//!
//! Converts authenticated Telegram webhook updates into typed internal domain
//! events, deduplicates by `update_id`, and rejects unsupported chat types.

pub mod client;
pub mod normalize;
pub mod webhook;

pub use client::TelegramBot;
pub use normalize::{EventKind, MediaKind, NormalizeError, NormalizedUpdate};
pub use webhook::{WebhookError, WebhookVerifier};
