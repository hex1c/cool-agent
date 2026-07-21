use std::fmt::{Debug, Display};

use domain::identity::ParticipantId;
use domain::retry::RetryPolicy;

use crate::redaction::{AccessToken, RefreshToken};

// ---------------------------------------------------------------------------
// TokenEndpoint trait — abstraction over the Google OAuth token endpoint
// ---------------------------------------------------------------------------

/// Response from the Google OAuth token endpoint.
pub struct TokenResponse {
    /// The access token for API calls.
    pub access_token: AccessToken,
    /// A refresh token, present on initial consent and possibly absent on
    /// subsequent authorizations where consent already exists.
    pub refresh_token: Option<RefreshToken>,
    /// Lifetime of the access token in seconds, if provided.
    pub expires_in: Option<u64>,
    /// Token type (typically `"Bearer"`).
    pub token_type: String,
}

/// Abstraction over the Google OAuth 2.0 token endpoint.
///
/// Implementations handle the HTTP communication with
/// `https://oauth2.googleapis.com/token` and the revocation endpoint
/// `https://oauth2.googleapis.com/revoke`. Tests use a mock implementation.
#[allow(async_fn_in_trait)]
pub trait TokenEndpoint {
    /// The error type for endpoint calls.
    type Error: Display + Debug;

    /// Exchange an authorization code for tokens using PKCE.
    async fn exchange(
        &self,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenResponse, Self::Error>;

    /// Refresh an access token using a refresh token.
    async fn refresh(&self, refresh_token: &str) -> Result<TokenResponse, Self::Error>;

    /// Revoke a token (access or refresh) at Google's revocation endpoint.
    async fn revoke(&self, token: &str) -> Result<(), Self::Error>;
}

// ---------------------------------------------------------------------------
// RefreshTokenStore trait — storage of refresh tokens behind the secret port
// ---------------------------------------------------------------------------

/// Storage abstraction for refresh tokens.
///
/// Each refresh token is stored as a separate secret value keyed by participant.
/// In production this is backed by Parameter Store `SecureString` values.
#[allow(async_fn_in_trait)]
pub trait RefreshTokenStore {
    /// The error type for store operations.
    type Error: Display + Debug;

    /// Store a refresh token for a participant.
    async fn store(
        &self,
        participant: ParticipantId,
        token: &RefreshToken,
    ) -> Result<(), Self::Error>;

    /// Load the refresh token for a participant, if one exists.
    async fn load(&self, participant: ParticipantId) -> Result<Option<RefreshToken>, Self::Error>;

    /// Delete the refresh token for a participant.
    async fn delete(&self, participant: ParticipantId) -> Result<(), Self::Error>;
}

// ---------------------------------------------------------------------------
// Token operation errors
// ---------------------------------------------------------------------------

/// Errors from token operations (exchange, refresh, revoke, store).
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// The token endpoint returned an error.
    #[error("token endpoint error: {0}")]
    Endpoint(String),
    /// All retry attempts were exhausted.
    #[error("retries exhausted: {0}")]
    RetriesExhausted(String),
    /// The token response is missing required fields.
    #[error("invalid token response from endpoint")]
    InvalidTokenResponse,
    /// The response is missing a refresh token where one is required.
    #[error("refresh token missing in token response")]
    RefreshTokenMissing,
    /// No refresh token is stored for the requested participant.
    #[error("no stored refresh token")]
    NoStoredToken,
    /// The token store returned an error.
    #[error("token store error: {0}")]
    Store(String),
    /// The retry policy is invalid (should not happen with a validated policy).
    #[error("retry policy invalid")]
    RetryPolicyInvalid,
}

// ---------------------------------------------------------------------------
// Retry helper
// ---------------------------------------------------------------------------

/// Execute an async operation with retry governed by `policy`.
///
/// Retries up to `policy.max_attempts()` times. Does not sleep between retries;
/// the caller manages backoff if needed. Uses `JitterSample::new(0)` for
/// deterministic behavior.
async fn execute_with_retry<T, E, Fut>(
    policy: RetryPolicy,
    mut call: impl FnMut() -> Fut,
) -> Result<T, TokenError>
where
    E: Display + Debug,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let max = policy.max_attempts();
    let mut last_err_msg: Option<String> = None;

    for _ in 0..max {
        match call().await {
            Ok(value) => return Ok(value),
            Err(e) => {
                last_err_msg = Some(e.to_string());
            }
        }
    }

    let msg = match last_err_msg {
        Some(m) => m,
        None => String::from("retry exhausted with no recorded error"),
    };
    Err(TokenError::RetriesExhausted(msg))
}

// ---------------------------------------------------------------------------
// Token operations with retry
// ---------------------------------------------------------------------------

