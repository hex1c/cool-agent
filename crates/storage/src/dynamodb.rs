use std::collections::HashMap;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

pub const TABLE_NAME_MIN_LENGTH: usize = 3;
pub const TABLE_NAME_MAX_LENGTH: usize = 255;
/// Conservative payload ceiling before dispatching a write (DynamoDB item
/// limit is 400 KB).
pub const MAX_PAYLOAD_BYTES: usize = 384_000;

/// Validation failures reject a table name, key, or payload before any
/// service call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreValidationError {
    TableNameEmpty,
    TableNameTooShort { length: usize, minimum: usize },
    TableNameTooLong { length: usize, maximum: usize },
    TableNameInvalidCharacters,
    Key(super::keys::KeyError),
    PayloadTooLarge { bytes: usize, maximum: usize },
    InvalidTransitionRevisions,
    InvalidConfirmationOperationBinding,
    InvalidHistoryCheckpoint,
    InvalidObjectMetadata,
}

impl Display for StoreValidationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TableNameEmpty => formatter.write_str("table name is empty"),
            Self::TableNameTooShort { length, minimum } => write!(
                formatter,
                "table name length {length} is below minimum {minimum}"
            ),
            Self::TableNameTooLong { length, maximum } => write!(
                formatter,
                "table name length {length} exceeds maximum {maximum}"
            ),
            Self::TableNameInvalidCharacters => {
                formatter.write_str("table name contains invalid characters")
            }
            Self::Key(error) => write!(formatter, "key error: {error:?}"),
            Self::PayloadTooLarge { bytes, maximum } => {
                write!(formatter, "payload size {bytes} exceeds {maximum} bytes")
            }
            Self::InvalidTransitionRevisions => {
                formatter.write_str("workflow and audit revisions are inconsistent")
            }
            Self::InvalidConfirmationOperationBinding => {
                formatter.write_str("confirmation and operation binding is invalid")
            }
            Self::InvalidHistoryCheckpoint => formatter.write_str("history checkpoint is invalid"),
            Self::InvalidObjectMetadata => formatter.write_str("object metadata is invalid"),
        }
    }
}

impl std::error::Error for StoreValidationError {}

/// Every storage adapter error is exposed only through this type so that
/// raw AWS-provider responses, request IDs, and retry metadata are never
/// leaked to callers or logs.
#[derive(Debug)]
pub enum StorageError {
    Validation(StoreValidationError),
    Serialization {
        entity: &'static str,
        cause: serde_json::Error,
    },
    Deserialization {
        entity: &'static str,
        cause: serde_json::Error,
    },
    CorruptItem {
        entity: &'static str,
        field: &'static str,
    },
    Build {
        operation: &'static str,
    },
    Service {
        operation: &'static str,
    },
}

impl Display for StorageError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(error) => error.fmt(formatter),
            Self::Serialization { entity, cause } => {
                write!(formatter, "failed to serialize {entity}: {cause}")
            }
            Self::Deserialization { entity, cause } => {
                write!(formatter, "failed to deserialize {entity}: {cause}")
            }
            Self::CorruptItem { entity, field } => {
                write!(formatter, "stored {entity} has invalid {field}")
            }
            Self::Build { operation } => {
                write!(formatter, "failed to build DynamoDB {operation}")
            }
            Self::Service { operation } => {
                write!(formatter, "DynamoDB {operation} failed")
            }
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialization { cause, .. } | Self::Deserialization { cause, .. } => Some(cause),
            Self::Validation(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StoreValidationError> for StorageError {
    fn from(value: StoreValidationError) -> Self {
        Self::Validation(value)
    }
}

/// Owned connection to a single DynamoDB application table.
///
/// Construct one per Lambda initialisation; the client is expected to be
/// pre-configured with credentials, region, and endpoint.
#[derive(Clone)]
pub struct DynamoDbStore {
    client: aws_sdk_dynamodb::Client,
    table_name: String,
}

impl std::fmt::Debug for DynamoDbStore {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DynamoDbStore")
            .field("client", &"[REDACTED]")
            .field("table_name", &self.table_name)
            .finish()
    }
}

impl DynamoDbStore {
    pub fn new(
        client: aws_sdk_dynamodb::Client,
        table_name: impl Into<String>,
    ) -> Result<Self, StoreValidationError> {
        let table_name = table_name.into();
        validate_table_name(&table_name)?;
        Ok(Self { client, table_name })
    }

    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    pub fn client(&self) -> &aws_sdk_dynamodb::Client {
        &self.client
    }
}

