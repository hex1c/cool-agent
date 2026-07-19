use std::collections::HashMap;

use application::ports::{ObjectClass, PageToken, StorageKey, StorageRecordId, StoredObject};
use application::repositories::{
    ConditionalWriteOutcome, HistoryCheckpoint, HistoryRepository, HistorySequence, PageRequest,
};
use aws_sdk_dynamodb::types::AttributeValue;
use domain::WorkflowTimestamp;
use domain::identity::WorkflowId;
use serde::{Deserialize, Serialize};

use crate::dynamodb::{
    DynamoDbStore, StorageError, deserialize_payload, serialize_payload, sha256_from_hex,
    sha256_to_hex,
};

// ── stored DTO ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredHistoryPointer {
    storage_key: String,
    byte_length: u64,
    sha256_hex: String,
    media_type: String,
    object_id: String,
    object_created_at_secs: u64,
    model_version: String,
    prompt_version: String,
    checkpoint_created_at_secs: u64,
}

impl StoredHistoryPointer {
    fn from_checkpoint(checkpoint: &HistoryCheckpoint) -> Result<Self, StorageError> {
        checkpoint.validate().map_err(|_| {
            StorageError::Validation(
                crate::dynamodb::StoreValidationError::InvalidHistoryCheckpoint,
            )
        })?;
        let object = &checkpoint.object;
        Ok(Self {
            storage_key: object.storage_key.as_str().to_owned(),
            byte_length: object.byte_length,
            sha256_hex: sha256_to_hex(&object.sha256),
            media_type: object.media_type.clone(),
            object_id: object.object_id.as_str().to_owned(),
            object_created_at_secs: object.created_at.as_unix_seconds(),
            model_version: checkpoint.model_version.clone(),
            prompt_version: checkpoint.prompt_version.clone(),
            checkpoint_created_at_secs: checkpoint.created_at.as_unix_seconds(),
        })
    }

    fn to_checkpoint(
        &self,
        workflow_id: &WorkflowId,
        sequence: HistorySequence,
    ) -> Result<HistoryCheckpoint, StorageError> {
        let object_id = StorageRecordId::new(&self.object_id).map_err(|_| corrupt("object_id"))?;
        let storage_key = StorageKey::new(&self.storage_key).map_err(|_| corrupt("storage_key"))?;
        let sha256 = sha256_from_hex("history pointer", &self.sha256_hex)?;
        let object = StoredObject {
            workflow_id: workflow_id.clone(),
            object_id,
            class: ObjectClass::SanitizedHistory,
            storage_key,
            byte_length: self.byte_length,
            sha256,
            media_type: self.media_type.clone(),
            created_at: WorkflowTimestamp::from_unix_seconds(self.object_created_at_secs),
        };
        let checkpoint = HistoryCheckpoint {
            workflow_id: workflow_id.clone(),
            sequence,
            object,
            model_version: self.model_version.clone(),
            prompt_version: self.prompt_version.clone(),
            created_at: WorkflowTimestamp::from_unix_seconds(self.checkpoint_created_at_secs),
        };
        checkpoint
            .validate()
            .map_err(|_| StorageError::CorruptItem {
                entity: "history pointer",
                field: "checkpoint validation",
            })?;
        Ok(checkpoint)
    }
}

// ── helpers ────────────────────────────────────────────────────────

fn attribute_not_exists(expression: &str) -> String {
    format!("attribute_not_exists({expression})")
}

fn string_attr(value: impl Into<String>) -> AttributeValue {
    AttributeValue::S(value.into())
}

fn key_error(error: crate::keys::KeyError) -> StorageError {
    StorageError::Validation(crate::dynamodb::StoreValidationError::Key(error))
}

fn corrupt(field: &'static str) -> StorageError {
    StorageError::CorruptItem {
        entity: "history pointer",
        field,
    }
}

fn required_string_attribute<'a>(
    item: &'a HashMap<String, AttributeValue>,
    entity: &'static str,
    field: &'static str,
) -> Result<&'a str, StorageError> {
    item.get(field)
        .and_then(|attribute| attribute.as_s().ok())
        .map(String::as_str)
        .ok_or(StorageError::CorruptItem { entity, field })
}

