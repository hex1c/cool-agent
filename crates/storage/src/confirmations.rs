use std::collections::HashMap;

use application::repositories::{
    ConditionalWriteOutcome, ConfirmationRepository, ConsumeAndPrepareRequest,
};
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::AttributeValue;
use domain::confirmation::ConfirmationRecord;
use domain::identity::{ConfirmationId, WorkflowId};
use domain::{
    ConfirmationCorrectionOutcome, ConfirmationIssueOutcome, TransitionOutcome, WorkflowRevision,
};

use crate::audit::StoredAudit;
use crate::dynamodb::{DynamoDbStore, StorageError, deserialize_payload, serialize_payload};

// ── stored enumeration ────────────────────────────────────────────

/// Storage discriminator that includes an `Invalidated` variant absent
/// from the domain `ConfirmationStatus`.  An invalidated confirmation
/// is treated as absent by `load` because the domain can never
/// recover it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoredConfirmationStatus {
    Pending,
    Consumed,
    Invalidated,
}

impl StoredConfirmationStatus {
    #[allow(dead_code)]
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Consumed => "consumed",
            Self::Invalidated => "invalidated",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "consumed" => Some(Self::Consumed),
            "invalidated" => Some(Self::Invalidated),
            _ => None,
        }
    }
}

// ── operation-journal DTO ─────────────────────────────────────────

/// Written inside a `OP#.../META` item when an operation journal entry
/// is first created in the `prepared` state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredOperationJournal {
    /// Exact logical identity; lifecycle fields remain scalar DynamoDB
    /// attributes so there is only one source of truth for mutable state.
    pub(crate) operation_key: domain::IdempotencyKey,
}

impl StoredOperationJournal {
    pub(crate) fn prepared(key: domain::IdempotencyKey) -> Self {
        Self { operation_key: key }
    }
}

// ── helpers ────────────────────────────────────────────────────────

fn attribute_not_exists(expression: &str) -> String {
    format!("attribute_not_exists({expression})")
}

fn string_attr(value: impl Into<String>) -> AttributeValue {
    AttributeValue::S(value.into())
}

fn number_attr(value: impl std::fmt::Display) -> AttributeValue {
    AttributeValue::N(format!("{value}"))
}

fn ddb_build_error(operation: &'static str, _error: impl std::fmt::Display) -> StorageError {
    StorageError::Build { operation }
}

