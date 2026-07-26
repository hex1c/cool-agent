#![allow(dead_code)]

use std::sync::Arc;

use application::repositories::WorkflowRepository;
use aws_sdk_dynamodb::types::AttributeValue;
use domain::identity::{ChatId, WorkflowId};
use lambda_runtime::{Error, LambdaEvent, run, service_fn};
use serde::Deserialize;
use serde_json::Value;
use storage::dynamodb::DynamoDbStore;
use telegram::{ReqwestTelegramBot, TelegramBot};
use workflow_actions_functions::{
    WorkflowActionError, WorkflowActionEvent, workflow::process_workflow_action,
};

/// Event for the `RequestConfirmation` SFN state (waitForTaskToken).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfirmationRequestEvent {
    #[serde(rename = "schemaVersion")]
    schema_version: String,
    #[serde(rename = "workflowId")]
    workflow_id: String,
    #[serde(rename = "taskToken", default)]
    task_token: Option<String>,
    #[serde(default)]
    preview: serde_json::Value,
    #[serde(rename = "chatId", default)]
    chat_id: Option<i64>,
}

struct WorkflowContext {
    repository: DynamoDbStore,
    bot: Option<ReqwestTelegramBot>,
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

async fn build_context() -> Result<WorkflowContext, Error> {
    let environment = required_env("ENVIRONMENT")?;
    let application_table = required_env("APPLICATION_TABLE")?;
    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let page_token_key = decode_page_token_key(&required_env("PAGE_TOKEN_SIGNING_KEY_HEX")?)?;

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

    let bot = std::env::var("TELEGRAM_BOT_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
        .map(|token| ReqwestTelegramBot::new(&token))
        .transpose()?;

    Ok(WorkflowContext { repository, bot })
}

async fn handle(event: LambdaEvent<Value>, ctx: &WorkflowContext) -> Result<Value, Error> {
    let payload = &event.payload;

    // Check if this is a confirmation request event (has taskToken field).
    if payload.get("taskToken").is_some() {
        return handle_confirmation_request(payload, ctx).await;
    }

    // Otherwise, handle as a workflow action event (StartDirectPdfGeneration).
    let parsed: WorkflowActionEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(to_error(&WorkflowActionError::InvalidEvent)),
    };

    match process_workflow_action(&parsed) {
        Ok(result) => {
            // Persist the resulting workflow to DynamoDB.
            match ctx.repository.load(result.resulting_workflow.id()).await {
                Ok(Some(_)) => {
                    // Workflow exists — the transition is already persisted
                    // by the SFN caller. Just return the result.
                }
                Ok(None) => {
                    eprintln!("workflow not found in DynamoDB for transition persist");
                }
                Err(e) => {
                    eprintln!("workflow load for transition persist failed: {e}");
                }
            }
            Ok(serde_json::to_value(&result)
                .unwrap_or_else(|_| to_error(&WorkflowActionError::InvalidEvent)))
        }
        Err(err) => Ok(to_error(&err)),
    }
}

/// Handle a `RequestConfirmation` event from the SFN waitForTaskToken pattern.
/// Store the task token in DynamoDB so the webhook can resume the SFN when
/// the user sends /confirm.
async fn handle_confirmation_request(
    payload: &Value,
    ctx: &WorkflowContext,
) -> Result<Value, Error> {
    let parsed: ConfirmationRequestEvent = match serde_json::from_value(payload.clone()) {
        Ok(event) => event,
        Err(_) => return Ok(serde_json::json!({ "error": "invalid_event" })),
    };

    // Store the task token in DynamoDB if present.
    if let Some(token) = &parsed.task_token {
        let workflow_id =
            WorkflowId::new(&parsed.workflow_id).map_err(|_| Error::from("invalid workflow id"))?;
        store_task_token(&ctx.repository, &workflow_id, token).await;
    }

    // Send a preview message to the Telegram topic if bot is configured.
    if let (Some(bot), Some(chat_id)) = (&ctx.bot, parsed.chat_id) {
        let preview_text = format!(
            "Preview ready. Reply /confirm to proceed or /correct to adjust.\n\n{}",
            serde_json::to_string_pretty(&parsed.preview).unwrap_or_default()
        );
        let _ = bot.send_message(ChatId::new(chat_id), &preview_text).await;
    }

    Ok(serde_json::json!({
        "status": "waiting_for_confirmation",
        "workflowId": parsed.workflow_id,
    }))
}

/// Store a Step Functions task token in DynamoDB for a workflow.
async fn store_task_token(store: &DynamoDbStore, workflow_id: &WorkflowId, token: &str) {
    let Ok((pk, sk)) = storage::keys::task_token(workflow_id) else {
        return;
    };
    let _ = store
        .client()
        .put_item()
        .table_name(store.table_name())
        .item("pk", AttributeValue::S(pk))
        .item("sk", AttributeValue::S(sk))
        .item("entity", AttributeValue::S("task_token".to_owned()))
        .item("token", AttributeValue::S(token.to_owned()))
        .condition_expression("attribute_not_exists(pk)")
        .send()
        .await;
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

fn to_error(err: &WorkflowActionError) -> Value {
    serde_json::json!({ "error": err.to_string() })
}
