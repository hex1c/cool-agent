#![cfg(feature = "integration")]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

//! Live verification for the Task 40 local AWS sandbox.
//!
//! The explicitly selected integration test fails when any sandbox service is
//! unavailable. Every required service must pass: DynamoDB conditional writes,
//! S3, SSM, Step Functions seeded wait/resume, Telegram webhook intake and API
//! calls, OAuth, Google, SMTP, and AI.

use std::error::Error;
use std::io::{Error as IoError, ErrorKind};
use std::net::{SocketAddr, TcpStream as StdTcpStream};
use std::time::Duration;

use aws_sdk_dynamodb::config::{Credentials as DynamoCredentials, Region as DynamoRegion};
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_s3::config::{Credentials as S3Credentials, Region as S3Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_sfn::config::{Credentials as SfnCredentials, Region as SfnRegion};
use aws_sdk_sfn::types::ExecutionStatus;
use aws_sdk_ssm::config::{Credentials as SsmCredentials, Region as SsmRegion};
use reqwest::{Client, StatusCode, redirect::Policy};
use serde_json::{Value, json};
use telegram::{WebhookVerifier, normalize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

const APPLICATION_TABLE: &str = "novus-development-application";
const ARTIFACT_BUCKET: &str = "novus-development-artifacts-local";
const TELEGRAM_SECRET: &str = "/novus/development/telegram/bot-token";
const WEBHOOK_SECRET: &str = "/novus/development/telegram/webhook-secret";
const WAIT_RESUME_STATE_MACHINE: &str = "novus-local-wait-resume";

fn endpoint(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn port_open(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    StdTcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok()
}

fn dynamodb_client(endpoint: &str) -> aws_sdk_dynamodb::Client {
    let config = aws_sdk_dynamodb::Config::builder()
        .behavior_version_latest()
        .endpoint_url(endpoint)
        .region(DynamoRegion::new("us-east-1"))
        .credentials_provider(DynamoCredentials::new(
            "test",
            "test",
            None,
            None,
            "local-sandbox",
        ))
        .build();
    aws_sdk_dynamodb::Client::from_conf(config)
}

fn s3_client(endpoint: &str) -> aws_sdk_s3::Client {
    let config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .endpoint_url(endpoint)
        .force_path_style(true)
        .region(S3Region::new("us-east-1"))
        .credentials_provider(S3Credentials::new(
            "test",
            "test",
            None,
            None,
            "local-sandbox",
        ))
        .build();
    aws_sdk_s3::Client::from_conf(config)
}

fn sfn_client(endpoint: &str) -> aws_sdk_sfn::Client {
    let config = aws_sdk_sfn::Config::builder()
        .behavior_version_latest()
        .endpoint_url(endpoint)
        .region(SfnRegion::new("us-east-1"))
        .credentials_provider(SfnCredentials::new(
            "test",
            "test",
            None,
            None,
            "local-sandbox",
        ))
        .build();
    aws_sdk_sfn::Client::from_conf(config)
}

fn ssm_client(endpoint: &str) -> aws_sdk_ssm::Client {
    let config = aws_sdk_ssm::Config::builder()
        .behavior_version_latest()
        .endpoint_url(endpoint)
        .region(SsmRegion::new("us-east-1"))
        .credentials_provider(SsmCredentials::new(
            "test",
            "test",
            None,
            None,
            "local-sandbox",
        ))
        .build();
    aws_sdk_ssm::Client::from_conf(config)
}

async fn wait_resume_succeeded(sfn: &aws_sdk_sfn::Client) -> bool {
    let Ok(state_machines) = sfn.list_state_machines().send().await else {
        return false;
    };
    let Some(state_machine) = state_machines
        .state_machines()
        .iter()
        .find(|item| item.name() == WAIT_RESUME_STATE_MACHINE)
    else {
        return false;
    };
    let Ok(executions) = sfn
        .list_executions()
        .state_machine_arn(state_machine.state_machine_arn())
        .send()
        .await
    else {
        return false;
    };
    executions
        .executions()
        .iter()
        .any(|execution| execution.status() == &ExecutionStatus::Succeeded)
}

async fn wait_for_seed(
    dynamodb: &aws_sdk_dynamodb::Client,
    s3: &aws_sdk_s3::Client,
    sfn: &aws_sdk_sfn::Client,
    ssm: &aws_sdk_ssm::Client,
) -> Result<(), Box<dyn Error>> {
    for _ in 0..60 {
        let table_ready = dynamodb
            .describe_table()
            .table_name(APPLICATION_TABLE)
            .send()
            .await
            .is_ok();
        let bucket_ready = s3
            .head_bucket()
            .bucket(ARTIFACT_BUCKET)
            .send()
            .await
            .is_ok();
        let secret_ready = ssm
            .get_parameter()
            .name(TELEGRAM_SECRET)
            .with_decryption(true)
            .send()
            .await
            .is_ok();
        let wait_resume_ready = wait_resume_succeeded(sfn).await;
        if table_ready && bucket_ready && secret_ready && wait_resume_ready {
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }
    Err(IoError::new(
        ErrorKind::TimedOut,
        "sandbox-init did not seed DynamoDB, S3, SSM, and Step Functions within 30 seconds",
    )
    .into())
}

async fn post_json(client: &Client, url: String, body: Value) -> Result<Value, Box<dyn Error>> {
    Ok(client
        .post(url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

async fn smtp_response(
    stream: &mut BufReader<TcpStream>,
    expected_code: &str,
) -> Result<(), Box<dyn Error>> {
    let mut response = String::new();
    stream.read_line(&mut response).await?;
    if !response.starts_with(expected_code) {
        return Err(IoError::other(format!(
            "unexpected SMTP response: expected {expected_code}, got {response:?}"
        ))
        .into());
    }
    Ok(())
}

async fn smtp_command(
    stream: &mut BufReader<TcpStream>,
    command: &str,
    expected_code: &str,
) -> Result<(), Box<dyn Error>> {
    stream.get_mut().write_all(command.as_bytes()).await?;
    stream.get_mut().flush().await?;
    smtp_response(stream, expected_code).await
}

async fn exercise_smtp(endpoint: &str, marker: &str) -> Result<(), Box<dyn Error>> {
    let address = endpoint
        .strip_prefix("smtp://")
        .unwrap_or(endpoint)
        .to_owned();
    let mut stream = BufReader::new(TcpStream::connect(address).await?);
    smtp_response(&mut stream, "220").await?;
    smtp_command(&mut stream, "HELO sandbox.local\r\n", "250").await?;
    smtp_command(&mut stream, "MAIL FROM:<novus@sandbox.local>\r\n", "250").await?;
    smtp_command(&mut stream, "RCPT TO:<recipient@sandbox.local>\r\n", "250").await?;
    smtp_command(&mut stream, "DATA\r\n", "354").await?;
    smtp_command(
        &mut stream,
        &format!(
            "Subject: Novus sandbox SMTP path {marker}\r\n\r\nSanitized local message.\r\n.\r\n"
        ),
        "250",
    )
    .await?;
    smtp_command(&mut stream, "QUIT\r\n", "221").await?;
    Ok(())
}

async fn verify_provider_paths(
    mock_endpoint: &str,
    mailhog_endpoint: &str,
    smtp_endpoint: &str,
    smtp_marker: &str,
) -> Result<(), Box<dyn Error>> {
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;

    let membership = post_json(
        &client,
        format!("{mock_endpoint}/botlocal-test-token/getChatMember"),
        json!({ "chat_id": -1000000000001_i64, "user_id": 4242 }),
    )
    .await?;
    assert_eq!(membership["result"]["status"], "member");

    let delivery = post_json(
        &client,
        format!("{mock_endpoint}/botlocal-test-token/sendMessage"),
        json!({ "chat_id": -1000000000001_i64, "text": "Sandbox delivery" }),
    )
    .await?;
    assert_eq!(delivery["result"]["message_id"], 9001);

    let authorize = client
        .get(format!("{mock_endpoint}/o/oauth2/v2/auth"))
        .send()
        .await?;
    assert_eq!(authorize.status(), StatusCode::FOUND);
    assert!(authorize.headers().get("location").is_some());

    let token = post_json(
        &client,
        format!("{mock_endpoint}/oauth2/v4/token"),
        json!({ "code": "local-code", "code_verifier": "local-verifier" }),
    )
    .await?;
    assert_eq!(token["token_type"], "Bearer");

    let drive = post_json(
        &client,
        format!("{mock_endpoint}/drive/v3/files"),
        json!({ "name": "sandbox-quotation.pdf" }),
    )
    .await?;
    assert_eq!(drive["id"], "sandbox-drive-file-1");

    let calendar = post_json(
        &client,
        format!("{mock_endpoint}/calendar/v3/calendars/primary/events"),
        json!({ "summary": "Sandbox planning meeting" }),
    )
    .await?;
    assert_eq!(calendar["status"], "confirmed");

    let ai = post_json(
        &client,
        format!("{mock_endpoint}/v1/chat/completions"),
        json!({ "model": "sandbox-basic-reasoning", "messages": [] }),
    )
    .await?;
    let extraction: Value = serde_json::from_str(
        ai["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| IoError::other("AI mock response omitted extraction content"))?,
    )?;
    assert_eq!(extraction["schemaVersion"], "novus.extraction.v1");

    timeout(
        Duration::from_secs(5),
        exercise_smtp(smtp_endpoint, smtp_marker),
    )
    .await??;
    for _ in 0..20 {
        let response = client
            .get(format!("{mailhog_endpoint}/api/v2/messages"))
            .send()
            .await?;
        if response
            .error_for_status()?
            .text()
            .await?
            .contains(smtp_marker)
        {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(IoError::new(
        ErrorKind::TimedOut,
        "MailHog did not capture the sandbox SMTP message",
    )
    .into())
}

#[tokio::test]
async fn local_sandbox_exercises_seeded_aws_and_provider_paths() -> Result<(), Box<dyn Error>> {
    for (override_name, port) in [
        ("DYNAMODB_ENDPOINT", 8000),
        ("LOCALSTACK_ENDPOINT", 4566),
        ("STEPFUNCTIONS_ENDPOINT", 8083),
        ("MOCK_PROVIDERS_ENDPOINT", 8081),
        ("SMTP_ENDPOINT", 1025),
        ("MAILHOG_ENDPOINT", 8025),
    ] {
        if std::env::var_os(override_name).is_none() && !port_open(port) {
            return Err(IoError::new(
                ErrorKind::ConnectionRefused,
                format!("required local sandbox port {port} is unavailable"),
            )
            .into());
        }
    }

    let dynamodb_endpoint = endpoint("DYNAMODB_ENDPOINT", "http://127.0.0.1:8000");
    let localstack_endpoint = endpoint("LOCALSTACK_ENDPOINT", "http://127.0.0.1:4566");
    let stepfunctions_endpoint = endpoint("STEPFUNCTIONS_ENDPOINT", "http://127.0.0.1:8083");
    let mock_endpoint = endpoint("MOCK_PROVIDERS_ENDPOINT", "http://127.0.0.1:8081");
    let smtp_endpoint = endpoint("SMTP_ENDPOINT", "127.0.0.1:1025");
    let mailhog_endpoint = endpoint("MAILHOG_ENDPOINT", "http://127.0.0.1:8025");

    let dynamodb = dynamodb_client(&dynamodb_endpoint);
    let s3 = s3_client(&localstack_endpoint);
    let sfn = sfn_client(&stepfunctions_endpoint);
    let ssm = ssm_client(&localstack_endpoint);
    wait_for_seed(&dynamodb, &s3, &sfn, &ssm).await?;

    let unique = Uuid::new_v4().simple().to_string();
    let pk = format!("SANDBOX#CONDITIONAL#{unique}");
    let sk = "PROOF";
    dynamodb
        .put_item()
        .table_name(APPLICATION_TABLE)
        .item("pk", AttributeValue::S(pk.clone()))
        .item("sk", AttributeValue::S(sk.to_owned()))
        .item("sanitized", AttributeValue::Bool(true))
        .condition_expression("attribute_not_exists(pk)")
        .send()
        .await?;
    let duplicate = dynamodb
        .put_item()
        .table_name(APPLICATION_TABLE)
        .item("pk", AttributeValue::S(pk.clone()))
        .item("sk", AttributeValue::S(sk.to_owned()))
        .condition_expression("attribute_not_exists(pk)")
        .send()
        .await;
    assert!(
        duplicate.is_err(),
        "duplicate conditional write was accepted"
    );

    let object_key = format!("artifacts/sandbox-workflow/{unique}.txt");
    let object_body = b"sanitized sandbox artifact";
    s3.put_object()
        .bucket(ARTIFACT_BUCKET)
        .key(&object_key)
        .content_type("text/plain")
        .body(ByteStream::from_static(object_body))
        .send()
        .await?;
    let stored = s3
        .get_object()
        .bucket(ARTIFACT_BUCKET)
        .key(&object_key)
        .send()
        .await?
        .body
        .collect()
        .await?
        .into_bytes();
    assert_eq!(stored.as_ref(), object_body);

    let secret = ssm
        .get_parameter()
        .name(TELEGRAM_SECRET)
        .with_decryption(true)
        .send()
        .await?;
    assert_eq!(
        secret.parameter().and_then(|parameter| parameter.value()),
        Some("local-test-token")
    );

    let webhook_secret = ssm
        .get_parameter()
        .name(WEBHOOK_SECRET)
        .with_decryption(true)
        .send()
        .await?;
    let webhook_secret = webhook_secret
        .parameter()
        .and_then(|parameter| parameter.value())
        .ok_or_else(|| IoError::other("seeded webhook secret has no value"))?;
    WebhookVerifier::new(webhook_secret.to_owned())
        .verify(Some("local-webhook-secret-not-real"))?;
    let normalized = normalize::normalize(include_bytes!(
        "../../../tests/fixtures/telegram/mention.json"
    ))?;
    assert_eq!(normalized.update_id, 1001);

    let mocks: Value = serde_json::from_str(include_str!(
        "../../../infrastructure/local/mock-providers.json"
    ))?;
    assert_eq!(mocks["sanitized"], true);
    verify_provider_paths(&mock_endpoint, &mailhog_endpoint, &smtp_endpoint, &unique).await?;

    dynamodb
        .delete_item()
        .table_name(APPLICATION_TABLE)
        .key("pk", AttributeValue::S(pk))
        .key("sk", AttributeValue::S(sk.to_owned()))
        .send()
        .await?;
    s3.delete_object()
        .bucket(ARTIFACT_BUCKET)
        .key(object_key)
        .send()
        .await?;

    Ok(())
}
