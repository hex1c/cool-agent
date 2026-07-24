use std::fmt::{Debug, Display, Formatter};

use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use zeroize::Zeroize;

use crate::redaction::{AccessToken, RefreshToken};
use crate::tokens::{TokenEndpoint, TokenResponse};

const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";

pub struct GoogleTokenEndpoint {
    client: Client,
    client_id: String,
    client_secret: String,
    token_url: Url,
    revoke_url: Url,
}

impl GoogleTokenEndpoint {
    pub fn new(client_id: String, client_secret: String) -> Result<Self, GoogleTokenError> {
        Self::with_endpoints(
            client_id,
            client_secret,
            GOOGLE_TOKEN_URL,
            GOOGLE_REVOKE_URL,
        )
    }

    pub fn with_endpoints(
        client_id: String,
        client_secret: String,
        token_url: &str,
        revoke_url: &str,
    ) -> Result<Self, GoogleTokenError> {
        if client_id.trim().is_empty() || client_secret.is_empty() {
            return Err(GoogleTokenError::Configuration);
        }
        let token_url = parse_endpoint(token_url)?;
        let revoke_url = parse_endpoint(revoke_url)?;
        Ok(Self {
            client: Client::new(),
            client_id,
            client_secret,
            token_url,
            revoke_url,
        })
    }

    async fn token_request(
        &self,
        form: &[(&str, &str)],
    ) -> Result<TokenResponse, GoogleTokenError> {
        let response = self
            .client
            .post(self.token_url.clone())
            .form(form)
            .send()
            .await
            .map_err(|_| GoogleTokenError::Transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(classify_status(status));
        }
        let payload: GoogleTokenPayload = response
            .json()
            .await
            .map_err(|_| GoogleTokenError::InvalidResponse)?;
        payload.try_into()
    }
}

impl Debug for GoogleTokenEndpoint {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleTokenEndpoint")
            .field("client", &"[REDACTED]")
            .field("client_id", &"[REDACTED]")
            .field("client_secret", &"[REDACTED]")
            .field("token_url", &self.token_url)
            .field("revoke_url", &self.revoke_url)
            .finish()
    }
}

impl Drop for GoogleTokenEndpoint {
    fn drop(&mut self) {
        self.client_id.zeroize();
        self.client_secret.zeroize();
    }
}

impl TokenEndpoint for GoogleTokenEndpoint {
    type Error = GoogleTokenError;

    async fn exchange(
        &self,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenResponse, Self::Error> {
        self.token_request(&[
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
            ("code", code),
            ("code_verifier", code_verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
        ])
        .await
    }

    async fn refresh(&self, refresh_token: &str) -> Result<TokenResponse, Self::Error> {
        self.token_request(&[
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .await
    }

    async fn revoke(&self, token: &str) -> Result<(), Self::Error> {
        let response = self
            .client
            .post(self.revoke_url.clone())
            .form(&[("token", token)])
            .send()
            .await
            .map_err(|_| GoogleTokenError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(classify_status(response.status()))
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GoogleTokenError {
    #[error("Google OAuth endpoint configuration is invalid")]
    Configuration,
    #[error("Google OAuth endpoint transport failed")]
    Transport,
    #[error("Google OAuth endpoint rejected the request")]
    Rejected,
    #[error("Google OAuth endpoint is temporarily unavailable")]
    Unavailable,
    #[error("Google OAuth endpoint returned an invalid response")]
    InvalidResponse,
}

#[derive(Deserialize)]
struct GoogleTokenPayload {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    token_type: Option<String>,
}

impl TryFrom<GoogleTokenPayload> for TokenResponse {
    type Error = GoogleTokenError;

    fn try_from(mut payload: GoogleTokenPayload) -> Result<Self, Self::Error> {
        let access_token = payload
            .access_token
            .take()
            .filter(|value| !value.is_empty())
            .ok_or(GoogleTokenError::InvalidResponse)?;
        let token_type = payload
            .token_type
            .take()
            .filter(|value| !value.is_empty())
            .ok_or(GoogleTokenError::InvalidResponse)?;
        Ok(Self {
            access_token: AccessToken::new(access_token),
            refresh_token: payload.refresh_token.take().map(RefreshToken::new),
            expires_in: payload.expires_in,
            token_type,
        })
    }
}

fn parse_endpoint(value: &str) -> Result<Url, GoogleTokenError> {
    let url = Url::parse(value).map_err(|_| GoogleTokenError::Configuration)?;
    let is_loopback_http = url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| host == "localhost" || host == "127.0.0.1");
    if url.scheme() != "https" && !is_loopback_http {
        return Err(GoogleTokenError::Configuration);
    }
    Ok(url)
}

fn classify_status(status: StatusCode) -> GoogleTokenError {
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        GoogleTokenError::Unavailable
    } else {
        GoogleTokenError::Rejected
    }
}

impl Display for GoogleTokenEndpoint {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GoogleTokenEndpoint([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn response_requires_access_token_and_type() {
        let missing = GoogleTokenPayload {
            access_token: None,
            refresh_token: None,
            expires_in: None,
            token_type: Some("Bearer".to_owned()),
        };
        assert!(matches!(
            TokenResponse::try_from(missing),
            Err(GoogleTokenError::InvalidResponse)
        ));
    }

    #[test]
    fn endpoint_debug_and_display_are_redacted() {
        let endpoint =
            GoogleTokenEndpoint::new("client".to_owned(), "super-sensitive-value".to_owned())
                .expect("valid endpoint");
        assert!(!format!("{endpoint:?}").contains("super-sensitive-value"));
        assert!(!endpoint.to_string().contains("super-sensitive-value"));
    }

    #[test]
    fn non_https_provider_endpoint_is_rejected() {
        assert!(matches!(
            GoogleTokenEndpoint::with_endpoints(
                "client".to_owned(),
                "secret".to_owned(),
                "http://provider.example/token",
                GOOGLE_REVOKE_URL,
            ),
            Err(GoogleTokenError::Configuration)
        ));
    }
}
