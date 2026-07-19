use std::collections::HashMap;

use application::ports::{ObjectClass, PageToken, StorageKey, StorageRecordId, StoredObject};
use application::repositories::{ConditionalWriteOutcome, ObjectMetadataRepository, PageRequest};
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
struct StoredObjectMetadata {
    object_id: String,
    class: String,
    storage_key: String,
    byte_length: u64,
    sha256_hex: String,
    media_type: String,
    created_at_secs: u64,
}

impl StoredObjectMetadata {
    fn from_stored(object: &StoredObject) -> Result<Self, StorageError> {
        object.validate().map_err(|_| {
            StorageError::Validation(crate::dynamodb::StoreValidationError::InvalidObjectMetadata)
        })?;
        Ok(Self {
            object_id: object.object_id.as_str().to_owned(),
            class: object_class_str(object.class).to_owned(),
            storage_key: object.storage_key.as_str().to_owned(),
            byte_length: object.byte_length,
            sha256_hex: sha256_to_hex(&object.sha256),
            media_type: object.media_type.clone(),
            created_at_secs: object.created_at.as_unix_seconds(),
        })
    }

    fn to_stored(&self, workflow_id: &WorkflowId) -> Result<StoredObject, StorageError> {
        let object_id = StorageRecordId::new(&self.object_id).map_err(|_| corrupt("object_id"))?;
        let storage_key = StorageKey::new(&self.storage_key).map_err(|_| corrupt("storage_key"))?;
        let class = parse_class(&self.class)?;
        let sha256 = sha256_from_hex("object metadata", &self.sha256_hex)?;
        let object = StoredObject {
            workflow_id: workflow_id.clone(),
            object_id,
            class,
            storage_key,
            byte_length: self.byte_length,
            sha256,
            media_type: self.media_type.clone(),
            created_at: WorkflowTimestamp::from_unix_seconds(self.created_at_secs),
        };
        object
            .validate()
            .map_err(|_| corrupt("object validation"))?;
        Ok(object)
    }
}

const fn object_class_str(class: ObjectClass) -> &'static str {
    match class {
        ObjectClass::RawInput => "raw",
        ObjectClass::Artifact => "artifact",
        ObjectClass::SanitizedHistory => "history",
    }
}

