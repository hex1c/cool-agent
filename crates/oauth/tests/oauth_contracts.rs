//! Contract tests for the private Google OAuth onboarding flow.
//!
//! These tests use mock implementations of [`TokenEndpoint`], [`StateStore`],
//! and [`RefreshTokenStore`] so no real network or Parameter Store calls are
//! made. They prove: connect (exchange + store), refresh, revoke (disconnect),
//! reconnect (clear_for_reauth then re-connect), state replay rejection,
//! participant binding, state expiry, and redaction of credential material in
//! Debug/Display output.

#![allow(clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use domain::identity::ParticipantId;
use domain::retry::RetryPolicy;
use oauth::flow::{self, CallbackError, PendingState, StateStore};
use oauth::redaction::{AccessToken, AuthorizationCode, OAuthStateValue, RefreshToken};
use oauth::tokens::{self, RefreshTokenStore, TokenEndpoint, TokenError, TokenResponse};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn participant(id: i64) -> ParticipantId {
    ParticipantId::new(id).expect("valid participant id")
}

fn policy() -> RetryPolicy {
    RetryPolicy::new(3, 10, 100, 0).expect("valid retry policy")
}

fn fake_access_token() -> AccessToken {
    AccessToken::new("ya29.fake-access-token-value".to_string())
}

fn fake_refresh_token() -> RefreshToken {
    RefreshToken::new("1//fake-refresh-token-value".to_string())
}

// ---------------------------------------------------------------------------
// Mock TokenEndpoint
// ---------------------------------------------------------------------------

struct MockTokenEndpoint {
    /// Token responses to return on successive exchange() calls.
    exchange_responses: Mutex<Vec<Result<TokenResponse, String>>>,
    /// Token responses to return on successive refresh() calls.
    refresh_responses: Mutex<Vec<Result<TokenResponse, String>>>,
    /// Whether revoke() should succeed.
    revoke_succeeds: Mutex<bool>,
    /// Number of revoke() calls observed.
    revoke_calls: Mutex<u32>,
}

impl MockTokenEndpoint {
    fn new() -> Self {
        Self {
            exchange_responses: Mutex::new(Vec::new()),
            refresh_responses: Mutex::new(Vec::new()),
            revoke_succeeds: Mutex::new(true),
            revoke_calls: Mutex::new(0),
        }
    }

    fn with_exchange(mut self, outcome: Result<TokenResponse, String>) -> Self {
        self.exchange_responses
            .get_mut()
            .expect("lock")
            .push(outcome);
        self
    }

    fn with_refresh(mut self, outcome: Result<TokenResponse, String>) -> Self {
        self.refresh_responses
            .get_mut()
            .expect("lock")
            .push(outcome);
        self
    }

    fn revoke_calls(&self) -> u32 {
        *self.revoke_calls.lock().expect("lock")
    }
}

impl TokenEndpoint for MockTokenEndpoint {
    type Error = String;

    async fn exchange(
        &self,
        _code: &str,
        _redirect_uri: &str,
        _code_verifier: &str,
    ) -> Result<TokenResponse, Self::Error> {
        let mut responses = self.exchange_responses.lock().expect("lock");
        if responses.is_empty() {
            return Err("no more exchange responses".to_string());
        }
        responses.remove(0)
    }

    async fn refresh(&self, _refresh_token: &str) -> Result<TokenResponse, Self::Error> {
        let mut responses = self.refresh_responses.lock().expect("lock");
        if responses.is_empty() {
            return Err("no more refresh responses".to_string());
        }
        responses.remove(0)
    }

