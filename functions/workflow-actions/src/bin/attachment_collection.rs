#![allow(dead_code)]

use std::sync::Arc;

use application::attachments::AttachmentCollectionService;
use application::config::AttachmentConfig;
use application::repositories::WorkflowRepository;
use domain::WorkflowTimestamp;
use domain::identity::{ChatId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId};
use domain::workflow::Workflow;
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde::Deserialize;
use serde_json::Value;
use storage::dynamodb::DynamoDbStore;
use storage::s3::S3ObjectStore;
use workflow_actions_functions::EVENT_SCHEMA_VERSION;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionEvent {
    #[serde(rename = "schemaVersion")]
    schema_version: String,
    #[serde(rename = "workflowId")]
    workflow_id: String,
    #[serde(rename = "updateId")]
    update_id: i64,
    #[serde(rename = "sourceMessageId")]
    source_message_id: i64,
    #[serde(rename = "chatId")]
    chat_id: i64,
    #[serde(rename = "messageThreadId")]
    message_thread_id: i64,
    #[serde(rename = "actorId")]
    actor_id: i64,
    instruction: String,
    #[serde(default)]
    attachments: Vec<String>,
}

struct CollectionContext {
    service: AttachmentCollectionService<S3ObjectStore, DynamoDbStore>,
    repository: DynamoDbStore,
    config: AttachmentConfig,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let ctx = Arc::new(build_context().await?);
    run(service_fn(move |event| {
        let ctx = Arc::clone(&ctx);
        async move { handle(event, ctx.as_ref()).await }
    }))
    .await
}

async fn build_context() -> Result<CollectionContext, Error> {
    let environment = required_env("ENVIRONMENT")?;
    let application_table = required_env("APPLICATION_TABLE")?;
    let artifact_bucket = required_env("ARTIFACT_BUCKET")?;
    let page_token_key = decode_page_token_key(&required_env("PAGE_TOKEN_SIGNING_KEY_HEX")?)?;

    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;

    let mut db_config = aws_sdk_dynamodb::config::Builder::from(&sdk_config);
    if let Ok(endpoint) = std::env::var("DYNAMODB_ENDPOINT") {
        db_config = db_config.endpoint_url(endpoint);
    }
    let repository = DynamoDbStore::new(
        aws_sdk_dynamodb::Client::from_conf(db_config.build()),
        &application_table,
        &environment,
        page_token_key,
    )?;

    let mut s3_config = aws_sdk_s3::config::Builder::from(&sdk_config);
    if let Ok(endpoint) = std::env::var("S3_ENDPOINT") {
        s3_config = s3_config.endpoint_url(endpoint).force_path_style(true);
    }
    let object_store = S3ObjectStore::new(
        aws_sdk_s3::Client::from_conf(s3_config.build()),
        &artifact_bucket,
        20_971_520,
        20_971_520,
        65_536,
        604_800,
    )?;

    let config = AttachmentConfig {
        max_count: 10,
        max_bytes: 20_971_520,
        allowed_mime_types: vec![
            "image/jpeg".to_owned(),
            "image/png".to_owned(),
            "application/pdf".to_owned(),
            "text/csv".to_owned(),
        ],
    };

    Ok(CollectionContext {
        service: AttachmentCollectionService::new(object_store, repository.clone(), config.clone()),
        repository,
        config,
    })
}

async fn handle(event: LambdaEvent<Value>, ctx: &CollectionContext) -> Result<Value, Error> {
    let parsed: CollectionEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(error_value("invalid_event")),
    };
    if parsed.schema_version != EVENT_SCHEMA_VERSION {
        return Ok(error_value("schema_version_mismatch"));
    }

    // Load the workflow from DynamoDB. If it doesn't exist yet, create it
    // from the event fields (the webhook may have created it already).
    let workflow_id = WorkflowId::new(parsed.workflow_id.clone())
        .map_err(|_| Error::from("invalid workflow id"))?;
    let _workflow = match ctx.repository.load(&workflow_id).await {
        Ok(Some(wf)) => wf,
        Ok(None) => {
            // Workflow not yet persisted — construct from the event.
            let topic = TopicSessionId::new(
                ChatId::new(parsed.chat_id),
                MessageThreadId::new(parsed.message_thread_id)
                    .map_err(|_| Error::from("invalid thread id"))?,
            );
            let owner =
                ParticipantId::new(parsed.actor_id).map_err(|_| Error::from("invalid actor id"))?;
            Workflow::new(
                workflow_id,
                topic,
                owner,
                WorkflowTimestamp::from_unix_seconds(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs()),
                ),
            )
        }
        Err(_) => return Ok(error_value("repository_unavailable")),
    };

    // If there are no attachments in the initial mention, signal done
    // immediately so the SFN proceeds to extraction. When attachments exist,
    // return a 30-minute collection window for follow-up uploads.
    if parsed.attachments.is_empty() {
        return Ok(serde_json::json!({
            "status": "done",
            "waitSeconds": 0,
            "workflowId": parsed.workflow_id,
            "collectedCount": 0,
        }));
    }

    let wait_seconds = 1800_u64;
    Ok(serde_json::json!({
        "status": "waiting",
        "waitSeconds": wait_seconds,
        "workflowId": parsed.workflow_id,
        "collectedCount": 0,
    }))
}

fn required_env(name: &str) -> Result<String, Error> {
    std::env::var(name).map_err(|_| format!("{name} environment variable is not set").into())
}

fn decode_page_token_key(value: &str) -> Result<[u8; 32], Error> {
    let decoded = hex::decode(value).map_err(|_| "PAGE_TOKEN_SIGNING_KEY_HEX must be hex")?;
    decoded
        .try_into()
        .map_err(|_| "PAGE_TOKEN_SIGNING_KEY_HEX must encode exactly 32 bytes".into())
}

fn error_value(code: &'static str) -> Value {
    serde_json::json!({ "error": code })
}
