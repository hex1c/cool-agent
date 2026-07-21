//! Private Google OAuth onboarding for Novus.
//!
//! This crate implements the OAuth 2.0 authorization-code flow with PKCE for
//! connecting personal Google accounts to Telegram participants. All OAuth
//! material (tokens, authorization codes, state values, PKCE verifiers) is
//! redacted in `Debug` and `Display` output and must never enter topic
//! delivery or logs.
//!
//! # Modules
//!
//! - [`flow`] — PKCE generation, state management, authorize URL construction,
//!   and callback verification (replay-resistant, participant-bound).
//! - [`tokens`] — Token endpoint abstraction, refresh token storage, token
//!   exchange/refresh/revoke with retry, connection status, disconnect, and
//!   reauthorization.
//! - [`redaction`] — Redacting wrapper types for access tokens, refresh tokens,
//!   authorization codes, PKCE verifiers, and state values.

pub mod flow;
pub mod redaction;
pub mod tokens;

/// Re-exported so consumers (and tests) can name the digest type returned by
/// [`flow::state_digest`] and accepted by [`flow::StateStore`] without a direct
/// dependency on the `application` crate.
pub use application::ports::OAuthStateDigest;
