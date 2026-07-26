use std::sync::Arc;

use application::external_operation::ExecutionOutcome;
use application::ports::PresignedObjectLink;
use application::repositories::ConditionalWriteOutcome;
use domain::identity::ChatId;
use domain::retry::AttemptNumber;
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde_json::Value;
use telegram::{ReqwestTelegramBot, TelegramBot};
use workflow_actions_functions::{
    DeliveryError, DeliveryEvent, DeliveryRequest, DeliveryResult, DeliveryRunner, process_delivery,
};

struct SimpleDeliveryRunner {
    s3_client: aws_sdk_s3::Client,
    bucket: String,
    bot: ReqwestTelegramBot,
}

impl DeliveryRunner for SimpleDeliveryRunner {
    async fn run(&self, request: DeliveryRequest) -> Result<DeliveryResult, DeliveryError> {
        let pdf_bytes = &request.pdf_bytes;
        let storage_key = request.artifact.storage_key.as_str().to_owned();

        // Upload PDF to S3.
        let s3_outcome = match self
            .s3_client
            .put_object()
            .bucket(&self.bucket)
            .key(&storage_key)
            .body(aws_sdk_s3::primitives::ByteStream::from(pdf_bytes.clone()))
            .content_type("application/pdf")
            .send()
            .await
        {
            Ok(_) => ConditionalWriteOutcome::Committed,
            Err(_) => ConditionalWriteOutcome::Conflict,
        };

        // Send topic message via Telegram Bot API.
        let chat_id = ChatId::new(request.topic.chat_id().get());
        let telegram_outcome = match self.bot.send_message(chat_id, &request.topic_message).await {
            Ok(()) => ExecutionOutcome::Accepted {
                attempt: AttemptNumber::new(1).map_err(|_| DeliveryError::Execution {
                    operation: "telegram",
                })?,
                resource_id: None,
            },
            Err(_) => ExecutionOutcome::Exhausted {
                completed_attempts: Vec::new(),
            },
        };

        let presigned_link =
            PresignedObjectLink::new(format!("https://{}/{}.pdf", self.bucket, storage_key))
                .map_err(|_| DeliveryError::LinkSigning)?;

        Ok(DeliveryResult {
            s3_outcome,
            drive_resource_id: None,
            presigned_link,
            telegram_outcome,
        })
    }
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

async fn build_runner() -> Result<SimpleDeliveryRunner, Error> {
    let artifact_bucket = required_env("ARTIFACT_BUCKET")?;
    let telegram_token = required_env("TELEGRAM_BOT_TOKEN")?;
    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let mut s3_config = aws_sdk_s3::config::Builder::from(&sdk_config);
    if let Ok(endpoint) = std::env::var("S3_ENDPOINT") {
        s3_config = s3_config.endpoint_url(endpoint).force_path_style(true);
    }
    let bot = ReqwestTelegramBot::new(&telegram_token)?;
    Ok(SimpleDeliveryRunner {
        s3_client: aws_sdk_s3::Client::from_conf(s3_config.build()),
        bucket: artifact_bucket,
        bot,
    })
}

async fn handle(event: LambdaEvent<Value>, runner: &SimpleDeliveryRunner) -> Result<Value, Error> {
    let parsed: DeliveryEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(error_value("invalid_event")),
    };
    match process_delivery(parsed, runner).await {
        Ok(result) => serde_json::to_value(result).map_err(Into::into),
        Err(_) => Ok(error_value("delivery_failed")),
    }
}

fn required_env(name: &str) -> Result<String, Error> {
    std::env::var(name).map_err(|_| format!("{name} environment variable is not set").into())
}

fn error_value(code: &'static str) -> Value {
    serde_json::json!({ "error": code })
}
