use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::sync::Arc;
use std::time::SystemTime;

use domain::identity::ParticipantId;
use domain::retry::MAX_EXTERNAL_OPERATION_ATTEMPTS;
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use oauth::flow::{
    self, PendingState, StateStore, build_authorize_url, generate_pkce, generate_state,
    state_digest, verify_provider_callback,
};
use oauth::google_endpoint::GoogleTokenEndpoint;
use oauth::redaction::{AuthorizationCode, OAuthStateValue, RefreshToken};
#[cfg(test)]
use oauth::tokens::TokenResponse;
use oauth::tokens::{RefreshTokenStore, TokenEndpoint, complete_exchange};
use serde::{Deserialize, Serialize};

#[cfg(test)]
use oauth::redaction::AccessToken;

// ── API Gateway event ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ApiGatewayEvent {
    #[serde(default)]
    #[serde(rename = "queryStringParameters")]
    query_string_parameters: Option<HashMap<String, String>>,
    #[serde(default)]
    #[serde(rename = "requestContext")]
    request_context: Option<RequestContext>,
}

#[derive(Debug, Deserialize)]
struct RequestContext {
    #[serde(default)]
    http: HttpContext,
}

#[derive(Debug, Deserialize, Default)]
struct HttpContext {
    #[serde(default)]
    method: String,
    #[serde(default)]
    path: String,
}

// ── API Gateway response ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ApiGatewayResponse {
    #[serde(rename = "statusCode")]
    status_code: u16,
    headers: HashMap<String, String>,
    body: String,
}

// ── Typed OAuth response ───────────────────────────────────────────────

/// Bounded, typed HTTP response for the OAuth handler.
///
/// Every variant maps to a single HTTP status code and a stable body label.
/// No tokens, codes, or secret values appear in bodies or logs — all output
/// uses redacted/stable labels via `oauth::redaction` types.
enum OauthResponse {
    /// Authorize URL built successfully.
    AuthorizeUrl { url: String },
    /// Token exchange completed, participant connected.
    Connected,
    /// State not found or already consumed (replay).
    InvalidState,
    /// State has expired.
    StateExpired,
    /// Token exchange or store failure.
    ExchangeFailed,
    /// State was created by a different participant.
    ParticipantMismatch,
    /// Required query or path parameters are missing.
    MissingParams,
    /// HTTP method not supported for this path.
    MethodNotAllowed,
    /// Path not recognized.
    NotFound,
}

impl Debug for OauthResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Manual Debug so AuthorizeUrl never risks leaking the URL into logs;
        // all other variants are stable labels.
        match self {
            Self::AuthorizeUrl { .. } => f.write_str("OauthResponse::AuthorizeUrl"),
            Self::Connected => f.write_str("OauthResponse::Connected"),
            Self::InvalidState => f.write_str("OauthResponse::InvalidState"),
            Self::StateExpired => f.write_str("OauthResponse::StateExpired"),
            Self::ExchangeFailed => f.write_str("OauthResponse::ExchangeFailed"),
            Self::ParticipantMismatch => f.write_str("OauthResponse::ParticipantMismatch"),
            Self::MissingParams => f.write_str("OauthResponse::MissingParams"),
            Self::MethodNotAllowed => f.write_str("OauthResponse::MethodNotAllowed"),
            Self::NotFound => f.write_str("OauthResponse::NotFound"),
        }
    }
}

// ── Conversion to API Gateway format ───────────────────────────────────