fn parse_sequence_from_sk(sk: &str) -> Result<HistorySequence, StorageError> {
    let value = sk
        .strip_prefix("HISTORY#")
        .filter(|value| value.len() == 20 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| corrupt("sk sequence"))?
        .parse()
        .map_err(|_| corrupt("sk sequence"))?;
    Ok(HistorySequence::new(value))
}

// ── impl ───────────────────────────────────────────────────────────

impl HistoryRepository for DynamoDbStore {
    type Error = StorageError;

    async fn append(
        &self,
        checkpoint: &HistoryCheckpoint,
    ) -> Result<ConditionalWriteOutcome, StorageError> {
        // validate before I/O
        checkpoint.validate().map_err(|_| {
            StorageError::Validation(
                crate::dynamodb::StoreValidationError::InvalidHistoryCheckpoint,
            )
        })?;

        let (pk, sk) = crate::keys::history_pointer(&checkpoint.workflow_id, checkpoint.sequence)
            .map_err(key_error)?;

        let stored = StoredHistoryPointer::from_checkpoint(checkpoint)?;
        let payload = serialize_payload("history pointer", &stored)?;

        let result = self
            .client()
            .put_item()
            .table_name(self.table_name())
            .item("pk", string_attr(pk))
            .item("sk", string_attr(sk))
            .item("entity", string_attr("history_pointer"))
            .item("workflow_id", string_attr(checkpoint.workflow_id.as_str()))
            .item("payload", string_attr(payload))
            .condition_expression(attribute_not_exists("pk"))
            .send()
            .await;

        match result {
            Ok(_) => Ok(ConditionalWriteOutcome::Committed),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|e| e.is_conditional_check_failed_exception()) =>
            {
                Ok(ConditionalWriteOutcome::Conflict)
            }
            Err(_) => Err(StorageError::Service {
                operation: "append history pointer",
            }),
        }
    }

    async fn page(
        &self,
        workflow_id: &WorkflowId,
        request: &PageRequest,
    ) -> Result<application::ports::Page<HistoryCheckpoint>, StorageError> {
        let pk = format!("WF#{}", workflow_id);
        let sk_prefix = "HISTORY#";

        let mut query = self
            .client()
            .query()
            .table_name(self.table_name())
            .key_condition_expression("pk = :pk AND begins_with(sk, :prefix)")
            .expression_attribute_values(":pk", string_attr(&pk))
            .expression_attribute_values(":prefix", string_attr(sk_prefix))
            .scan_index_forward(true)
            .limit(i32::from(request.limit));

        if let Some(ref token) = request.token {
            let exclusive_start_key = self
                .decode_page_token(token.as_str(), "history", true, &pk, sk_prefix)
                .map_err(|_| StorageError::CorruptItem {
                    entity: "history pointer",
                    field: "page token",
                })?;
            query = query.set_exclusive_start_key(Some(exclusive_start_key));
        }

        let output = query.send().await.map_err(|_| StorageError::Service {
            operation: "page history pointers",
        })?;

        let mut items = Vec::new();
        if let Some(ddb_items) = output.items {
            for ddb_item in ddb_items {
                let entity = required_string_attribute(&ddb_item, "history pointer", "entity")?;
                if entity != "history_pointer" {
                    return Err(StorageError::CorruptItem {
                        entity: "history pointer",
                        field: "entity",
                    });
                }
                let stored_workflow_id =
                    required_string_attribute(&ddb_item, "history pointer", "workflow_id")?;
                if stored_workflow_id != workflow_id.as_str() {
                    return Err(StorageError::CorruptItem {
                        entity: "history pointer",
                        field: "workflow_id",
                    });
                }
                let stored_sk = required_string_attribute(&ddb_item, "history pointer", "sk")?;
                let sequence = parse_sequence_from_sk(stored_sk)?;
                let payload = required_string_attribute(&ddb_item, "history pointer", "payload")?;
                let stored: StoredHistoryPointer = deserialize_payload("history pointer", payload)?;
                let checkpoint = stored.to_checkpoint(workflow_id, sequence)?;
                items.push(checkpoint);
            }
        }

        let next_token = match output.last_evaluated_key {
            Some(ref lek) if !lek.is_empty() => {
                let token_str = self.encode_page_token(lek, "history", true)?;
                Some(
                    PageToken::new(token_str).map_err(|_| StorageError::CorruptItem {
                        entity: "history pointer",
                        field: "page token",
                    })?,
                )
            }
            _ => None,
        };

        Ok(application::ports::Page { items, next_token })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use application::ports::StorageRecordId;
    use domain::identity::WorkflowId;

    #[test]
    fn stored_history_pointer_round_trips() {
        let id = WorkflowId::new("workflow-1").expect("valid");
        let checkpoint = HistoryCheckpoint {
            workflow_id: id.clone(),
            sequence: HistorySequence::new(9),
            object: StoredObject {
                workflow_id: id,
                object_id: StorageRecordId::new("obj-1").expect("valid"),
                class: ObjectClass::SanitizedHistory,
                storage_key: StorageKey::new("history/workflow-1/00001.json").expect("valid"),
                byte_length: 1024,
                sha256: [0xab; 32],
                media_type: "application/json".to_owned(),
                created_at: WorkflowTimestamp::from_unix_seconds(100),
            },
            model_version: "model-v1".to_owned(),
            prompt_version: "prompt-v1".to_owned(),
            created_at: WorkflowTimestamp::from_unix_seconds(100),
        };

        let stored = StoredHistoryPointer::from_checkpoint(&checkpoint).expect("encode");
        let json = serde_json::to_string(&stored).expect("serialize");
        let decoded: StoredHistoryPointer = serde_json::from_str(&json).expect("deserialize");

        let round_tripped = decoded
            .to_checkpoint(&checkpoint.workflow_id, checkpoint.sequence)
            .expect("decode");

        assert_eq!(round_tripped.workflow_id, checkpoint.workflow_id);
        assert_eq!(round_tripped.sequence, checkpoint.sequence);
        assert_eq!(
            round_tripped.object.byte_length,
            checkpoint.object.byte_length
        );
        assert_eq!(round_tripped.object.sha256, checkpoint.object.sha256);
        assert_eq!(
            round_tripped.object.media_type,
            checkpoint.object.media_type
        );
        assert_eq!(round_tripped.model_version, checkpoint.model_version);
        assert_eq!(round_tripped.prompt_version, checkpoint.prompt_version);
    }

    #[test]
    fn stored_history_pointer_rejects_unknown_fields() {
        let id = WorkflowId::new("workflow-1").expect("valid");
        let checkpoint = HistoryCheckpoint {
            workflow_id: id.clone(),
            sequence: HistorySequence::new(1),
            object: StoredObject {
                workflow_id: id,
                object_id: StorageRecordId::new("obj-1").expect("valid"),
                class: ObjectClass::SanitizedHistory,
                storage_key: StorageKey::new("history/workflow-1/00001.json").expect("valid"),
                byte_length: 42,
                sha256: [0xcd; 32],
                media_type: "text/plain".to_owned(),
                created_at: WorkflowTimestamp::from_unix_seconds(1),
            },
            model_version: "v1".to_owned(),
            prompt_version: "v1".to_owned(),
            created_at: WorkflowTimestamp::from_unix_seconds(1),
        };
        let stored = StoredHistoryPointer::from_checkpoint(&checkpoint).expect("encode");
        let mut value = serde_json::to_value(stored).expect("serialize");
        value
            .as_object_mut()
            .expect("must be object")
            .insert("extra".to_owned(), serde_json::Value::Bool(true));
        let result: Result<StoredHistoryPointer, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }

    #[test]
    fn parse_sequence_from_valid_sk() {
        let seq = parse_sequence_from_sk("HISTORY#00000000000000000009").expect("parse");
        assert_eq!(seq.get(), 9);
    }

    #[test]
    fn parse_sequence_rejects_invalid_sk() {
        assert!(parse_sequence_from_sk("AUDIT#00000000000000000009").is_err());
        assert!(parse_sequence_from_sk("HISTORY#abc").is_err());
        assert!(parse_sequence_from_sk("HISTORY#").is_err());
    }
}
