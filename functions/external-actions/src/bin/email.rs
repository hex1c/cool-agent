use std::sync::Arc;

use application::external_operation::ProviderOutcome;
use email::lettre_client::LettreSmtpClient;
use email::message::{ConfirmedEmailProof, EmailMessageRequest, SendError};
use email::smtp::{HostingerSmtpService, SmtpCredentials};
use external_actions_functions::{EmailActionEvent, EmailActionRunner, process_email_action};
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde::Deserialize;
use serde_json::Value;

struct SmtpRunner {
    service: HostingerSmtpService<LettreSmtpClient>,
    credentials: SmtpCredentials,
}

impl EmailActionRunner for SmtpRunner {
    async fn run(
        &self,
        proof: &ConfirmedEmailProof,
        request: &EmailMessageRequest,
    ) -> Result<ProviderOutcome, SendError> {
        self.service.send(&self.credentials, proof, request).await
    }
}

#[derive(Deserialize)]
struct StoredSmtpCredentials {
    username: String,
    password: String,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let runner = Arc::new(build_runner().await?);
    run(service_fn(move |event| {
        let runner = Arc::clone(&runner);
        async move { handle(event, runner.as_ref()).await }
    }))
    .await
}

async fn handle(event: LambdaEvent<Value>, runner: &SmtpRunner) -> Result<Value, Error> {
    let parsed: EmailActionEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(error_value("invalid_event")),
    };
    match process_email_action(parsed, runner).await {
        Ok(result) => serde_json::to_value(result).map_err(Into::into),
        Err(_) => Ok(error_value("email_action_failed")),
    }
}

async fn build_runner() -> Result<SmtpRunner, Error> {
    let host =
        std::env::var("HOSTINGER_SMTP_HOST").unwrap_or_else(|_| "smtp.hostinger.com".to_owned());
    if host != "smtp.hostinger.com" {
        return Err("HOSTINGER_SMTP_HOST must be smtp.hostinger.com".into());
    }
    let port = std::env::var("HOSTINGER_SMTP_PORT")
        .unwrap_or_else(|_| "587".to_owned())
        .parse::<u16>()
        .map_err(|_| "HOSTINGER_SMTP_PORT is invalid")?;
    let security =
        std::env::var("HOSTINGER_SMTP_SECURITY").unwrap_or_else(|_| "starttls".to_owned());
    let implicit_tls = match (security.as_str(), port) {
        ("starttls", 587) => false,
        ("ssl", 465) => true,
        _ => return Err("SMTP security must be starttls/587 or ssl/465".into()),
    };
    let credentials = load_credentials().await?;
    let client = LettreSmtpClient::new(host, port, implicit_tls)?;
    Ok(SmtpRunner {
        service: HostingerSmtpService::new(client),
        credentials: SmtpCredentials::new(credentials.username, credentials.password),
    })
}

async fn load_credentials() -> Result<StoredSmtpCredentials, Error> {
    if let (Ok(username), Ok(password)) = (
        std::env::var("HOSTINGER_SMTP_USERNAME"),
        std::env::var("HOSTINGER_SMTP_PASSWORD"),
    ) && !username.is_empty()
        && !password.is_empty()
    {
        return Ok(StoredSmtpCredentials { username, password });
    }
    let parameter_name = std::env::var("SMTP_CREDENTIALS_PARAMETER")
        .map_err(|_| "SMTP credentials are not configured")?;
    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let output = aws_sdk_ssm::Client::new(&sdk_config)
        .get_parameter()
        .name(parameter_name)
        .with_decryption(true)
        .send()
        .await
        .map_err(|_| "SMTP credentials parameter could not be loaded")?;
    let value = output
        .parameter()
        .and_then(|parameter| parameter.value())
        .ok_or("SMTP credentials parameter is empty")?;
    serde_json::from_str(value).map_err(|_| "SMTP credentials parameter is invalid JSON".into())
}

fn error_value(code: &'static str) -> Value {
    serde_json::json!({ "error": code })
}
