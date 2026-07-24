use domain::identity::ParticipantId;
use google::auth::{GoogleAccessToken, OwnerTokenSource};
use oauth::google_endpoint::GoogleTokenEndpoint;
use oauth::redaction::RefreshToken;
use oauth::tokens::TokenEndpoint;

pub struct SsmOwnerTokenSource {
    ssm: aws_sdk_ssm::Client,
    endpoint: GoogleTokenEndpoint,
    environment: String,
}

impl SsmOwnerTokenSource {
    pub async fn from_environment() -> Result<Self, String> {
        let client_id = std::env::var("OAUTH_CLIENT_ID")
            .map_err(|_| "OAuth client ID is not configured".to_owned())?;
        let environment = std::env::var("ENVIRONMENT").unwrap_or_else(|_| "development".to_owned());
        let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let mut ssm_config = aws_sdk_ssm::config::Builder::from(&sdk_config);
        if let Ok(endpoint) = std::env::var("LOCALSTACK_ENDPOINT") {
            ssm_config = ssm_config.endpoint_url(endpoint);
        }
        let ssm = aws_sdk_ssm::Client::from_conf(ssm_config.build());
        let client_secret = load_client_secret(&ssm).await?;
        let endpoint = GoogleTokenEndpoint::new(client_id, client_secret)
            .map_err(|_| "Google token endpoint configuration failed".to_owned())?;
        Ok(Self {
            ssm,
            endpoint,
            environment,
        })
    }

    fn refresh_token_parameter(&self, owner: ParticipantId) -> String {
        format!(
            "/novus/{}/google/refresh-tokens/{}",
            self.environment,
            owner.get()
        )
    }
}

impl OwnerTokenSource for SsmOwnerTokenSource {
    type Error = String;

    async fn access_token(&self, owner: ParticipantId) -> Result<GoogleAccessToken, Self::Error> {
        let output = self
            .ssm
            .get_parameter()
            .name(self.refresh_token_parameter(owner))
            .with_decryption(true)
            .send()
            .await
            .map_err(|_| "Google refresh token is unavailable".to_owned())?;
        let refresh_token = output
            .parameter()
            .and_then(|parameter| parameter.value())
            .filter(|value| !value.is_empty())
            .map(|value| RefreshToken::new(value.to_owned()))
            .ok_or_else(|| "Google refresh token is unavailable".to_owned())?;
        let response = self
            .endpoint
            .refresh(refresh_token.as_str())
            .await
            .map_err(|_| "Google access token refresh failed".to_owned())?;
        Ok(GoogleAccessToken::new(
            response.access_token.as_str().to_owned(),
        ))
    }
}

async fn load_client_secret(client: &aws_sdk_ssm::Client) -> Result<String, String> {
    if let Ok(secret) = std::env::var("OAUTH_CLIENT_SECRET")
        && !secret.is_empty()
    {
        return Ok(secret);
    }
    let parameter_name = std::env::var("OAUTH_CLIENT_SECRET_PARAMETER")
        .map_err(|_| "OAuth client secret is not configured".to_owned())?;
    let output = client
        .get_parameter()
        .name(parameter_name)
        .with_decryption(true)
        .send()
        .await
        .map_err(|_| "OAuth client secret is unavailable".to_owned())?;
    output
        .parameter()
        .and_then(|parameter| parameter.value())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "OAuth client secret is unavailable".to_owned())
}