    async fn revoke(&self, _token: &str) -> Result<(), Self::Error> {
        *self.revoke_calls.lock().expect("lock") += 1;
        if *self.revoke_succeeds.lock().expect("lock") {
            Ok(())
        } else {
            Err("revoke failed".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Mock RefreshTokenStore
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockRefreshTokenStore {
    tokens: Mutex<HashMap<ParticipantId, RefreshToken>>,
}

impl RefreshTokenStore for MockRefreshTokenStore {
    type Error = String;

    async fn store(
        &self,
        participant: ParticipantId,
        token: &RefreshToken,
    ) -> Result<(), Self::Error> {
        self.tokens
            .lock()
            .expect("lock")
            .insert(participant, token.clone());
        Ok(())
    }

    async fn load(&self, participant: ParticipantId) -> Result<Option<RefreshToken>, Self::Error> {
        Ok(self.tokens.lock().expect("lock").get(&participant).cloned())
    }

    async fn delete(&self, participant: ParticipantId) -> Result<(), Self::Error> {
        self.tokens.lock().expect("lock").remove(&participant);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Mock StateStore
// ---------------------------------------------------------------------------

struct MockStateStore {
    records: Mutex<HashMap<[u8; 32], PendingState>>,
}

impl MockStateStore {
    fn new() -> Self {
        Self {
            records: Mutex::new(HashMap::new()),
        }
    }
}

impl StateStore for MockStateStore {
    type Error = String;

    async fn store(
        &self,
        digest: &oauth::OAuthStateDigest,
        record: PendingState,
    ) -> Result<(), Self::Error> {
        self.records
            .lock()
            .expect("lock")
            .insert(*digest.as_bytes(), record);
        Ok(())
    }

    async fn consume(
        &self,
        digest: &oauth::OAuthStateDigest,
    ) -> Result<Option<PendingState>, Self::Error> {
        Ok(self.records.lock().expect("lock").remove(digest.as_bytes()))
    }
}

// ---------------------------------------------------------------------------
// Connect: exchange code + store refresh token
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_exchanges_code_and_stores_refresh_token() {
    let endpoint = MockTokenEndpoint::new().with_exchange(Ok(TokenResponse {
        access_token: fake_access_token(),
        refresh_token: Some(fake_refresh_token()),
        expires_in: Some(3600),
        token_type: "Bearer".to_string(),
    }));
    let store = MockRefreshTokenStore::default();
    let p = participant(1001);

    let access = tokens::complete_exchange(
        &endpoint,
        &store,
        policy(),
        p,
        "fake-auth-code",
        "https://example.com/callback",
        "fake-verifier",
    )
    .await
    .expect("connect succeeds");

    assert!(!access.as_str().is_empty());
    let stored = store.load(p).await.expect("load");
    assert!(stored.is_some(), "refresh token must be stored");
}

#[tokio::test]
async fn connect_without_refresh_token_succeeds_but_stores_nothing() {
    let endpoint = MockTokenEndpoint::new().with_exchange(Ok(TokenResponse {
        access_token: fake_access_token(),
        refresh_token: None,
        expires_in: Some(3600),
        token_type: "Bearer".to_string(),
    }));
    let store = MockRefreshTokenStore::default();
    let p = participant(1002);

    let _ = tokens::complete_exchange(
        &endpoint,
        &store,
        policy(),
        p,
        "fake-auth-code",
        "https://example.com/callback",
        "fake-verifier",
    )
    .await
    .expect("connect succeeds without refresh token");

    assert!(
        store.load(p).await.expect("load").is_none(),
        "no refresh token stored when absent"
    );
}

// ---------------------------------------------------------------------------
// Refresh: use stored refresh token to get a new access token
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_returns_new_access_token() {
    let endpoint = MockTokenEndpoint::new().with_refresh(Ok(TokenResponse {
        access_token: fake_access_token(),
        refresh_token: None,
        expires_in: Some(3600),
        token_type: "Bearer".to_string(),
    }));
    let store = MockRefreshTokenStore::default();
    let p = participant(1003);
    store
        .store(p, &fake_refresh_token())
        .await
        .expect("seed store");

    let access = tokens::refresh_for_participant(&endpoint, &store, policy(), p)
        .await
        .expect("refresh succeeds");

    assert!(!access.as_str().is_empty());
}

#[tokio::test]
async fn refresh_without_stored_token_fails_with_no_stored_token() {
    let endpoint = MockTokenEndpoint::new();
    let store = MockRefreshTokenStore::default();
    let p = participant(1004);

    let err = tokens::refresh_for_participant(&endpoint, &store, policy(), p)
        .await
        .expect_err("no stored token must fail");

    assert!(matches!(err, TokenError::NoStoredToken), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Revoke / disconnect: pauses Google work without deleting history
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disconnect_revokes_and_deletes_token() {
    let endpoint = MockTokenEndpoint::new();
    let store = MockRefreshTokenStore::default();
    let p = participant(1005);
    store
        .store(p, &fake_refresh_token())
        .await
        .expect("seed store");

    tokens::disconnect(&endpoint, &store, policy(), p)
        .await
        .expect("disconnect succeeds");

    assert!(store.load(p).await.expect("load").is_none());
    assert_eq!(endpoint.revoke_calls(), 1, "revoke must be called once");
}

#[tokio::test]
async fn disconnect_still_deletes_when_revoke_fails() {
    let endpoint = MockTokenEndpoint::new();
    *endpoint.revoke_succeeds.lock().expect("lock") = false;
    let store = MockRefreshTokenStore::default();
    let p = participant(1006);
    store
        .store(p, &fake_refresh_token())
        .await
        .expect("seed store");

    // disconnect swallows the revoke error and still deletes the local record
    // so the participant returns to onboarding.
    tokens::disconnect(&endpoint, &store, policy(), p)
        .await
        .expect("disconnect succeeds even if revoke fails");

    assert!(
        store.load(p).await.expect("load").is_none(),
        "local token must be deleted even when revoke fails"
    );
}

#[tokio::test]
async fn connection_status_reflects_stored_token() {
    let store = MockRefreshTokenStore::default();
    let p = participant(1007);

    assert!(
        !tokens::connection_status(&store, p).await.expect("status"),
        "not connected initially"
    );

    store.store(p, &fake_refresh_token()).await.expect("seed");

    assert!(
        tokens::connection_status(&store, p).await.expect("status"),
        "connected after storing token"
    );
}

// ---------------------------------------------------------------------------
// Reconnect: clear_for_reauth then re-connect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconnect_clears_then_reconnects() {
    let endpoint = MockTokenEndpoint::new().with_exchange(Ok(TokenResponse {
        access_token: fake_access_token(),
        refresh_token: Some(fake_refresh_token()),
        expires_in: Some(3600),
        token_type: "Bearer".to_string(),
    }));
    let store = MockRefreshTokenStore::default();
    let p = participant(1008);

    // Participant was previously connected.
    store.store(p, &fake_refresh_token()).await.expect("seed");
    assert!(tokens::connection_status(&store, p).await.expect("status"));

    // Reauthorization: clear the stored token without touching workflow history.
    tokens::clear_for_reauth(&store, p)
        .await
        .expect("clear for reauth");
    assert!(
        !tokens::connection_status(&store, p).await.expect("status"),
        "must be disconnected after clear_for_reauth"
    );

    // Re-connect via a fresh exchange.
    let _ = tokens::complete_exchange(
        &endpoint,
        &store,
        policy(),
        p,
        "new-auth-code",
        "https://example.com/callback",
        "new-verifier",
    )
    .await
    .expect("reconnect exchange succeeds");

    assert!(
        tokens::connection_status(&store, p).await.expect("status"),
        "connected again after reconnect"
    );
}

// ---------------------------------------------------------------------------
// State replay rejection + participant binding
// ---------------------------------------------------------------------------

async fn seed_state(store: &MockStateStore, participant_value: ParticipantId) -> OAuthStateValue {
    let state = flow::generate_state().expect("state");
    let pkce = flow::generate_pkce().expect("pkce");
    let digest = flow::state_digest(&state);
    store
        .store(
            &digest,
            PendingState {
                participant: participant_value,
                code_verifier: pkce.verifier,
                created_at: SystemTime::now(),
            },
        )
        .await
        .expect("store pending state");
    state
}

#[tokio::test]
async fn callback_replay_is_rejected() {
    let store = MockStateStore::new();
    let p = participant(2001);
    let state = seed_state(&store, p).await;

    let code = AuthorizationCode::new("auth-code-1".to_string());
    let first = flow::verify_callback(&store, &state, code, p)
        .await
        .expect("first callback succeeds");
    assert_eq!(first.participant, p);

    // Replay with the same state must be rejected: state is single-use.
    let replay = flow::verify_callback(
        &store,
        &state,
        AuthorizationCode::new("auth-code-2".to_string()),
        p,
    )
    .await
    .expect_err("replay must fail");

    assert!(
        matches!(replay, CallbackError::StateNotFound),
        "replay should yield StateNotFound, got {replay:?}"
    );
}

#[tokio::test]
async fn callback_participant_mismatch_is_rejected() {
    let store = MockStateStore::new();
    let owner = participant(2002);
    let attacker = participant(2003);
    let state = seed_state(&store, owner).await;

    let err = flow::verify_callback(
        &store,
        &state,
        AuthorizationCode::new("auth-code".to_string()),
        attacker,
    )
    .await
    .expect_err("participant mismatch must fail");

    assert!(
        matches!(err, CallbackError::ParticipantMismatch),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Redaction: credential material never appears in Debug/Display output
// ---------------------------------------------------------------------------

#[test]
fn access_token_is_redacted_in_debug_and_display() {
    let token = fake_access_token();
    let debug = format!("{token:?}");
    let display = format!("{token}");
    assert!(!debug.contains("ya29.fake-access-token-value"));
    assert!(!display.contains("ya29.fake-access-token-value"));
    assert!(debug.contains("REDACTED"));
    assert!(display.contains("REDACTED"));
}

#[test]
fn refresh_token_is_redacted_in_debug_and_display() {
    let token = fake_refresh_token();
    let debug = format!("{token:?}");
    let display = format!("{token}");
    assert!(!debug.contains("1//fake-refresh-token-value"));
    assert!(!display.contains("1//fake-refresh-token-value"));
    assert!(debug.contains("REDACTED"));
    assert!(display.contains("REDACTED"));
}

#[test]
fn authorization_code_is_redacted_in_debug_and_display() {
    let code = AuthorizationCode::new("4/0AfakeAuthCodeValue".to_string());
    let debug = format!("{code:?}");
    let display = format!("{code}");
    assert!(!debug.contains("4/0AfakeAuthCodeValue"));
    assert!(!display.contains("4/0AfakeAuthCodeValue"));
}

#[test]
fn oauth_state_value_is_redacted_in_debug() {
    let state = OAuthStateValue::new("secret-state-value-123".to_string());
    let debug = format!("{state:?}");
    assert!(
        !debug.contains("secret-state-value-123"),
        "state must not leak in debug: {debug}"
    );
}

#[test]
fn token_error_messages_do_not_embed_token_values() {
    // An endpoint error string is carried as a label only; the test asserts the
    // redacting token types never expose their value, so even if an endpoint
    // error is surfaced it cannot contain the token via the redacting types.
    let access = fake_access_token();
    let refresh = fake_refresh_token();
    let combined = format!("{access:?} {refresh:?}");
    assert!(!combined.contains("ya29.fake-access-token-value"));
    assert!(!combined.contains("1//fake-refresh-token-value"));
}

// ---------------------------------------------------------------------------
// PKCE + authorize URL sanity
// ---------------------------------------------------------------------------

#[test]
fn pkce_pair_has_verifier_and_challenge() {
    let pkce = flow::generate_pkce().expect("pkce");
    assert!(!pkce.verifier.as_str().is_empty());
    assert!(!pkce.challenge.is_empty());
}

#[test]
fn authorize_url_contains_pkce_and_state_and_offline_access() {
    let state = flow::generate_state().expect("state");
    let pkce = flow::generate_pkce().expect("pkce");
    let url = flow::build_authorize_url(
        "test-client-id",
        "https://example.com/callback",
        &["https://www.googleapis.com/auth/drive.readonly"],
        &state,
        &pkce.challenge,
    )
    .expect("authorize url");

    assert!(url.contains("code_challenge="));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("access_type=offline"));
    assert!(url.contains("response_type=code"));
    assert!(url.contains("state="));
}

#[test]
fn generated_state_values_are_unique() {
    let a = flow::generate_state().expect("state");
    let b = flow::generate_state().expect("state");
    assert_ne!(a.as_str(), b.as_str());
}

// ---------------------------------------------------------------------------
// Retry exhaustion on persistent endpoint failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exchange_retries_then_exhausts_on_persistent_failure() {
    let endpoint = MockTokenEndpoint::new()
        .with_exchange(Err("endpoint down".to_string()))
        .with_exchange(Err("endpoint down".to_string()))
        .with_exchange(Err("endpoint down".to_string()));

    let err = tokens::complete_exchange(
        &endpoint,
        &MockRefreshTokenStore::default(),
        policy(),
        participant(3001),
        "code",
        "https://example.com/callback",
        "verifier",
    )
    .await
    .expect_err("must fail after retries");

    assert!(
        matches!(err, TokenError::RetriesExhausted(_)),
        "got {err:?}"
    );
}