fn validate_table_name(value: &str) -> Result<(), StoreValidationError> {
    if value.is_empty() {
        return Err(StoreValidationError::TableNameEmpty);
    }
    if value.len() > TABLE_NAME_MAX_LENGTH {
        return Err(StoreValidationError::TableNameTooLong {
            length: value.len(),
            maximum: TABLE_NAME_MAX_LENGTH,
        });
    }
    if value.len() < TABLE_NAME_MIN_LENGTH {
        return Err(StoreValidationError::TableNameTooShort {
            length: value.len(),
            minimum: TABLE_NAME_MIN_LENGTH,
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(StoreValidationError::TableNameInvalidCharacters);
    }
    Ok(())
}

/// Serialize a domain value and reject oversize payloads.
pub fn serialize_payload<T: serde::Serialize>(
    entity: &'static str,
    value: &T,
) -> Result<String, StorageError> {
    let json = serde_json::to_string(value)
        .map_err(|cause| StorageError::Serialization { entity, cause })?;
    if json.len() > MAX_PAYLOAD_BYTES {
        return Err(StorageError::Validation(
            StoreValidationError::PayloadTooLarge {
                bytes: json.len(),
                maximum: MAX_PAYLOAD_BYTES,
            },
        ));
    }
    Ok(json)
}

/// Deserialize a domain value from a stored payload string.
pub fn deserialize_payload<'a, T: serde::Deserialize<'a>>(
    entity: &'static str,
    payload: &'a str,
) -> Result<T, StorageError> {
    serde_json::from_str(payload).map_err(|cause| StorageError::Deserialization { entity, cause })
}

/// Error returned when a stored page token is malformed or belongs to a
/// different workflow/query family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTokenError {
    reason: &'static str,
}

impl Display for PageTokenError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid page token: {}", self.reason)
    }
}

impl std::error::Error for PageTokenError {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPageToken {
    pk: String,
    sk: String,
}

/// Encode a DynamoDB last-evaluated key into an opaque page token.
pub fn encode_page_token(
    last_evaluated_key: &HashMap<String, aws_sdk_dynamodb::types::AttributeValue>,
) -> Result<String, StorageError> {
    let pk = required_page_key(last_evaluated_key, "pk")?;
    let sk = required_page_key(last_evaluated_key, "sk")?;
    let bytes = serde_json::to_vec(&StoredPageToken {
        pk: pk.to_owned(),
        sk: sk.to_owned(),
    })
    .map_err(|_| StorageError::Build {
        operation: "page token encode",
    })?;

    use base64::Engine;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Decode a page token and bind it to exactly one workflow partition and
/// one sort-key family before returning it to DynamoDB.
pub fn decode_page_token(
    token: &str,
    expected_pk: &str,
    expected_sk_prefix: &str,
) -> Result<HashMap<String, aws_sdk_dynamodb::types::AttributeValue>, PageTokenError> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| page_token_error("invalid base64"))?;
    let parsed: StoredPageToken =
        serde_json::from_slice(&bytes).map_err(|_| page_token_error("invalid payload"))?;

    if parsed.pk != expected_pk {
        return Err(page_token_error("partition mismatch"));
    }
    if !parsed.sk.starts_with(expected_sk_prefix) {
        return Err(page_token_error("sort-key family mismatch"));
    }

    Ok(HashMap::from([
        (
            "pk".to_owned(),
            aws_sdk_dynamodb::types::AttributeValue::S(parsed.pk),
        ),
        (
            "sk".to_owned(),
            aws_sdk_dynamodb::types::AttributeValue::S(parsed.sk),
        ),
    ]))
}

fn required_page_key<'a>(
    key: &'a HashMap<String, aws_sdk_dynamodb::types::AttributeValue>,
    field: &'static str,
) -> Result<&'a str, StorageError> {
    key.get(field)
        .and_then(|value| value.as_s().ok())
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(StorageError::Build {
            operation: "page token encode",
        })
}

const fn page_token_error(reason: &'static str) -> PageTokenError {
    PageTokenError { reason }
}

pub(crate) fn sha256_to_hex(bytes: &[u8; 32]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            use std::fmt::Write;
            let _ = write!(output, "{byte:02x}");
            output
        })
}