impl OauthResponse {
    fn into_api_gateway(self) -> ApiGatewayResponse {
        let mut headers = HashMap::new();
        headers.insert("Content-Type".into(), "application/json".into());

        match self {
            Self::AuthorizeUrl { url } => {
                let body = serde_json::json!({ "url": url }).to_string();
                ApiGatewayResponse {
                    status_code: 200,
                    headers,
                    body,
                }
            }
            Self::Connected => ApiGatewayResponse {
                status_code: 200,
                headers,
                body: r#"{"status":"connected"}"#.into(),
            },
            Self::InvalidState => ApiGatewayResponse {
                status_code: 400,
                headers,
                body: r#"{"error":"invalid_state"}"#.into(),
            },
            Self::StateExpired => ApiGatewayResponse {
                status_code: 400,
                headers,
                body: r#"{"error":"state_expired"}"#.into(),
            },
            Self::ExchangeFailed => ApiGatewayResponse {
                status_code: 502,
                headers,
                body: r#"{"error":"exchange_failed"}"#.into(),
            },
            Self::ParticipantMismatch => ApiGatewayResponse {
                status_code: 403,
                headers,
                body: r#"{"error":"participant_mismatch"}"#.into(),
            },
            Self::MissingParams => ApiGatewayResponse {
                status_code: 400,
                headers,
                body: r#"{"error":"missing_params"}"#.into(),
            },
            Self::MethodNotAllowed => ApiGatewayResponse {
                status_code: 405,
                headers,
                body: r#"{"error":"method_not_allowed"}"#.into(),
            },
            Self::NotFound => ApiGatewayResponse {
                status_code: 404,
                headers,
                body: r#"{"error":"not_found"}"#.into(),
            },
        }
    }
}

// ── Route matching helpers ─────────────────────────────────────────────

fn path_segments(path: &str) -> Vec<&str> {
    path.trim_matches('/').split('/').collect::<Vec<_>>()
}

fn parse_participant_id(raw: &str) -> Result<ParticipantId, ()> {
    let id: i64 = raw.parse().map_err(|_| ())?;
    ParticipantId::new(id).map_err(|_| ())
}

// ── Handler logic ──────────────────────────────────────────────────────

/// Process an OAuth authorize request: generate PKCE, state, store, and
/// return the Google authorization URL.
async fn process_authorize<S: StateStore>(
    store: &S,
    participant_id: ParticipantId,
    client_id: &str,
    redirect_uri: &str,
) -> OauthResponse
where
    S::Error: Display + Debug,
{
    let pkce = match generate_pkce() {
        Ok(p) => p,
        Err(_) => return OauthResponse::ExchangeFailed,
    };
    let state = match generate_state() {
        Ok(s) => s,
        Err(_) => return OauthResponse::ExchangeFailed,
    };
    let digest = state_digest(&state);

    let pending = PendingState {
        participant: participant_id,
        code_verifier: pkce.verifier,
        created_at: SystemTime::now(),
    };

    if store.store(&digest, pending).await.is_err() {
        return OauthResponse::ExchangeFailed;
    }

    let scopes = &["https://www.googleapis.com/auth/drive.file"];
    match build_authorize_url(client_id, redirect_uri, scopes, &state, &pkce.challenge) {
        Ok(url) => OauthResponse::AuthorizeUrl { url },
        Err(_) => OauthResponse::ExchangeFailed,
    }
}

/// Process an OAuth callback: verify state, exchange code, store tokens.
async fn process_callback<S, T, R>(
    state_store: &S,
    token_endpoint: &T,
    refresh_store: &R,
    code: &str,
    state: &str,
    redirect_uri: &str,
) -> OauthResponse
where
    S: StateStore,
    S::Error: Display + Debug,
    T: TokenEndpoint,
    T::Error: Display + Debug,
    R: RefreshTokenStore,
    R::Error: Display + Debug,
{
    let state_value = OAuthStateValue::new(state.to_string());
    let auth_code = AuthorizationCode::new(code.to_string());

    let callback_result = verify_provider_callback(state_store, &state_value, auth_code).await;

    let result = match callback_result {
        Ok(r) => r,
        Err(flow::CallbackError::StateNotFound) => return OauthResponse::InvalidState,
        Err(flow::CallbackError::StateExpired) => return OauthResponse::StateExpired,
        Err(flow::CallbackError::ParticipantMismatch) => return OauthResponse::ParticipantMismatch,
        Err(flow::CallbackError::Store(_)) => return OauthResponse::ExchangeFailed,
    };

    let retry_policy = domain::retry::RetryPolicy::new(MAX_EXTERNAL_OPERATION_ATTEMPTS, 10, 100, 0);

    let policy = match retry_policy {
        Ok(p) => p,
        Err(_) => return OauthResponse::ExchangeFailed,
    };

    match complete_exchange(
        token_endpoint,
        refresh_store,
        policy,
        result.participant,
        result.authorization_code.as_str(),
        redirect_uri,
        result.code_verifier.as_str(),
    )
    .await
    {
        Ok(_) => OauthResponse::Connected,
        Err(_) => OauthResponse::ExchangeFailed,
    }
}

