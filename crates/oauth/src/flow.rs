use std::fmt::{Debug, Display};
use std::time::{Duration, SystemTime};

use application::ports::OAuthStateDigest;
use base64::Engine;
use domain::identity::ParticipantId;
use sha2::{Digest, Sha256};

use crate::redaction::{AuthorizationCode, OAuthStateValue, PkceVerifier};

/// Number of random bytes for PKCE verifier (before base64url encoding).
const PKCE_VERIFIER_BYTES: usize = 32;

/// TTL for an OAuth state record (10 minutes).
pub const STATE_TTL: Duration = Duration::from_secs(600);

/// A PKCE verifier and its S256 challenge.
pub struct PkcePair {
    pub verifier: PkceVerifier,
    pub challenge: String,
}

/// Generate a PKCE verifier (32 random bytes, base64url-encoded without padding)
/// and its SHA-256 code challenge.
///
/// # Errors
///
/// Returns [`FlowError::Random`] if the system randomness source is unavailable.
pub fn generate_pkce() -> Result<PkcePair, FlowError> {
    let mut bytes = [0u8; PKCE_VERIFIER_BYTES];
    getrandom::getrandom(&mut bytes).map_err(FlowError::Random)?;
    let verifier_str = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let hash = Sha256::digest(verifier_str.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash);
    Ok(PkcePair {
        verifier: PkceVerifier::new(verifier_str),
        challenge,
    })
}

/// Generate a random OAuth state value (32 random bytes, base64url-encoded without padding).
///
/// # Errors
///
/// Returns [`FlowError::Random`] if the system randomness source is unavailable.
pub fn generate_state() -> Result<OAuthStateValue, FlowError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(FlowError::Random)?;
    let state_str = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    Ok(OAuthStateValue::new(state_str))
}

/// Build a Google OAuth 2.0 authorization URL using the authorization-code flow
/// with PKCE and offline access.
///
/// The returned URL includes `access_type=offline` to request a refresh token
/// on initial consent.
///
/// # Errors
///
/// Returns [`FlowError::UrlParse`] if the Google authorization endpoint URL
/// cannot be constructed (should never happen with the hard-coded base URL).
pub fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    scopes: &[&str],
    state: &OAuthStateValue,
    challenge: &str,
) -> Result<String, FlowError> {
    let mut url = url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|_| FlowError::UrlParse)?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("client_id", client_id);
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("response_type", "code");
        query.append_pair("scope", &scopes.join(" "));
        query.append_pair("access_type", "offline");
        query.append_pair("state", state.as_str());
        query.append_pair("code_challenge", challenge);
        query.append_pair("code_challenge_method", "S256");
    }
    Ok(url.to_string())
}

/// Compute the SHA-256 digest of a state value for store lookup.
pub fn state_digest(state: &OAuthStateValue) -> OAuthStateDigest {
    let hash = Sha256::digest(state.as_str().as_bytes());
    let mut digest_bytes = [0u8; 32];
    let hash_slice = hash.as_slice();
    let len = hash_slice.len().min(32);
    if let (Some(dest), Some(src)) = (digest_bytes.get_mut(..len), hash_slice.get(..len)) {
        dest.copy_from_slice(src);
    }
    OAuthStateDigest::new(digest_bytes)
}

/// A pending OAuth state record stored during the authorization flow.
#[derive(Clone)]
pub struct PendingState {
    /// The Telegram participant who initiated this authorization.
    pub participant: ParticipantId,
    /// The PKCE code verifier needed to exchange the authorization code.
    pub code_verifier: PkceVerifier,
    /// When this state record was created, for expiry checks.
    pub created_at: SystemTime,
}

/// Abstraction for storing and atomically consuming OAuth state records.
///
/// Implementations must make [`consume`](StateStore::consume) atomic: if two
/// concurrent callbacks present the same state value, only one may succeed.
#[allow(async_fn_in_trait)]
pub trait StateStore {
    /// The error type for store operations.
    type Error: Display + Debug;

    /// Store a pending OAuth state record keyed by its SHA-256 digest.
    async fn store(
        &self,
        digest: &OAuthStateDigest,
        record: PendingState,
    ) -> Result<(), Self::Error>;