pub(crate) fn sha256_from_hex(entity: &'static str, value: &str) -> Result<[u8; 32], StorageError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(StorageError::CorruptItem {
            entity,
            field: "sha256_hex",
        });
    }

    let mut digest = [0_u8; 32];
    for (slot, pair) in digest.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let [high, low] = pair else {
            return Err(StorageError::CorruptItem {
                entity,
                field: "sha256_hex",
            });
        };
        *slot = (hex_nibble(*high) << 4) | hex_nibble(*low);
    }
    Ok(digest)
}

const fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn table_name_rejects_empty_or_control_bearing_values() {
        assert!(matches!(
            validate_table_name(""),
            Err(StoreValidationError::TableNameEmpty)
        ));
        assert!(matches!(
            validate_table_name("ab"),
            Err(StoreValidationError::TableNameTooShort { .. })
        ));
        assert!(validate_table_name("novus-app-dev").is_ok());
        assert!(matches!(
            validate_table_name("has\ncontrol"),
            Err(StoreValidationError::TableNameInvalidCharacters)
        ));
        let too_long = "x".repeat(TABLE_NAME_MAX_LENGTH + 1);
        assert!(matches!(
            validate_table_name(&too_long),
            Err(StoreValidationError::TableNameTooLong { .. })
        ));
    }

    #[test]
    fn store_construction_rejects_invalid_tables() -> Result<(), Box<dyn std::error::Error>> {
        let config = aws_sdk_dynamodb::Config::builder()
            .endpoint_url("http://localhost:8000")
            .build();
        let client = aws_sdk_dynamodb::Client::from_conf(config);
        assert!(matches!(
            DynamoDbStore::new(client.clone(), ""),
            Err(StoreValidationError::TableNameEmpty)
        ));
        let store = DynamoDbStore::new(client, "novus-app-dev")?;
        assert_eq!(
            format!("{store:?}"),
            "DynamoDbStore { client: \"[REDACTED]\", table_name: \"novus-app-dev\" }"
        );
        Ok(())
    }

    #[test]
    fn payload_serialization_enforces_size_limit() {
        let large = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        let result = serialize_payload("test", &large);
        assert!(matches!(
            result,
            Err(StorageError::Validation(
                StoreValidationError::PayloadTooLarge { .. }
            ))
        ));

        let small = [1_u64, 2, 3];
        let result = serialize_payload("test", &small);
        assert!(result.is_ok());
    }

    #[test]
    fn page_tokens_round_trip_and_are_bound_to_exact_partition() {
        let key = HashMap::from([
            (
                "pk".to_owned(),
                aws_sdk_dynamodb::types::AttributeValue::S("WF#workflow-1".to_owned()),
            ),
            (
                "sk".to_owned(),
                aws_sdk_dynamodb::types::AttributeValue::S(
                    "HISTORY#00000000000000000009".to_owned(),
                ),
            ),
        ]);
        let token = encode_page_token(&key).expect("valid key should encode");
        let decoded = decode_page_token(&token, "WF#workflow-1", "HISTORY#")
            .expect("matching token should decode");
        assert_eq!(decoded, key);
        assert!(decode_page_token(&token, "WF#workflow-10", "HISTORY#").is_err());
        assert!(decode_page_token(&token, "WF#workflow-1", "OBJECT#").is_err());
    }

    #[test]
    fn page_tokens_reject_unknown_fields() {
        use base64::Engine;
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"pk":"WF#workflow-1","sk":"HISTORY#1","extra":true}"#);
        assert!(decode_page_token(&token, "WF#workflow-1", "HISTORY#").is_err());
    }

    #[test]
    fn sha256_encoding_is_canonical_and_round_trips() {
        let digest = [0xab; 32];
        let encoded = sha256_to_hex(&digest);
        assert_eq!(encoded, "ab".repeat(32));
        assert_eq!(
            sha256_from_hex("test", &encoded).expect("valid hex"),
            digest
        );
        assert!(sha256_from_hex("test", &"AB".repeat(32)).is_err());
        assert!(sha256_from_hex("test", "too-short").is_err());
    }

    #[test]
    fn service_errors_do_not_include_provider_messages() {
        let error = StorageError::Service {
            operation: "load workflow",
        };
        assert_eq!(error.to_string(), "DynamoDB load workflow failed");
    }
}
