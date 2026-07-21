#![cfg(feature = "integration")]

use application::ports::{
    ArtifactLinkSigner, ObjectClass, ObjectStore, SecretProvider, SecretReference, StorageKey,
    StorageRecordId, StoredObject,
};
use aws_sdk_s3::config::{Credentials as S3Credentials, Region as S3Region};
use aws_sdk_ssm::config::{Credentials as SsmCredentials, Region as SsmRegion};
use aws_sdk_ssm::types::ParameterType;
use domain::WorkflowTimestamp;
use domain::identity::WorkflowId;
use sha2::{Digest, Sha256};
use storage::s3::{S3Error, S3ObjectStore, S3ValidationError};
use storage::secrets::{SecretProviderError, SecretProviderValidationError, SsmSecretProvider};

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:4566";

fn localstack_available() -> Result<bool, Box<dyn std::error::Error>> {
    if std::env::var_os("LOCALSTACK_ENDPOINT").is_some() {
        return Ok(true);
    }
    Ok(std::net::TcpStream::connect_timeout(
        &"127.0.0.1:4566".parse()?,
        std::time::Duration::from_millis(250),
    )
    .is_ok())
}

fn endpoint() -> String {
    std::env::var("LOCALSTACK_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned())
}

fn s3_client() -> aws_sdk_s3::Client {
    let config = aws_sdk_s3::Config::builder()
        .behavior_version_latest()
        .endpoint_url(endpoint())
        .force_path_style(true)
        .region(S3Region::new("us-east-1"))
        .credentials_provider(S3Credentials::new(
            "test",
            "test",
            None,
            None,
            "localstack-s3-integration",
        ))
        .build();
    aws_sdk_s3::Client::from_conf(config)
}

fn ssm_client() -> aws_sdk_ssm::Client {
    let config = aws_sdk_ssm::Config::builder()
        .behavior_version_latest()
        .endpoint_url(endpoint())
        .region(SsmRegion::new("us-east-1"))
        .credentials_provider(SsmCredentials::new(
            "test",
            "test",
            None,
            None,
            "localstack-ssm-integration",
        ))
        .build();
    aws_sdk_ssm::Client::from_conf(config)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn object(
    workflow_id: &WorkflowId,
    object_id: &str,
    class: ObjectClass,
    storage_key: &str,
    media_type: &str,
    bytes: &[u8],
) -> Result<StoredObject, Box<dyn std::error::Error>> {
    Ok(StoredObject {
        workflow_id: workflow_id.clone(),
        object_id: StorageRecordId::new(object_id)?,
        class,
        storage_key: StorageKey::new(storage_key)?,
        byte_length: u64::try_from(bytes.len())?,
        sha256: sha256(bytes),
        media_type: media_type.to_owned(),
        created_at: WorkflowTimestamp::from_unix_seconds(1),
    })
}

#[tokio::test]
async fn s3_objects_presigning_and_secure_parameters_round_trip()
-> Result<(), Box<dyn std::error::Error>> {
    if !localstack_available()? {
        eprintln!("skipping S3/SSM integration test: LocalStack port 4566 is unavailable");
        return Ok(());
    }

    let unique = uuid::Uuid::new_v4().simple().to_string();
    let bucket = format!("novus-storage-{unique}");
    let s3_client = s3_client();
    s3_client.create_bucket().bucket(&bucket).send().await?;
    let store = S3ObjectStore::new(
        s3_client.clone(),
        &bucket,
        1_048_576,
        1_048_576,
        1_048_576,
        900,
    )?;
    let workflow_id = WorkflowId::new(format!("workflow-{unique}"))?;

    let raw_bytes = b"raw attachment";
    let raw = object(
        &workflow_id,
        "raw-1",
        ObjectClass::RawInput,
        &format!("raw/{workflow_id}/raw-1.bin"),
        "application/octet-stream",
        raw_bytes,
    )?;
    store.put(&raw, raw_bytes).await?;
    assert_eq!(store.get(&raw).await?, raw_bytes);
    assert!(store.put(&raw, raw_bytes).await.is_err());

    let artifact_bytes = b"generated artifact";
    let artifact = object(
        &workflow_id,
        "artifact-1",
        ObjectClass::Artifact,
        &format!("artifacts/{workflow_id}/artifact-1.pdf"),
        "application/pdf",
        artifact_bytes,
    )?;
    store.put(&artifact, artifact_bytes).await?;
    assert_eq!(store.get(&artifact).await?, artifact_bytes);
    let link = store.presign_artifact(&artifact).await?;
    assert!(link.as_str().contains("X-Amz-Expires=900"));

    let history_bytes = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"user","content":"hello"}]}"#;
    let history = object(
        &workflow_id,
        "history-1",
        ObjectClass::SanitizedHistory,
        &format!("history/{workflow_id}/00000000000000000001.json"),
        "application/json",
        history_bytes,
    )?;
    store.put(&history, history_bytes).await?;
    assert_eq!(store.get(&history).await?, history_bytes);

    let unsafe_history_bytes = br#"{"refreshToken":"must-not-persist"}"#;
    let unsafe_history = object(
        &workflow_id,
        "history-2",
        ObjectClass::SanitizedHistory,
        &format!("history/{workflow_id}/00000000000000000002.json"),
        "application/json",
        unsafe_history_bytes,
    )?;
    assert!(matches!(
        store.put(&unsafe_history, unsafe_history_bytes).await,
        Err(S3Error::Validation(S3ValidationError::CredentialKeyPresent))
    ));

    let ssm_client = ssm_client();
    let secure_name = format!("/novus/development/integration/{unique}/secret");
    ssm_client
        .put_parameter()
        .name(&secure_name)
        .r#type(ParameterType::SecureString)
        .value("integration-secret-value")
        .send()
        .await?;
    let provider = SsmSecretProvider::new(ssm_client.clone(), "development")?;
    let secure_reference = SecretReference::new(&secure_name)?;
    let secret = provider.get_secret(&secure_reference).await?;
    assert_eq!(secret.expose(), b"integration-secret-value");
    assert_eq!(format!("{secret:?}"), "SecretValue([REDACTED])");

    let plain_name = format!("/novus/development/integration/{unique}/plain");
    ssm_client
        .put_parameter()
        .name(&plain_name)
        .r#type(ParameterType::String)
        .value("not-a-secure-string")
        .send()
        .await?;
    let plain_reference = SecretReference::new(&plain_name)?;
    assert!(matches!(
        provider.get_secret(&plain_reference).await,
        Err(SecretProviderError::Validation(
            SecretProviderValidationError::ParameterNotSecureString
        ))
    ));
    let cross_environment =
        SecretReference::new(format!("/novus/staging/integration/{unique}/secret"))?;
    assert!(matches!(
        provider.get_secret(&cross_environment).await,
        Err(SecretProviderError::Validation(
            SecretProviderValidationError::ReferenceOutOfScope
        ))
    ));

    for key in [
        raw.storage_key.as_str(),
        artifact.storage_key.as_str(),
        history.storage_key.as_str(),
    ] {
        s3_client
            .delete_object()
            .bucket(&bucket)
            .key(key)
            .send()
            .await?;
    }
    s3_client.delete_bucket().bucket(&bucket).send().await?;
    ssm_client
        .delete_parameters()
        .names(secure_name)
        .names(plain_name)
        .send()
        .await?;

    Ok(())
}