/// Route an API Gateway event to the correct OAuth handler path.
async fn route_oauth<S, T, R>(
    event: &ApiGatewayEvent,
    state_store: &S,
    token_endpoint: &T,
    refresh_store: &R,
    client_id: &str,
    redirect_uri: &str,
) -> OauthResponse
where
    S: StateStore,
    S::Error: Display + Debug,
    T: TokenEndpoint,
    T::Error: Display + Debug,
    R: RefreshTokenStore,
    R::Error: Display + Debug,
{
    let ctx = match event.request_context.as_ref() {
        Some(c) => c,
        None => return OauthResponse::NotFound,
    };

    let method = ctx.http.method.as_str();
    let path = ctx.http.path.as_str();
    let segments = path_segments(path);

    // Expected paths: /oauth/authorize/{participant_id} or /oauth/google/callback.
    if segments.len() < 3 {
        return OauthResponse::NotFound;
    }

    let first = segments.first().copied().unwrap_or("");
    let second = segments.get(1).copied().unwrap_or("");
    let third = segments.get(2).copied().unwrap_or("");

    if first != "oauth" {
        return OauthResponse::NotFound;
    }

    if method != "GET" {
        return OauthResponse::MethodNotAllowed;
    }

    match (second, third) {
        ("authorize", participant) => {
            let participant_id = match parse_participant_id(participant) {
                Ok(id) => id,
                Err(()) => return OauthResponse::MissingParams,
            };
            process_authorize(state_store, participant_id, client_id, redirect_uri).await
        }
        ("google", "callback") => {
            let params = match event.query_string_parameters.as_ref() {
                Some(p) => p,
                None => return OauthResponse::MissingParams,
            };

            let code = match params.get("code") {
                Some(c) if !c.is_empty() => c.as_str(),
                _ => return OauthResponse::MissingParams,
            };

            let state = match params.get("state") {
                Some(s) if !s.is_empty() => s.as_str(),
                _ => return OauthResponse::MissingParams,
            };

            process_callback(
                state_store,
                token_endpoint,
                refresh_store,
                code,
                state,
                redirect_uri,
            )
            .await
        }
        _ => OauthResponse::NotFound,
    }
}

struct DynamoDbStateStore {
    client: aws_sdk_dynamodb::Client,
    table_name: String,
    environment: String,
}

impl DynamoDbStateStore {
    fn key(&self, digest: &application::ports::OAuthStateDigest) -> String {
        format!(
            "ENV#{}#OAUTH_STATE#{}",
            self.environment,
            hex::encode(digest.as_bytes())
        )
    }
}

impl StateStore for DynamoDbStateStore {
    type Error = String;