    /// Atomically consume a state record.
    ///
    /// Returns `Some(record)` if the state exists and hasn't been consumed yet,
    /// or `None` if it doesn't exist or has already been consumed.
    async fn consume(&self, digest: &OAuthStateDigest)
    -> Result<Option<PendingState>, Self::Error>;
}

/// The result of successfully verifying an OAuth callback.
#[derive(Debug)]
pub struct CallbackResult {
    /// The participant who initiated the authorization.
    pub participant: ParticipantId,
    /// The PKCE code verifier needed for token exchange.
    pub code_verifier: PkceVerifier,
    /// The authorization code from the callback query string.
    pub authorization_code: AuthorizationCode,
}

/// Errors that can occur during OAuth flow setup (PKCE, state generation, URL construction).
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// The system randomness source is unavailable.
    #[error("randomness source unavailable: {0}")]
    Random(#[from] getrandom::Error),
    /// Failed to construct the Google authorization URL.
    #[error("failed to construct authorization URL")]
    UrlParse,
}

/// Errors that can occur during callback verification.
#[derive(Debug, thiserror::Error)]
pub enum CallbackError<E: Display + Debug> {
    /// The state was not found (never existed or already consumed).
    #[error("state not found or already consumed")]
    StateNotFound,
    /// The state has expired (beyond `STATE_TTL`).
    #[error("state expired")]
    StateExpired,
    /// The state was created for a different participant.
    #[error("participant mismatch: state bound to a different user")]
    ParticipantMismatch,
    /// The underlying state store returned an error.
    #[error("state store error: {0}")]
    Store(E),
}

/// Verify an OAuth callback using the participant bound to the stored state.
///
/// This is the provider-callback path: Google redirects to one fixed callback
/// URI, so participant identity comes only from the single-use state record.
pub async fn verify_provider_callback<S: StateStore>(
    store: &S,
    state: &OAuthStateValue,
    authorization_code: AuthorizationCode,
) -> Result<CallbackResult, CallbackError<S::Error>> {
    let digest = state_digest(state);
    let record = store
        .consume(&digest)
        .await
        .map_err(CallbackError::Store)?
        .ok_or(CallbackError::StateNotFound)?;

    if state_is_expired(record.created_at) {
        return Err(CallbackError::StateExpired);
    }

    Ok(CallbackResult {
        participant: record.participant,
        code_verifier: record.code_verifier,
        authorization_code,
    })
}

/// Verify an OAuth callback by validating the state parameter and consuming it atomically.
///
/// This function:
/// 1. Computes the SHA-256 digest of the incoming state value.
/// 2. Atomically consumes the state record from the store.
/// 3. Verifies the state hasn't expired (within `STATE_TTL`).
/// 4. Verifies the participant matches the expected participant.
///
/// Replay is prevented by the atomic consume: once consumed, a second callback
/// with the same state will get `StateNotFound`.
///
/// # Errors
///
/// - [`CallbackError::StateNotFound`] — state never existed or already consumed (replay).
/// - [`CallbackError::StateExpired`] — state is older than `STATE_TTL`.
/// - [`CallbackError::ParticipantMismatch`] — state was created for a different user.
/// - [`CallbackError::Store`] — the state store returned an error.
pub async fn verify_callback<S: StateStore>(
    store: &S,
    state: &OAuthStateValue,
    authorization_code: AuthorizationCode,
    expected_participant: ParticipantId,
) -> Result<CallbackResult, CallbackError<S::Error>> {
    let digest = state_digest(state);
    let record = store
        .consume(&digest)
        .await
        .map_err(CallbackError::Store)?
        .ok_or(CallbackError::StateNotFound)?;

    if state_is_expired(record.created_at) {
        return Err(CallbackError::StateExpired);
    }

    if record.participant != expected_participant {
        return Err(CallbackError::ParticipantMismatch);
    }

    Ok(CallbackResult {
        participant: record.participant,
        code_verifier: record.code_verifier,
        authorization_code,
    })
}

fn state_is_expired(created_at: SystemTime) -> bool {
    SystemTime::now()
        .duration_since(created_at)
        .is_ok_and(|elapsed| elapsed >= STATE_TTL)
}
