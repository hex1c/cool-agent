use std::collections::HashMap;

use application::repositories::{ConditionalWriteOutcome, WorkflowCreation, WorkflowRepository};
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::AttributeValue;
use domain::identity::{TopicSessionId, WorkflowId};
use domain::{TransitionAudit, TransitionOutcome, Workflow, WorkflowRevision, WorkflowStateKind};

use crate::audit::StoredAudit;
use crate::dynamodb::{DynamoDbStore, StorageError, deserialize_payload, serialize_payload};

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

fn require_entity(
    item: &HashMap<String, AttributeValue>,
    expected: &'static str,
) -> Result<(), StorageError> {
    if required_string_attribute(item, expected, "entity")? == expected {
        Ok(())
    } else {
        Err(StorageError::CorruptItem {
            entity: expected,
            field: "entity",
        })
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

impl WorkflowRepository for DynamoDbStore {
    type Error = StorageError;

    async fn load(&self, workflow_id: &WorkflowId) -> Result<Option<Workflow>, StorageError> {
        let (pk, sk) = crate::keys::workflow_metadata(workflow_id).map_err(key_error)?;
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
                operation: "load workflow",
            })?;

        let item = match output.item {
            Some(item) => item,
            None => return Ok(None),
        };

        require_entity(&item, "workflow")?;
        let payload = required_string_attribute(&item, "workflow", "payload")?;
        let workflow: Workflow = deserialize_payload("workflow", payload)?;
        Ok(Some(workflow))
    }

    async fn load_by_topic(&self, topic: TopicSessionId) -> Result<Option<Workflow>, StorageError> {
        let (claim_pk, claim_sk) = crate::keys::topic_claim(topic).map_err(key_error)?;
        let claim_output = self
            .client()
            .get_item()
            .table_name(self.table_name())
            .key("pk", string_attr(claim_pk))
            .key("sk", string_attr(claim_sk))
            .consistent_read(true)
            .send()
            .await
            .map_err(|_error| StorageError::Service {
                operation: "load topic claim",
            })?;

        let claim = match claim_output.item {
            Some(item) => item,
            None => return Ok(None),
        };
        require_entity(&claim, "topic_claim")?;
        let workflow_id_value = required_string_attribute(&claim, "topic claim", "workflow_id")?;

        let workflow_id =
            WorkflowId::new(workflow_id_value).map_err(|_error| StorageError::CorruptItem {
                entity: "topic claim",
                field: "workflow_id",
            })?;

        self.load(&workflow_id).await
    }

    async fn create(
        &self,
        creation: &WorkflowCreation,
    ) -> Result<ConditionalWriteOutcome, StorageError> {
        let workflow = creation.workflow();

        let (wf_pk, wf_sk) = crate::keys::workflow_metadata(workflow.id()).map_err(key_error)?;
        let (claim_pk, claim_sk) = crate::keys::topic_claim(workflow.topic()).map_err(key_error)?;
        let (audit_pk, audit_sk) =
            crate::keys::audit(workflow.id(), WorkflowRevision::INITIAL).map_err(key_error)?;

        let wf_payload = serialize_payload("workflow", workflow)?;
        let revision_num = workflow.revision().get();

        let creation_audit = TransitionAudit {
            owner: workflow.owner(),
            actor: creation.actor(),
            source_message: creation.source_message(),
            from: WorkflowStateKind::RequestAccepted,
            to: WorkflowStateKind::RequestAccepted,
            old_revision: WorkflowRevision::INITIAL,
            new_revision: WorkflowRevision::INITIAL,
            timestamp: workflow.updated_at(),
        };
        let audit_envelope = StoredAudit::Transition(creation_audit);
        let audit_payload = serialize_payload("audit", &audit_envelope)?;

        let outcome = self
            .client()
            .transact_write_items()
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .put(
                        aws_sdk_dynamodb::types::Put::builder()
                            .table_name(self.table_name())
                            .item("pk", string_attr(wf_pk))
                            .item("sk", string_attr(wf_sk))
                            .item("entity", string_attr("workflow"))
                            .item("revision", number_attr(revision_num))
                            .item("payload", string_attr(wf_payload))
                            .condition_expression(attribute_not_exists("pk"))
                            .build()
                            .map_err(|e| ddb_build_error("workflow put", e))?,
                    )
                    .build(),
            )
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .put(
                        aws_sdk_dynamodb::types::Put::builder()
                            .table_name(self.table_name())
                            .item("pk", string_attr(claim_pk))
                            .item("sk", string_attr(claim_sk))
                            .item("entity", string_attr("topic_claim"))
                            .item("workflow_id", string_attr(workflow.id().as_str()))
                            .condition_expression(attribute_not_exists("pk"))
                            .build()
                            .map_err(|e| ddb_build_error("topic-claim put", e))?,
                    )
                    .build(),
            )
            .transact_items(
                aws_sdk_dynamodb::types::TransactWriteItem::builder()
                    .put(
                        aws_sdk_dynamodb::types::Put::builder()
                            .table_name(self.table_name())
                            .item("pk", string_attr(audit_pk))
                            .item("sk", string_attr(audit_sk))
                            .item("entity", string_attr("audit"))
                            .item("revision", number_attr(revision_num))
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
                operation: "create workflow",
            }),
        }
    }

    async fn commit_transition(
        &self,
        transition: &TransitionOutcome,
    ) -> Result<ConditionalWriteOutcome, StorageError> {
        let workflow = &transition.workflow;
        let audit = &transition.audit;
        if workflow.revision() != audit.new_revision
            || audit.old_revision.get().checked_add(1) != Some(audit.new_revision.get())
        {
            return Err(StorageError::Validation(
                crate::dynamodb::StoreValidationError::InvalidTransitionRevisions,
            ));
        }

        let (wf_pk, wf_sk) = crate::keys::workflow_metadata(workflow.id()).map_err(key_error)?;
        let (audit_pk, audit_sk) =
            crate::keys::audit(workflow.id(), audit.new_revision).map_err(key_error)?;

        let new_payload = serialize_payload("workflow", workflow)?;
        let audit_envelope = StoredAudit::Transition(audit.clone());
        let audit_payload = serialize_payload("audit", &audit_envelope)?;
        let new_revision = workflow.revision().get();
        let expected_revision = audit.old_revision.get();

        let outcome = self
            .client()
            .transact_write_items()
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
                            .map_err(|e| ddb_build_error("workflow update", e))?,
                    )
                    .build(),
            )
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
                operation: "commit workflow transition",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::HashMap;

    use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
    use aws_sdk_dynamodb::types::error::TransactionCanceledException;
    use aws_sdk_dynamodb::types::{AttributeValue, CancellationReason};

    use super::{is_conditional_conflict, require_entity, string_attr};
    use crate::dynamodb::StorageError;

    fn canceled_with(codes: &[&str]) -> TransactWriteItemsError {
        let mut builder = TransactionCanceledException::builder();
        for code in codes {
            builder =
                builder.cancellation_reasons(CancellationReason::builder().code(*code).build());
        }
        TransactWriteItemsError::TransactionCanceledException(builder.build())
    }

    #[test]
    fn only_conditional_transaction_cancellation_is_a_conflict() {
        assert!(is_conditional_conflict(&canceled_with(&[
            "None",
            "ConditionalCheckFailed",
            "None",
        ])));
        assert!(!is_conditional_conflict(&canceled_with(&[
            "ConditionalCheckFailed",
            "ValidationError",
        ])));
        assert!(!is_conditional_conflict(&canceled_with(&[
            "TransactionConflict",
        ])));
    }

    #[test]
    fn entity_discriminator_fails_closed() {
        let workflow = HashMap::from([("entity".to_owned(), string_attr("workflow"))]);
        assert!(require_entity(&workflow, "workflow").is_ok());

        let wrong = HashMap::from([("entity".to_owned(), string_attr("audit"))]);
        assert!(matches!(
            require_entity(&wrong, "workflow"),
            Err(StorageError::CorruptItem {
                entity: "workflow",
                field: "entity",
            })
        ));

        let malformed = HashMap::from([("entity".to_owned(), AttributeValue::N("1".to_owned()))]);
        assert!(require_entity(&malformed, "workflow").is_err());
    }
}