    async fn store(
        &self,
        digest: &application::ports::OAuthStateDigest,
        record: PendingState,
    ) -> Result<(), Self::Error> {
        let created_at = record
            .created_at
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "OAuth state timestamp is invalid".to_owned())?
            .as_secs();
        let expires_at = created_at.saturating_add(flow::STATE_TTL.as_secs());
        self.client
            .put_item()
            .table_name(&self.table_name)
            .item(
                "pk",
                aws_sdk_dynamodb::types::AttributeValue::S(self.key(digest)),
            )
            .item(
                "sk",
                aws_sdk_dynamodb::types::AttributeValue::S("METADATA".to_owned()),
            )
            .item(
                "entity",
                aws_sdk_dynamodb::types::AttributeValue::S("oauth_state".to_owned()),
            )
            .item(
                "participant",
                aws_sdk_dynamodb::types::AttributeValue::N(record.participant.get().to_string()),
            )
            .item(
                "code_verifier",
                aws_sdk_dynamodb::types::AttributeValue::S(
                    record.code_verifier.as_str().to_owned(),
                ),
            )
            .item(
                "created_at",
                aws_sdk_dynamodb::types::AttributeValue::N(created_at.to_string()),
            )
            .item(
                "expires_at",
                aws_sdk_dynamodb::types::AttributeValue::N(expires_at.to_string()),
            )
            .condition_expression("attribute_not_exists(pk)")
            .send()
            .await
            .map_err(|_| "OAuth state storage failed".to_owned())?;
        Ok(())
    }

    async fn consume(
        &self,
        digest: &application::ports::OAuthStateDigest,
    ) -> Result<Option<PendingState>, Self::Error> {
        let output = self
            .client
            .delete_item()
            .table_name(&self.table_name)
            .key(
                "pk",
                aws_sdk_dynamodb::types::AttributeValue::S(self.key(digest)),
            )
            .key(
                "sk",
                aws_sdk_dynamodb::types::AttributeValue::S("METADATA".to_owned()),
            )
            .return_values(aws_sdk_dynamodb::types::ReturnValue::AllOld)
            .send()
            .await
            .map_err(|_| "OAuth state consume failed".to_owned())?;
        let Some(attributes) = output.attributes else {
            return Ok(None);
        };
        let participant = attributes
            .get("participant")
            .and_then(|value| value.as_n().ok())
            .and_then(|value| value.parse::<i64>().ok())
            .and_then(|value| ParticipantId::new(value).ok())
            .ok_or_else(|| "OAuth state participant is invalid".to_owned())?;
        let code_verifier = attributes
            .get("code_verifier")
            .and_then(|value| value.as_s().ok())
            .filter(|value| !value.is_empty())
            .map(|value| oauth::redaction::PkceVerifier::new(value.to_owned()))
            .ok_or_else(|| "OAuth state verifier is invalid".to_owned())?;
        let created_at = attributes
            .get("created_at")
            .and_then(|value| value.as_n().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| std::time::UNIX_EPOCH + std::time::Duration::from_secs(value))
            .ok_or_else(|| "OAuth state timestamp is invalid".to_owned())?;
        Ok(Some(PendingState {
            participant,
            code_verifier,
            created_at,
        }))
    }
}

struct SsmRefreshTokenStore {
    client: aws_sdk_ssm::Client,
    environment: String,
}

impl SsmRefreshTokenStore {
    fn parameter_name(&self, participant: ParticipantId) -> String {
        format!(
            "/novus/{}/google/refresh-tokens/{}",
            self.environment,
            participant.get()
        )
    }
}

impl RefreshTokenStore for SsmRefreshTokenStore {
    type Error = String;

    async fn store(
        &self,
        participant: ParticipantId,
        token: &RefreshToken,
    ) -> Result<(), Self::Error> {
        self.client
            .put_parameter()
            .name(self.parameter_name(participant))
            .r#type(aws_sdk_ssm::types::ParameterType::SecureString)
            .value(token.as_str())
            .overwrite(true)
            .send()
            .await
            .map_err(|_| "refresh token storage failed".to_owned())?;
        Ok(())
    }

    async fn load(&self, participant: ParticipantId) -> Result<Option<RefreshToken>, Self::Error> {
        match self
            .client
            .get_parameter()
            .name(self.parameter_name(participant))
            .with_decryption(true)
            .send()
            .await
        {
            Ok(output) => Ok(output
                .parameter()
                .and_then(|parameter| parameter.value())
                .map(|value| RefreshToken::new(value.to_owned()))),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|service| service.is_parameter_not_found()) =>
            {
                Ok(None)
            }
            Err(_) => Err("refresh token load failed".to_owned()),
        }
    }

    async fn delete(&self, participant: ParticipantId) -> Result<(), Self::Error> {
        match self
            .client
            .delete_parameter()
            .name(self.parameter_name(participant))
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|service| service.is_parameter_not_found()) =>
            {
                Ok(())
            }
            Err(_) => Err("refresh token deletion failed".to_owned()),
        }
    }
}