fn key_error(error: crate::keys::KeyError) -> StorageError {
    StorageError::Validation(crate::dynamodb::StoreValidationError::Key(error))
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

fn validate_transition(
    transition: &TransitionOutcome,
    expected_revision: WorkflowRevision,
) -> Result<(), StorageError> {
    let audit = &transition.audit;
    if audit.old_revision == expected_revision
        && transition.workflow.revision() == audit.new_revision
        && audit.old_revision.get().checked_add(1) == Some(audit.new_revision.get())
    {
        Ok(())
    } else {
        Err(StorageError::Validation(
            crate::dynamodb::StoreValidationError::InvalidTransitionRevisions,
        ))
    }
}

fn is_conditional_conflict(error: &TransactWriteItemsError) -> bool {
    let TransactWriteItemsError::TransactionCanceledException(canceled) = error else {
        return false;
    };
    let reasons = canceled.cancellation_reasons();
    !reasons.is_empty()
        && reasons
            .iter()
            .any(|reason| reason.code() == Some("ConditionalCheckFailed"))
        && reasons.iter().all(|reason| {
            matches!(
                reason.code(),
                None | Some("None" | "ConditionalCheckFailed")
            )
        })
}

// ── impl ───────────────────────────────────────────────────────────

impl ConfirmationRepository for DynamoDbStore {
    type Error = StorageError;

    async fn load(
        &self,
        workflow_id: &WorkflowId,
        confirmation_id: &ConfirmationId,
    ) -> Result<Option<ConfirmationRecord>, StorageError> {
        let (pk, sk) =
            crate::keys::confirmation(workflow_id, confirmation_id).map_err(key_error)?;
        let output = self
            .client()
            .get_item()
            .table_name(self.table_name())
            .key("pk", string_attr(pk))
            .key("sk", string_attr(sk))
            .consistent_read(true)
            .send()
            .await
            .map_err(|_error| StorageError::Service {
                operation: "load confirmation",
            })?;

        let item = match output.item {
            Some(item) => item,
            None => return Ok(None),
        };

        let entity = required_string_attribute(&item, "confirmation", "entity")?;
        if entity != "confirmation" {
            return Err(StorageError::CorruptItem {
                entity: "confirmation",
                field: "entity",
            });
        }

        if required_string_attribute(&item, "confirmation", "workflow_id")? != workflow_id.as_str()
            || required_string_attribute(&item, "confirmation", "confirmation_id")?
                != confirmation_id.as_str()
        {
            return Err(StorageError::CorruptItem {
                entity: "confirmation",
                field: "identity binding",
            });
        }

        let status = StoredConfirmationStatus::from_str(required_string_attribute(
            &item,
            "confirmation",
            "status",
        )?)
        .ok_or(StorageError::CorruptItem {
            entity: "confirmation",
            field: "status",
        })?;
        if status == StoredConfirmationStatus::Invalidated {
            return Ok(None);
        }

        let payload = required_string_attribute(&item, "confirmation", "payload")?;
        let record: ConfirmationRecord = deserialize_payload("confirmation", payload)?;
        let status_matches = matches!(
            (status, record.status()),
            (
                StoredConfirmationStatus::Pending,
                domain::ConfirmationStatus::Pending
            ) | (
                StoredConfirmationStatus::Consumed,
                domain::ConfirmationStatus::Consumed
            )
        );
        if !status_matches {
            return Err(StorageError::CorruptItem {
                entity: "confirmation",
                field: "status",
            });
        }
        Ok(Some(record))
    }

    async fn issue(
        &self,
        outcome: &ConfirmationIssueOutcome,
    ) -> Result<ConditionalWriteOutcome, StorageError> {
        validate_transition(
            &outcome.transition,
            outcome.precondition.expected_workflow_revision,
        )?;
        let workflow = &outcome.transition.workflow;
        let audit = &outcome.transition.audit;

        let (wf_pk, wf_sk) = crate::keys::workflow_metadata(workflow.id()).map_err(key_error)?;
        let (conf_pk, conf_sk) = crate::keys::confirmation(
            workflow.id(),
            &outcome.precondition.confirmation_id_must_not_exist,
        )
        .map_err(key_error)?;
        let (audit_pk, audit_sk) =
            crate::keys::audit(workflow.id(), audit.new_revision).map_err(key_error)?;

        let expected_revision = outcome.precondition.expected_workflow_revision.get();
        let new_revision = workflow.revision().get();

        let wf_payload = serialize_payload("workflow", workflow)?;
        let confirmation_payload = serialize_payload("confirmation", &outcome.confirmation)?;

        let audit_envelope = StoredAudit::Transition(outcome.transition.audit.clone());
        let audit_payload = serialize_payload("audit", &audit_envelope)?;

        let outcome = self
            .client()
            .transact_write_items()
            // 1.  Conditionally update the workflow.
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .update(
                        aws_sdk_dynamodb::types::Update::builder()
                            .table_name(self.table_name())
                            .key("pk", string_attr(wf_pk))
                            .key("sk", string_attr(wf_sk))
                            .update_expression("SET #rev = :new_rev, #payload = :new_payload")
                            .condition_expression(
                                "#rev = :expected_rev AND #entity = :expected_entity",
                            )
                            .expression_attribute_names("#rev", "revision")
                            .expression_attribute_names("#entity", "entity")
                            .expression_attribute_names("#payload", "payload")
                            .expression_attribute_values(":new_rev", number_attr(new_revision))
                            .expression_attribute_values(
                                ":expected_rev",
                                number_attr(expected_revision),
                            )
                            .expression_attribute_values(":new_payload", string_attr(wf_payload))
                            .expression_attribute_values(
                                ":expected_entity",
                                string_attr("workflow"),
                            )
                            .build()
                            .map_err(|e| ddb_build_error("issue workflow update", e))?,
                    )
                    .build(),
            )
            // 2.  Put the pending confirmation (must not already exist).
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .put(
                        aws_sdk_dynamodb::types::Put::builder()
                            .table_name(self.table_name())
                            .item("pk", string_attr(conf_pk))
                            .item("sk", string_attr(conf_sk))
                            .item("entity", string_attr("confirmation"))
                            .item("status", string_attr("pending"))
                            .item("workflow_id", string_attr(workflow.id().as_str()))
                            .item(
                                "confirmation_id",
                                string_attr(
                                    outcome.precondition.confirmation_id_must_not_exist.as_str(),
                                ),
                            )
                            .item("payload", string_attr(confirmation_payload))
                            .condition_expression(attribute_not_exists("pk"))
                            .build()
                            .map_err(|e| ddb_build_error("confirmation put", e))?,
                    )
                    .build(),
            )
            // 3.  Append the audit item.
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .put(
                        aws_sdk_dynamodb::types::Put::builder()
                            .table_name(self.table_name())
                            .item("pk", string_attr(audit_pk))
                            .item("sk", string_attr(audit_sk))
                            .item("entity", string_attr("audit"))
                            .item("revision", number_attr(audit.new_revision.get()))
                            .item("payload", string_attr(audit_payload))
                            .condition_expression(attribute_not_exists("pk"))
                            .build()
                            .map_err(|e| ddb_build_error("audit put", e))?,
                    )
                    .build(),
            )
            .send()
            .await;

        match outcome {
            Ok(_) => Ok(ConditionalWriteOutcome::Committed),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(is_conditional_conflict) =>
            {
                Ok(ConditionalWriteOutcome::Conflict)
            }
            Err(_) => Err(StorageError::Service {
                operation: "issue confirmation",
            }),
        }
    }

    async fn consume_and_prepare_operation(
        &self,
        request: &ConsumeAndPrepareRequest,
    ) -> Result<ConditionalWriteOutcome, StorageError> {
        validate_transition(
            &request.consumption.transition,
            request.consumption.precondition.expected_workflow_revision,
        )?;
        let transition = &request.consumption.transition;
        let workflow = &transition.workflow;
        let audit = &transition.audit;
        let confirmation = &request.consumption.confirmation;

        let (wf_pk, wf_sk) = crate::keys::workflow_metadata(workflow.id()).map_err(key_error)?;

        let confirmation_id = request.consumption.precondition.confirmation_id.clone();
        let (conf_pk, conf_sk) =
            crate::keys::confirmation(workflow.id(), &confirmation_id).map_err(key_error)?;

        let (audit_pk, audit_sk) =
            crate::keys::audit(workflow.id(), audit.new_revision).map_err(key_error)?;

        let (op_pk, op_sk) =
            crate::keys::operation_journal(&request.operation_key).map_err(key_error)?;

        let expected_revision = request
            .consumption
            .precondition
            .expected_workflow_revision
            .get();
        let new_revision = workflow.revision().get();
        let new_payload = serialize_payload("workflow", workflow)?;

        let confirmation_payload = serialize_payload("confirmation", confirmation)?;

        let audit_envelope = StoredAudit::AuthorizedTransition {
            transition: audit.clone(),
            authorization: request.consumption.authorization.clone(),
        };
        let audit_payload = serialize_payload("audit", &audit_envelope)?;

        let op_journal = StoredOperationJournal::prepared(request.operation_key.clone());
        let op_journal_payload = serialize_payload("operation journal", &op_journal)?;

        let outcome = self
            .client()
            .transact_write_items()
            // 1.  Conditionally update workflow.
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .update(
                        aws_sdk_dynamodb::types::Update::builder()
                            .table_name(self.table_name())
                            .key("pk", string_attr(wf_pk))
                            .key("sk", string_attr(wf_sk))
                            .update_expression("SET #rev = :new_rev, #payload = :new_payload")
                            .condition_expression(
                                "#rev = :expected_rev AND #entity = :expected_entity",
                            )
                            .expression_attribute_names("#rev", "revision")
                            .expression_attribute_names("#entity", "entity")
                            .expression_attribute_names("#payload", "payload")
                            .expression_attribute_values(":new_rev", number_attr(new_revision))
                            .expression_attribute_values(
                                ":expected_rev",
                                number_attr(expected_revision),
                            )
                            .expression_attribute_values(":new_payload", string_attr(new_payload))
                            .expression_attribute_values(
                                ":expected_entity",
                                string_attr("workflow"),
                            )
                            .build()
                            .map_err(|e| ddb_build_error("consume workflow update", e))?,
                    )
                    .build(),
            )
            // 2.  Conditionally consume the confirmation.
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .update(
                        aws_sdk_dynamodb::types::Update::builder()
                            .table_name(self.table_name())
                            .key("pk", string_attr(conf_pk))
                            .key("sk", string_attr(conf_sk))
                            .update_expression("SET #status = :new_status, #payload = :new_payload")
                            .condition_expression(
                                "#status = :expected_status AND #entity = :expected_entity AND #confirmation_id = :confirmation_id",
                            )
                            .expression_attribute_names("#status", "status")
                            .expression_attribute_names("#entity", "entity")
                            .expression_attribute_names("#payload", "payload")
                            .expression_attribute_names("#confirmation_id", "confirmation_id")
                            .expression_attribute_values(":expected_status", string_attr("pending"))
                            .expression_attribute_values(
                                ":expected_entity",
                                string_attr("confirmation"),
                            )
                            .expression_attribute_values(
                                ":confirmation_id",
                                string_attr(confirmation_id.as_str()),
                            )
                            .expression_attribute_values(":new_status", string_attr("consumed"))
                            .expression_attribute_values(
                                ":new_payload",
                                string_attr(confirmation_payload),
                            )
                            .build()
                            .map_err(|e| ddb_build_error("consume confirmation update", e))?,
                    )
                    .build(),
            )
            // 3.  Append the transition + authorization audit.
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .put(
                        aws_sdk_dynamodb::types::Put::builder()
                            .table_name(self.table_name())
                            .item("pk", string_attr(audit_pk))
                            .item("sk", string_attr(audit_sk))
                            .item("entity", string_attr("audit"))
                            .item("revision", number_attr(audit.new_revision.get()))
                            .item("payload", string_attr(audit_payload))
                            .condition_expression(attribute_not_exists("pk"))
                            .build()
                            .map_err(|e| ddb_build_error("audit put", e))?,
                    )
                    .build(),
            )
            // 4.  Create the operation journal item in prepared state.
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .put(
                        aws_sdk_dynamodb::types::Put::builder()
                            .table_name(self.table_name())
                            .item("pk", string_attr(op_pk))
                            .item("sk", string_attr(op_sk))
                            .item("entity", string_attr("operation_journal"))
                            .item("state", string_attr("prepared"))
                            .item("next_attempt", number_attr(1))
                            .item("completed_count", number_attr(0))
                            .item("payload", string_attr(op_journal_payload))
                            .condition_expression(attribute_not_exists("pk"))
                            .build()
                            .map_err(|e| ddb_build_error("op journal put", e))?,
                    )
                    .build(),
            )
            .send()
            .await;

        match outcome {
            Ok(_) => Ok(ConditionalWriteOutcome::Committed),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(is_conditional_conflict) =>
            {
                Ok(ConditionalWriteOutcome::Conflict)
            }
            Err(_) => Err(StorageError::Service {
                operation: "consume confirmation",
            }),
        }
    }

    async fn correct(
        &self,
        outcome: &ConfirmationCorrectionOutcome,
    ) -> Result<ConditionalWriteOutcome, StorageError> {
        validate_transition(
            &outcome.transition,
            outcome.precondition.expected_workflow_revision,
        )?;
        let workflow = &outcome.transition.workflow;
        let audit = &outcome.transition.audit;

        let (wf_pk, wf_sk) = crate::keys::workflow_metadata(workflow.id()).map_err(key_error)?;
        let confirmation_id = outcome.precondition.confirmation_id.clone();
        let (conf_pk, conf_sk) =
            crate::keys::confirmation(workflow.id(), &confirmation_id).map_err(key_error)?;
        let (audit_pk, audit_sk) =
            crate::keys::audit(workflow.id(), audit.new_revision).map_err(key_error)?;

        let expected_revision = outcome.precondition.expected_workflow_revision.get();
        let new_revision = workflow.revision().get();
        let wf_payload = serialize_payload("workflow", workflow)?;

        let audit_envelope = StoredAudit::AuthorizedTransition {
            transition: audit.clone(),
            authorization: outcome.authorization.clone(),
        };
        let audit_payload = serialize_payload("audit", &audit_envelope)?;

        let outcome = self
            .client()
            .transact_write_items()
            // 1.  Conditionally update workflow.
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .update(
                        aws_sdk_dynamodb::types::Update::builder()
                            .table_name(self.table_name())
                            .key("pk", string_attr(wf_pk))
                            .key("sk", string_attr(wf_sk))
                            .update_expression("SET #rev = :new_rev, #payload = :new_payload")
                            .condition_expression(
                                "#rev = :expected_rev AND #entity = :expected_entity",
                            )
                            .expression_attribute_names("#rev", "revision")
                            .expression_attribute_names("#entity", "entity")
                            .expression_attribute_names("#payload", "payload")
                            .expression_attribute_values(":new_rev", number_attr(new_revision))
                            .expression_attribute_values(
                                ":expected_rev",
                                number_attr(expected_revision),
                            )
                            .expression_attribute_values(":new_payload", string_attr(wf_payload))
                            .expression_attribute_values(
                                ":expected_entity",
                                string_attr("workflow"),
                            )
                            .build()
                            .map_err(|e| ddb_build_error("correct workflow update", e))?,
                    )
                    .build(),
            )
            // 2.  Invalidate the pending confirmation in place.
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .update(
                        aws_sdk_dynamodb::types::Update::builder()
                            .table_name(self.table_name())
                            .key("pk", string_attr(conf_pk))
                            .key("sk", string_attr(conf_sk))
                            .update_expression("SET #status = :new_status REMOVE #payload")
                            .condition_expression(
                                "#status = :expected_status AND #entity = :expected_entity AND #confirmation_id = :confirmation_id",
                            )
                            .expression_attribute_names("#status", "status")
                            .expression_attribute_names("#entity", "entity")
                            .expression_attribute_names("#confirmation_id", "confirmation_id")
                            .expression_attribute_names("#payload", "payload")
                            .expression_attribute_values(":expected_status", string_attr("pending"))
                            .expression_attribute_values(
                                ":expected_entity",
                                string_attr("confirmation"),
                            )
                            .expression_attribute_values(
                                ":confirmation_id",
                                string_attr(confirmation_id.as_str()),
                            )
                            .expression_attribute_values(":new_status", string_attr("invalidated"))
                            .build()
                            .map_err(|e| ddb_build_error("correct confirmation update", e))?,
                    )
                    .build(),
            )
            // 3.  Append the transition + authorization audit.
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .put(
                        aws_sdk_dynamodb::types::Put::builder()
                            .table_name(self.table_name())
                            .item("pk", string_attr(audit_pk))
                            .item("sk", string_attr(audit_sk))
                            .item("entity", string_attr("audit"))
                            .item("revision", number_attr(audit.new_revision.get()))
                            .item("payload", string_attr(audit_payload))
                            .condition_expression(attribute_not_exists("pk"))
                            .build()
                            .map_err(|e| ddb_build_error("audit put", e))?,
                    )
                    .build(),
            )
            .send()
            .await;

        match outcome {
            Ok(_) => Ok(ConditionalWriteOutcome::Committed),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(is_conditional_conflict) =>
            {
                Ok(ConditionalWriteOutcome::Conflict)
            }
            Err(_) => Err(StorageError::Service {
                operation: "correct confirmation",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn stored_confirmation_status_discriminates_invalidated() {
        assert_eq!(
            StoredConfirmationStatus::from_str("pending"),
            Some(StoredConfirmationStatus::Pending)
        );
        assert_eq!(
            StoredConfirmationStatus::from_str("consumed"),
            Some(StoredConfirmationStatus::Consumed)
        );
        assert_eq!(
            StoredConfirmationStatus::from_str("invalidated"),
            Some(StoredConfirmationStatus::Invalidated)
        );
        assert_eq!(StoredConfirmationStatus::from_str("unknown"), None);
        assert_eq!(StoredConfirmationStatus::from_str(""), None);
        assert_eq!(StoredConfirmationStatus::Pending.as_str(), "pending");
        assert_eq!(StoredConfirmationStatus::Consumed.as_str(), "consumed");
        assert_eq!(
            StoredConfirmationStatus::Invalidated.as_str(),
            "invalidated"
        );
    }

    #[test]
    fn prepared_operation_journal_serialization_round_trips() {
        let key = domain::IdempotencyKey::new(
            WorkflowId::new("wf").expect("valid"),
            domain::WorkflowRevision::new(8),
            domain::OperationKind::GoogleWrite,
            domain::OperationTargetFingerprint::new([7; 32]),
        );
        let journal = StoredOperationJournal::prepared(key);
        let json = serde_json::to_string(&journal).expect("serialize");
        let parsed: StoredOperationJournal = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.operation_key, journal.operation_key);
    }

    #[test]
    fn prepared_journal_rejects_unknown_fields() {
        let key = domain::IdempotencyKey::new(
            WorkflowId::new("wf").expect("valid"),
            domain::WorkflowRevision::new(8),
            domain::OperationKind::GoogleWrite,
            domain::OperationTargetFingerprint::new([7; 32]),
        );
        let mut value =
            serde_json::to_value(StoredOperationJournal::prepared(key)).expect("serialize journal");
        value
            .as_object_mut()
            .expect("journal must serialize as an object")
            .insert("extra".to_owned(), serde_json::Value::Bool(true));
        let result: Result<StoredOperationJournal, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }
}