fn parse_class(value: &str) -> Result<ObjectClass, StorageError> {
    match value {
        "raw" => Ok(ObjectClass::RawInput),
        "artifact" => Ok(ObjectClass::Artifact),
        "history" => Ok(ObjectClass::SanitizedHistory),
        _ => Err(corrupt("class")),
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
        entity: "object metadata",
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

impl DynamoDbStore {
    /// Strongly consistent read of an object-metadata item at `(pk, sk)`,
    /// deserialization, and comparison against the requested payload. If
    /// the stored record matches, the conflict is an idempotent retry and
    /// `Committed` is returned. If the stored record differs or is
    /// corrupt, `ConflictDifferent` is returned.
    async fn verify_object_conflict(
        &self,
        pk: &str,
        sk: &str,
        expected_workflow_id: &WorkflowId,
        expected: &StoredObjectMetadata,
    ) -> Result<ConditionalWriteOutcome, StorageError> {
        let output = self
            .client()
            .get_item()
            .table_name(self.table_name())
            .key("pk", string_attr(pk))
            .key("sk", string_attr(sk))
            .consistent_read(true)
            .send()
            .await
            .map_err(|_| StorageError::Service {
                operation: "verify object conflict",
            })?;

        let item = output.item.ok_or(StorageError::ConflictDifferent {
            entity: "object metadata",
        })?;

        // Every mismatch during conflict verification — including
        // missing attributes, wrong entity, wrong workflow_id, and
        // malformed payloads — must map to ConflictDifferent, not
        // CorruptItem or Deserialization.
        let entity =
            required_string_attribute(&item, "object metadata", "entity").map_err(|_| {
                StorageError::ConflictDifferent {
                    entity: "object metadata",
                }
            })?;
        if entity != "object_metadata" {
            return Err(StorageError::ConflictDifferent {
                entity: "object metadata",
            });
        }
        let stored_workflow_id = required_string_attribute(&item, "object metadata", "workflow_id")
            .map_err(|_| StorageError::ConflictDifferent {
                entity: "object metadata",
            })?;
        if stored_workflow_id != expected_workflow_id.as_str() {
            return Err(StorageError::ConflictDifferent {
                entity: "object metadata",
            });
        }
        let payload =
            required_string_attribute(&item, "object metadata", "payload").map_err(|_| {
                StorageError::ConflictDifferent {
                    entity: "object metadata",
                }
            })?;
        let stored: StoredObjectMetadata = deserialize_payload("object metadata", payload)
            .map_err(|_| StorageError::ConflictDifferent {
                entity: "object metadata",
            })?;
        if stored == *expected {
            Ok(ConditionalWriteOutcome::Committed)
        } else {
            Err(StorageError::ConflictDifferent {
                entity: "object metadata",
            })
        }
    }
}

// ── impl ───────────────────────────────────────────────────────────

impl ObjectMetadataRepository for DynamoDbStore {
    type Error = StorageError;

    async fn record(&self, object: &StoredObject) -> Result<ConditionalWriteOutcome, StorageError> {
        // validate before I/O
        object.validate().map_err(|_| {
            StorageError::Validation(crate::dynamodb::StoreValidationError::InvalidObjectMetadata)
        })?;

        let (pk, sk) =
            crate::keys::object_metadata(&object.workflow_id, object.class, &object.object_id)
                .map_err(key_error)?;
        let pk_for_conflict = pk.clone();
        let sk_for_conflict = sk.clone();

        let stored = StoredObjectMetadata::from_stored(object)?;
        let payload = serialize_payload("object metadata", &stored)?;

        let result = self
            .client()
            .put_item()
            .table_name(self.table_name())
            .item("pk", string_attr(pk))
            .item("sk", string_attr(sk))
            .item("entity", string_attr("object_metadata"))
            .item("workflow_id", string_attr(object.workflow_id.as_str()))
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
                // A conditional failure means *something* exists at this
                // key. Prove it is the same immutable object via a strongly
                // consistent read; if the payload differs, this is a
                // corruption/collision, not an idempotent retry.
                self.verify_object_conflict(
                    &pk_for_conflict,
                    &sk_for_conflict,
                    &object.workflow_id,
                    &stored,
                )
                .await
            }
            Err(_) => Err(StorageError::Service {
                operation: "record object metadata",
            }),
        }
    }

    async fn page(
        &self,
        workflow_id: &WorkflowId,
        request: &PageRequest,
    ) -> Result<application::ports::Page<StoredObject>, StorageError> {
        let pk = format!("WF#{}", workflow_id);
        let sk_prefix = "OBJECT#";

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
                .decode_page_token(token.as_str(), "objects", true, &pk, sk_prefix)
                .map_err(|_| StorageError::CorruptItem {
                    entity: "object metadata",
                    field: "page token",
                })?;
            query = query.set_exclusive_start_key(Some(exclusive_start_key));
        }

        let output = query.send().await.map_err(|_| StorageError::Service {
            operation: "page object metadata",
        })?;

        let mut items = Vec::new();
        if let Some(ddb_items) = output.items {
            for ddb_item in ddb_items {
                let entity = required_string_attribute(&ddb_item, "object metadata", "entity")?;
                if entity != "object_metadata" {
                    return Err(StorageError::CorruptItem {
                        entity: "object metadata",
                        field: "entity",
                    });
                }
                let stored_workflow_id =
                    required_string_attribute(&ddb_item, "object metadata", "workflow_id")?;
                if stored_workflow_id != workflow_id.as_str() {
                    return Err(StorageError::CorruptItem {
                        entity: "object metadata",
                        field: "workflow_id",
                    });
                }
                let payload = required_string_attribute(&ddb_item, "object metadata", "payload")?;
                let stored: StoredObjectMetadata = deserialize_payload("object metadata", payload)?;
                let object = stored.to_stored(workflow_id)?;
                let stored_sk = required_string_attribute(&ddb_item, "object metadata", "sk")?;
                let (_, expected_sk) =
                    crate::keys::object_metadata(workflow_id, object.class, &object.object_id)
                        .map_err(key_error)?;
                if stored_sk != expected_sk {
                    return Err(corrupt("sk"));
                }
                items.push(object);
            }
        }

        let next_token = match output.last_evaluated_key {
            Some(ref lek) if !lek.is_empty() => {
                let token_str = self.encode_page_token(lek, "objects", true)?;
                Some(
                    PageToken::new(token_str).map_err(|_| StorageError::CorruptItem {
                        entity: "object metadata",
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
    fn stored_object_metadata_round_trips() {
        let id = WorkflowId::new("workflow-1").expect("valid");
        let object = StoredObject {
            workflow_id: id,
            object_id: StorageRecordId::new("obj-1").expect("valid"),
            class: ObjectClass::RawInput,
            storage_key: StorageKey::new("raw/workflow-1/attachment.dat").expect("valid"),
            byte_length: 2048,
            sha256: [0xef; 32],
            media_type: "application/octet-stream".to_owned(),
            created_at: WorkflowTimestamp::from_unix_seconds(200),
        };

        let stored = StoredObjectMetadata::from_stored(&object).expect("encode");
        let json = serde_json::to_string(&stored).expect("serialize");
        let decoded: StoredObjectMetadata = serde_json::from_str(&json).expect("deserialize");

        let round_tripped = decoded.to_stored(&object.workflow_id).expect("decode");
        assert_eq!(round_tripped.object_id, object.object_id);
        assert_eq!(round_tripped.class, object.class);
        assert_eq!(round_tripped.byte_length, object.byte_length);
        assert_eq!(round_tripped.sha256, object.sha256);
        assert_eq!(round_tripped.media_type, object.media_type);
        assert_eq!(round_tripped.storage_key, object.storage_key);
    }

    #[test]
    fn stored_object_metadata_round_trips_all_classes() {
        let id = WorkflowId::new("wf").expect("valid");
        let cases = [
            (ObjectClass::RawInput, "raw/wf/obj.dat"),
            (ObjectClass::Artifact, "artifacts/wf/obj.dat"),
            (ObjectClass::SanitizedHistory, "history/wf/obj.dat"),
        ];
        for (class, key) in cases {
            let object = StoredObject {
                workflow_id: id.clone(),
                object_id: StorageRecordId::new("obj").expect("valid"),
                class,
                storage_key: StorageKey::new(key).expect("valid"),
                byte_length: 1,
                sha256: [0x11; 32],
                media_type: "text/plain".to_owned(),
                created_at: WorkflowTimestamp::from_unix_seconds(1),
            };
            let stored = StoredObjectMetadata::from_stored(&object).expect("encode");
            let decoded = stored.to_stored(&id).expect("decode");
            assert_eq!(decoded.class, class);
            assert_eq!(decoded.object_id, object.object_id);
        }
    }

    #[test]
    fn stored_object_metadata_rejects_unknown_fields() {
        let id = WorkflowId::new("wf").expect("valid");
        let object = StoredObject {
            workflow_id: id,
            object_id: StorageRecordId::new("obj").expect("valid"),
            class: ObjectClass::RawInput,
            storage_key: StorageKey::new("raw/wf/obj.dat").expect("valid"),
            byte_length: 1,
            sha256: [0x22; 32],
            media_type: "text/plain".to_owned(),
            created_at: WorkflowTimestamp::from_unix_seconds(1),
        };
        let stored = StoredObjectMetadata::from_stored(&object).expect("encode");
        let mut value = serde_json::to_value(stored).expect("serialize");
        value
            .as_object_mut()
            .expect("must be object")
            .insert("extra".to_owned(), serde_json::Value::Bool(true));
        let result: Result<StoredObjectMetadata, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }

    #[test]
    fn parse_class_rejects_invalid() {
        assert!(parse_class("invalid").is_err());
        assert!(parse_class("").is_err());
        assert_eq!(parse_class("raw").expect("valid"), ObjectClass::RawInput);
        assert_eq!(
            parse_class("artifact").expect("valid"),
            ObjectClass::Artifact
        );
        assert_eq!(
            parse_class("history").expect("valid"),
            ObjectClass::SanitizedHistory
        );
    }
}