// ── Lambda entry point ─────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Error> {
    let client_id = std::env::var("OAUTH_CLIENT_ID")
        .map_err(|_| "OAUTH_CLIENT_ID environment variable is not set")?;
    let redirect_uri = std::env::var("OAUTH_REDIRECT_URI")
        .map_err(|_| "OAUTH_REDIRECT_URI environment variable is not set")?;
    let environment = std::env::var("ENVIRONMENT").unwrap_or_else(|_| "development".to_owned());
    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let mut ssm_config = aws_sdk_ssm::config::Builder::from(&sdk_config);
    if let Ok(endpoint) = std::env::var("LOCALSTACK_ENDPOINT") {
        ssm_config = ssm_config.endpoint_url(endpoint);
    }
    let ssm_client = aws_sdk_ssm::Client::from_conf(ssm_config.build());
    let client_secret = load_oauth_client_secret(&ssm_client).await?;
    let mut dynamodb_config = aws_sdk_dynamodb::config::Builder::from(&sdk_config);
    if let Ok(endpoint) = std::env::var("DYNAMODB_ENDPOINT") {
        dynamodb_config = dynamodb_config.endpoint_url(endpoint);
    }
    let application_table = std::env::var("APPLICATION_TABLE")
        .map_err(|_| "APPLICATION_TABLE environment variable is not set")?;

    let state_store = Arc::new(DynamoDbStateStore {
        client: aws_sdk_dynamodb::Client::from_conf(dynamodb_config.build()),
        table_name: application_table,
        environment: environment.clone(),
    });
    let token_endpoint = Arc::new(GoogleTokenEndpoint::new(client_id.clone(), client_secret)?);
    let refresh_store = Arc::new(SsmRefreshTokenStore {
        client: ssm_client,
        environment,
    });

    run(service_fn(move |event: LambdaEvent<ApiGatewayEvent>| {
        let state_store = state_store.clone();
        let token_endpoint = token_endpoint.clone();
        let refresh_store = refresh_store.clone();
        let client_id = client_id.clone();
        let redirect_uri = redirect_uri.clone();
        async move {
            let response = route_oauth(
                &event.payload,
                &*state_store,
                &*token_endpoint,
                &*refresh_store,
                &client_id,
                &redirect_uri,
            )
            .await;
            let response = response.into_api_gateway();
            emit_oauth_metric(response.status_code);
            Ok::<_, Error>(response)
        }
    }))
    .await
}

async fn load_oauth_client_secret(client: &aws_sdk_ssm::Client) -> Result<String, Error> {
    if let Ok(secret) = std::env::var("OAUTH_CLIENT_SECRET")
        && !secret.is_empty()
    {
        return Ok(secret);
    }
    let parameter_name = std::env::var("OAUTH_CLIENT_SECRET_PARAMETER")
        .map_err(|_| "OAUTH_CLIENT_SECRET or OAUTH_CLIENT_SECRET_PARAMETER must be set")?;
    let output = client
        .get_parameter()
        .name(parameter_name)
        .with_decryption(true)
        .send()
        .await
        .map_err(|_| "OAuth client secret parameter could not be loaded")?;
    output
        .parameter()
        .and_then(|parameter| parameter.value())
        .filter(|secret| !secret.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "OAuth client secret parameter is empty".into())
}