/// Exchange an authorization code for tokens, with retry.
///
/// Calls [`TokenEndpoint::exchange`] up to `policy.max_attempts()` times
/// on failure.
pub async fn exchange_code(
    endpoint: &impl TokenEndpoint,
    policy: RetryPolicy,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<TokenResponse, TokenError> {
    execute_with_retry(policy, || async {
        endpoint
            .exchange(code, redirect_uri, code_verifier)
            .await
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| {
        if matches!(e, TokenError::RetriesExhausted(_)) {
            e
        } else {
            TokenError::Endpoint(e.to_string())
        }
    })
}

/// Refresh an access token using a stored refresh token, with retry.
///
/// Calls [`TokenEndpoint::refresh`] up to `policy.max_attempts()` times
/// on failure.
pub async fn refresh_access_token(
    endpoint: &impl TokenEndpoint,
    policy: RetryPolicy,
    refresh_token: &RefreshToken,
) -> Result<TokenResponse, TokenError> {
    execute_with_retry(policy, || async {
        endpoint
            .refresh(refresh_token.as_str())
            .await
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| {
        if matches!(e, TokenError::RetriesExhausted(_)) {
            e
        } else {
            TokenError::Endpoint(e.to_string())
        }
    })
}

/// Revoke a token at Google, with retry.
///
/// Calls [`TokenEndpoint::revoke`] up to `policy.max_attempts()` times
/// on failure.
pub async fn revoke_token(
    endpoint: &impl TokenEndpoint,
    policy: RetryPolicy,
    token: &RefreshToken,
) -> Result<(), TokenError> {
    execute_with_retry(policy, || async {
        endpoint
            .revoke(token.as_str())
            .await
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| {
        if matches!(e, TokenError::RetriesExhausted(_)) {
            e
        } else {
            TokenError::Endpoint(e.to_string())
        }
    })
}

// ---------------------------------------------------------------------------
// High-level token management operations
// ---------------------------------------------------------------------------

/// Complete the OAuth token exchange: exchange the authorization code for
/// tokens and store the refresh token.
pub async fn complete_exchange(
    endpoint: &impl TokenEndpoint,
    store: &impl RefreshTokenStore,
    policy: RetryPolicy,
    participant: ParticipantId,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<AccessToken, TokenError> {
    let response = exchange_code(endpoint, policy, code, redirect_uri, code_verifier).await?;
    if let Some(ref rt) = response.refresh_token {
        store
            .store(participant, rt)
            .await
            .map_err(|e| TokenError::Store(e.to_string()))?;
    }
    Ok(response.access_token)
}

/// Refresh the access token for a participant, storing any new refresh token.
pub async fn refresh_for_participant(
    endpoint: &impl TokenEndpoint,
    store: &impl RefreshTokenStore,
    policy: RetryPolicy,
    participant: ParticipantId,
) -> Result<AccessToken, TokenError> {
    let refresh_token = store
        .load(participant)
        .await
        .map_err(|e| TokenError::Store(e.to_string()))?
        .ok_or(TokenError::NoStoredToken)?;
    let response = refresh_access_token(endpoint, policy, &refresh_token).await?;
    if let Some(ref new_rt) = response.refresh_token {
        store
            .store(participant, new_rt)
            .await
            .map_err(|e| TokenError::Store(e.to_string()))?;
    }
    Ok(response.access_token)
}

/// Check whether a participant has a stored refresh token (Google is connected).
pub async fn connection_status(
    store: &impl RefreshTokenStore,
    participant: ParticipantId,
) -> Result<bool, TokenError> {
    store
        .load(participant)
        .await
        .map(|opt| opt.is_some())
        .map_err(|e| TokenError::Store(e.to_string()))
}

/// Disconnect Google for a participant: revoke the refresh token at Google and
/// delete it from the store. This pauses Google-dependent work without deleting
/// workflow history.
///
/// Even if revocation fails (e.g., the token is already invalid), the stored
/// token is still deleted so the participant can reauthorize.
pub async fn disconnect(
    endpoint: &impl TokenEndpoint,
    store: &impl RefreshTokenStore,
    policy: RetryPolicy,
    participant: ParticipantId,
) -> Result<(), TokenError> {
    let refresh_token = match store.load(participant).await {
        Ok(Some(rt)) => rt,
        Ok(None) => return Ok(()),
        Err(e) => return Err(TokenError::Store(e.to_string())),
    };

    // Try to revoke at Google; ignore errors since the token may already be
    // invalid, and we want to delete the local record regardless.
    let _revoke_result = revoke_token(endpoint, policy, &refresh_token).await;

    store
        .delete(participant)
        .await
        .map_err(|e| TokenError::Store(e.to_string()))
}

/// Prepare for reauthorization: remove any stored refresh token so the participant
/// returns to the onboarding flow. Does not delete workflow history.
pub async fn clear_for_reauth(
    store: &impl RefreshTokenStore,
    participant: ParticipantId,
) -> Result<(), TokenError> {
    store
        .delete(participant)
        .await
        .map_err(|e| TokenError::Store(e.to_string()))
}
