use std::fmt::{Display, Formatter};

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
    fn service_errors_do_not_include_provider_messages() {
        let error = StorageError::Service {
            operation: "load workflow",
        };
        assert_eq!(error.to_string(), "DynamoDB load workflow failed");
    }
}