fn emit_oauth_metric(status_code: u16) {
    let environment = std::env::var("ENVIRONMENT").unwrap_or_else(|_| "unknown".to_owned());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let error_count = u8::from(status_code >= 400);
    let record = serde_json::json!({
        "_aws": {
            "Timestamp": timestamp,
            "CloudWatchMetrics": [{
                "Namespace": "Novus/Edge",
                "Dimensions": [["Environment"]],
                "Metrics": [
                    {"Name": "OAuthRequestCount", "Unit": "Count"},
                    {"Name": "OAuthErrorCount", "Unit": "Count"}
                ]
            }]
        },
        "Environment": environment,
        "OAuthRequestCount": 1,
        "OAuthErrorCount": error_count,
        "event": "edge_request_outcome",
        "handler": "oauth",
        "outcome": if error_count == 0 { "accepted" } else { "rejected" }
    });
    if let Ok(line) = serde_json::to_string(&record) {
        println!("{line}");
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(clippy::panic)]

    use std::sync::Mutex;

    use super::*;

    // ── Mock stores ────────────────────────────────────────────────────

    struct MockStateStore {
        records: Mutex<HashMap<application::ports::OAuthStateDigest, PendingState>>,
        consume_result: Mutex<Option<Result<Option<PendingState>, String>>>,
    }

    impl MockStateStore {
        fn new() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
                consume_result: Mutex::new(None),
            }
        }

        fn set_consume_result(&self, result: Result<Option<PendingState>, String>) {
            // Ignore poison errors in tests.
            if let Ok(mut r) = self.consume_result.lock() {
                *r = Some(result);
            }
        }

        fn insert(&self, digest: application::ports::OAuthStateDigest, record: PendingState) {
            if let Ok(mut map) = self.records.lock() {
                map.insert(digest, record);
            }
        }
    }

    impl StateStore for MockStateStore {
        type Error = String;

        async fn store(
            &self,
            digest: &application::ports::OAuthStateDigest,
            record: PendingState,
        ) -> Result<(), Self::Error> {
            if let Ok(mut map) = self.records.lock() {
                map.insert(*digest, record);
            }
            Ok(())
        }

        async fn consume(
            &self,
            _digest: &application::ports::OAuthStateDigest,
        ) -> Result<Option<PendingState>, Self::Error> {
            match self.consume_result.lock() {
                Ok(guard) => match guard.as_ref() {
                    Some(Ok(opt)) => Ok(opt.clone()),
                    Some(Err(e)) => Err(e.clone()),
                    None => Ok(None),
                },
                Err(_) => Ok(None),
            }
        }
    }

    struct MockTokenEndpoint {
        exchange_result: Mutex<Option<Result<TokenResponse, String>>>,
    }

    impl MockTokenEndpoint {
        fn new() -> Self {
            Self {
                exchange_result: Mutex::new(None),
            }
        }

        fn set_exchange_result(&self, result: Result<TokenResponse, String>) {
            if let Ok(mut r) = self.exchange_result.lock() {
                *r = Some(result);
            }
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
            match self.exchange_result.lock() {
                Ok(guard) => match guard.as_ref() {
                    Some(Ok(resp)) => Ok(TokenResponse {
                        access_token: AccessToken::new(resp.access_token.as_str().to_string()),
                        refresh_token: resp
                            .refresh_token
                            .as_ref()
                            .map(|rt| RefreshToken::new(rt.as_str().to_string())),
                        expires_in: resp.expires_in,
                        token_type: resp.token_type.clone(),
                    }),
                    Some(Err(e)) => Err(e.clone()),
                    None => Err("mock not configured".into()),
                },
                Err(_) => Err("lock poisoned".into()),
            }
        }

        async fn refresh(&self, _refresh_token: &str) -> Result<TokenResponse, Self::Error> {
            Err("mock refresh not implemented".into())
        }

        async fn revoke(&self, _token: &str) -> Result<(), Self::Error> {
            Err("mock revoke not implemented".into())
        }
    }

    struct MockRefreshTokenStore {
        stored: Mutex<Vec<(ParticipantId, RefreshToken)>>,
    }

    impl MockRefreshTokenStore {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
            }
        }
    }

    impl RefreshTokenStore for MockRefreshTokenStore {
        type Error = String;

        async fn store(
            &self,
            participant: ParticipantId,
            token: &RefreshToken,
        ) -> Result<(), Self::Error> {
            if let Ok(mut v) = self.stored.lock() {
                v.push((participant, token.clone()));
            }
            Ok(())
        }

        async fn load(
            &self,
            _participant: ParticipantId,
        ) -> Result<Option<RefreshToken>, Self::Error> {
            Ok(None)
        }

        async fn delete(&self, _participant: ParticipantId) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────

    fn participant() -> ParticipantId {
        ParticipantId::new(42).expect("valid participant id")
    }

    fn make_authorize_event(participant_id: i64) -> ApiGatewayEvent {
        ApiGatewayEvent {
            query_string_parameters: None,
            request_context: Some(RequestContext {
                http: HttpContext {
                    method: "GET".into(),
                    path: format!("/oauth/authorize/{participant_id}"),
                },
            }),
        }
    }

    fn make_callback_event(code: &str, state: &str, _participant_id: i64) -> ApiGatewayEvent {
        let mut params = HashMap::new();
        params.insert("code".into(), code.to_string());
        params.insert("state".into(), state.to_string());

        ApiGatewayEvent {
            query_string_parameters: Some(params),
            request_context: Some(RequestContext {
                http: HttpContext {
                    method: "GET".into(),
                    path: "/oauth/google/callback".into(),
                },
            }),
        }
    }

    fn make_pending_state(participant_id: ParticipantId) -> PendingState {
        let pkce = generate_pkce().expect("pkce generation");
        PendingState {
            participant: participant_id,
            code_verifier: pkce.verifier,
            created_at: SystemTime::now(),
        }
    }

    fn make_token_response() -> TokenResponse {
        TokenResponse {
            access_token: AccessToken::new("ya29.fake-access-token".into()),
            refresh_token: Some(RefreshToken::new("1//fake-refresh-token".into())),
            expires_in: Some(3600),
            token_type: "Bearer".into(),
        }
    }

    // ── Authorize path tests ───────────────────────────────────────────

    #[test]
    fn authorize_returns_url() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let store = MockStateStore::new();
        let event = make_authorize_event(42);

        let stub_ep = MockTokenEndpoint::new();
        let stub_refresh = MockRefreshTokenStore::new();

        let response = rt.block_on(route_oauth(
            &event,
            &store,
            &stub_ep,
            &stub_refresh,
            "test-client-id",
            "https://example.com/callback",
        ));

        match response {
            OauthResponse::AuthorizeUrl { url } => {
                assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth"));
                assert!(url.contains("client_id=test-client-id"));
                assert!(url.contains("code_challenge_method=S256"));
            }
            other => panic!("expected AuthorizeUrl, got {other:?}"),
        }
    }

    #[test]
    fn authorize_api_gateway_response_has_200() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let store = MockStateStore::new();
        let event = make_authorize_event(42);

        let stub_ep = MockTokenEndpoint::new();
        let stub_refresh = MockRefreshTokenStore::new();

        let response = rt.block_on(route_oauth(
            &event,
            &store,
            &stub_ep,
            &stub_refresh,
            "test-client-id",
            "https://example.com/callback",
        ));

        let agw = response.into_api_gateway();
        assert_eq!(agw.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(&agw.body).expect("valid json body");
        assert!(
            body.get("url")
                .and_then(|v| v.as_str())
                .is_some_and(|u| u.starts_with("https://accounts.google.com/o/oauth2/v2/auth"))
        );
    }

    // ── Callback path tests ────────────────────────────────────────────

    #[test]
    fn callback_success_returns_connected() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        let pending = make_pending_state(participant());
        let state_value = OAuthStateValue::new("fake-state-value".into());
        let digest = state_digest(&state_value);

        state_store.insert(digest, pending);
        state_store.set_consume_result(Ok(Some(make_pending_state(participant()))));
        token_endpoint.set_exchange_result(Ok(make_token_response()));

        let event = make_callback_event("fake-auth-code", "fake-state-value", 42);

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        match response {
            OauthResponse::Connected => {}
            other => panic!("expected Connected, got {other:?}"),
        }
    }

    #[test]
    fn callback_connected_api_gateway_response_has_200() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        let pending = make_pending_state(participant());
        let state_value = OAuthStateValue::new("fake-state-value".into());
        let digest = state_digest(&state_value);

        state_store.insert(digest, pending);
        state_store.set_consume_result(Ok(Some(make_pending_state(participant()))));
        token_endpoint.set_exchange_result(Ok(make_token_response()));

        let event = make_callback_event("fake-auth-code", "fake-state-value", 42);

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        let agw = response.into_api_gateway();
        assert_eq!(agw.status_code, 200);
        let body: serde_json::Value = serde_json::from_str(&agw.body).expect("valid json body");
        assert_eq!(
            body.get("status").and_then(|v| v.as_str()),
            Some("connected")
        );
    }

    #[test]
    fn callback_state_not_found_returns_invalid_state() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        // consume returns None → StateNotFound
        state_store.set_consume_result(Ok(None));

        let event = make_callback_event("fake-auth-code", "fake-state-value", 42);

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        assert!(matches!(response, OauthResponse::InvalidState));
    }

    #[test]
    fn callback_invalid_state_api_gateway_response_has_400() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        state_store.set_consume_result(Ok(None));

        let event = make_callback_event("fake-auth-code", "fake-state-value", 42);

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        let agw = response.into_api_gateway();
        assert_eq!(agw.status_code, 400);
    }

    #[test]
    fn callback_missing_code_returns_missing_params() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        let mut params = HashMap::new();
        params.insert("state".into(), "fake-state-value".into());
        let event = ApiGatewayEvent {
            query_string_parameters: Some(params),
            request_context: Some(RequestContext {
                http: HttpContext {
                    method: "GET".into(),
                    path: "/oauth/google/callback".into(),
                },
            }),
        };

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        assert!(matches!(response, OauthResponse::MissingParams));
    }

    #[test]
    fn callback_missing_params_api_gateway_response_has_400() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        let event = make_callback_event("", "fake-state-value", 42);

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        let agw = response.into_api_gateway();
        assert_eq!(agw.status_code, 400);
    }

    // ── Routing tests ──────────────────────────────────────────────────

    #[test]
    fn unknown_path_returns_not_found() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        let event = ApiGatewayEvent {
            query_string_parameters: None,
            request_context: Some(RequestContext {
                http: HttpContext {
                    method: "GET".into(),
                    path: "/unknown/path".into(),
                },
            }),
        };

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        assert!(matches!(response, OauthResponse::NotFound));
    }

    #[test]
    fn post_method_returns_method_not_allowed() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        let event = ApiGatewayEvent {
            query_string_parameters: None,
            request_context: Some(RequestContext {
                http: HttpContext {
                    method: "POST".into(),
                    path: "/oauth/authorize/42".into(),
                },
            }),
        };

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        assert!(matches!(response, OauthResponse::MethodNotAllowed));
    }

    // ── No token values in response bodies ─────────────────────────────

    #[test]
    fn callback_success_body_contains_no_token_values() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        let pending = make_pending_state(participant());
        let state_value = OAuthStateValue::new("fake-state-value".into());
        let digest = state_digest(&state_value);

        state_store.insert(digest, pending);
        state_store.set_consume_result(Ok(Some(make_pending_state(participant()))));
        token_endpoint.set_exchange_result(Ok(make_token_response()));

        let event = make_callback_event("fake-auth-code", "fake-state-value", 42);

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        let agw = response.into_api_gateway();
        let body_str = &agw.body;

        assert!(!body_str.contains("ya29"));
        assert!(!body_str.contains("access_token"));
        assert!(!body_str.contains("refresh_token"));
        assert!(!body_str.contains("fake-auth-code"));
        assert!(!body_str.contains("fake-state-value"));
        assert!(body_str.contains("connected"));
    }

    #[test]
    fn error_response_body_contains_no_token_values() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let state_store = MockStateStore::new();
        let token_endpoint = MockTokenEndpoint::new();
        let refresh_store = MockRefreshTokenStore::new();

        state_store.set_consume_result(Err("store error with token ya29.abc".into()));

        let event = make_callback_event("fake-auth-code", "fake-state-value", 42);

        let response = rt.block_on(route_oauth(
            &event,
            &state_store,
            &token_endpoint,
            &refresh_store,
            "test-client-id",
            "https://example.com/callback",
        ));

        let agw = response.into_api_gateway();
        let body_str = &agw.body;

        assert!(!body_str.contains("ya29"));
        assert!(!body_str.contains("fake-auth-code"));
        assert!(body_str.contains("exchange_failed"));
    }
}
