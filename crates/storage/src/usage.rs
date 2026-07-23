use std::collections::HashMap;

use application::ports::StorageRecordId;
use application::repositories::{
    ConditionalWriteOutcome, InvoiceMonth, UsageOperationClass, UsageRepository, UsageReservation,
    UsageReservationState, UsageSnapshot,
};
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::{AttributeValue, Put, TransactWriteItem, Update};
use domain::identity::WorkflowId;
use domain::workflow::WorkflowTimestamp;

use crate::dynamodb::{DynamoDbStore, StorageError};

const AGGREGATE_ENTITY: &str = "budget_aggregate";
const RESERVATION_ENTITY: &str = "budget_reservation";

fn string_attr(value: impl Into<String>) -> AttributeValue {
    AttributeValue::S(value.into())
}

fn number_attr(value: impl std::fmt::Display) -> AttributeValue {
    AttributeValue::N(format!("{value}"))
}

fn partition_key(month: &InvoiceMonth) -> String {
    format!("MONTH#{}", month.as_str())
}

fn reservation_sort_key(id: &StorageRecordId) -> String {
    format!("RESERVATION#{}", id.as_str())
}

fn required_string<'a>(
    item: &'a HashMap<String, AttributeValue>,
    entity: &'static str,
    field: &'static str,
) -> Result<&'a str, StorageError> {
    item.get(field)
        .and_then(|value| value.as_s().ok())
        .map(String::as_str)
        .ok_or(StorageError::CorruptItem { entity, field })
}

fn required_u64(
    item: &HashMap<String, AttributeValue>,
    entity: &'static str,
    field: &'static str,
) -> Result<u64, StorageError> {
    item.get(field)
        .and_then(|value| value.as_n().ok())
        .and_then(|value| value.parse().ok())
        .ok_or(StorageError::CorruptItem { entity, field })
}

fn operation_class_label(value: UsageOperationClass) -> &'static str {
    match value {
        UsageOperationClass::NewWorkflow => "new_workflow",
        UsageOperationClass::AiCall => "ai_call",
        UsageOperationClass::ExternalWrite => "external_write",
        UsageOperationClass::Retry => "retry",
        UsageOperationClass::Deployment => "deployment",
        UsageOperationClass::Status => "status",
        UsageOperationClass::Cancellation => "cancellation",
        UsageOperationClass::FailureReporting => "failure_reporting",
        UsageOperationClass::ArtifactRetrieval => "artifact_retrieval",
    }
}

fn parse_operation_class(value: &str) -> Result<UsageOperationClass, StorageError> {
    match value {
        "new_workflow" => Ok(UsageOperationClass::NewWorkflow),
        "ai_call" => Ok(UsageOperationClass::AiCall),
        "external_write" => Ok(UsageOperationClass::ExternalWrite),
        "retry" => Ok(UsageOperationClass::Retry),
        "deployment" => Ok(UsageOperationClass::Deployment),
        "status" => Ok(UsageOperationClass::Status),
        "cancellation" => Ok(UsageOperationClass::Cancellation),
        "failure_reporting" => Ok(UsageOperationClass::FailureReporting),
        "artifact_retrieval" => Ok(UsageOperationClass::ArtifactRetrieval),
        _ => Err(StorageError::CorruptItem {
            entity: RESERVATION_ENTITY,
            field: "operation_class",
        }),
    }
}

fn reservation_state_label(value: UsageReservationState) -> &'static str {
    match value {
        UsageReservationState::Reserved => "reserved",
        UsageReservationState::Settled => "settled",
        UsageReservationState::Released => "released",
        UsageReservationState::ManualReview => "manual_review",
    }
}

fn parse_reservation_state(value: &str) -> Result<UsageReservationState, StorageError> {
    match value {
        "reserved" => Ok(UsageReservationState::Reserved),
        "settled" => Ok(UsageReservationState::Settled),
        "released" => Ok(UsageReservationState::Released),
        "manual_review" => Ok(UsageReservationState::ManualReview),
        _ => Err(StorageError::CorruptItem {
            entity: RESERVATION_ENTITY,
            field: "state",
        }),
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

fn build_error(operation: &'static str, _error: impl std::fmt::Display) -> StorageError {
    StorageError::Build { operation }
}

impl DynamoDbStore {
    async fn load_usage_reservation(
        &self,
        month: &InvoiceMonth,
        reservation_id: &StorageRecordId,
    ) -> Result<Option<UsageReservation>, StorageError> {
        let output = self
            .client()
            .get_item()
            .table_name(self.table_name())
            .key("pk", string_attr(partition_key(month)))
            .key("sk", string_attr(reservation_sort_key(reservation_id)))
            .consistent_read(true)
            .send()
            .await
            .map_err(|_| StorageError::Service {
                operation: "load usage reservation",
            })?;
        output.item().map(parse_reservation).transpose()
    }
}

impl UsageRepository for DynamoDbStore {
    type Error = StorageError;

    async fn load(&self, month: &InvoiceMonth) -> Result<Option<UsageSnapshot>, StorageError> {
        let output = self
            .client()
            .get_item()
            .table_name(self.table_name())
            .key("pk", string_attr(partition_key(month)))
            .key("sk", string_attr("AGGREGATE"))
            .consistent_read(true)
            .send()
            .await
            .map_err(|_| StorageError::Service {
                operation: "load usage aggregate",
            })?;
        output.item().map(parse_snapshot).transpose()
    }

    async fn load_reservation(
        &self,
        month: &InvoiceMonth,
        reservation_id: &StorageRecordId,
    ) -> Result<Option<UsageReservation>, StorageError> {
        self.load_usage_reservation(month, reservation_id).await
    }

    async fn reserve(
        &self,
        expected_version: u64,
        reservation: &UsageReservation,
    ) -> Result<ConditionalWriteOutcome, StorageError> {
        if let Some(existing) = self
            .load_usage_reservation(&reservation.invoice_month, &reservation.reservation_id)
            .await?
        {
            return Ok(if existing == *reservation {
                ConditionalWriteOutcome::Committed
            } else {
                ConditionalWriteOutcome::Conflict
            });
        }

        let pk = partition_key(&reservation.invoice_month);
        let sk = reservation_sort_key(&reservation.reservation_id);
        let put = Put::builder()
            .table_name(self.table_name())
            .item("pk", string_attr(pk.clone()))
            .item("sk", string_attr(sk))
            .item("entity", string_attr(RESERVATION_ENTITY))
            .item(
                "reservation_id",
                string_attr(reservation.reservation_id.as_str()),
            )
            .item("workflow_id", string_attr(reservation.workflow_id.as_str()))
            .item(
                "invoice_month",
                string_attr(reservation.invoice_month.as_str()),
            )
            .item(
                "operation_class",
                string_attr(operation_class_label(reservation.operation_class)),
            )
            .item(
                "estimate_micro_inr",
                number_attr(reservation.estimate_micro_inr),
            )
            .item(
                "state",
                string_attr(reservation_state_label(reservation.state)),
            )
            .item(
                "pricing_version",
                string_attr(reservation.pricing_version.as_str()),
            )
            .item(
                "created_at",
                number_attr(reservation.created_at.as_unix_seconds()),
            )
            .condition_expression("attribute_not_exists(pk) AND attribute_not_exists(sk)")
            .build()
            .map_err(|error| build_error("usage reservation put", error))?;
        let update = Update::builder()
            .table_name(self.table_name())
            .key("pk", string_attr(pk))
            .key("sk", string_attr("AGGREGATE"))
            .update_expression(
                "SET reserved_micro_inr = reserved_micro_inr + :estimate, optimistic_version = optimistic_version + :one",
            )
            .condition_expression("optimistic_version = :expected")
            .expression_attribute_values(":estimate", number_attr(reservation.estimate_micro_inr))
            .expression_attribute_values(":one", number_attr(1))
            .expression_attribute_values(":expected", number_attr(expected_version))
            .build()
            .map_err(|error| build_error("usage aggregate reserve update", error))?;
        let outcome = self
            .client()
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().put(put).build())
            .transact_items(TransactWriteItem::builder().update(update).build())
            .send()
            .await;

        match outcome {
            Ok(_) => Ok(ConditionalWriteOutcome::Committed),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(is_conditional_conflict) =>
            {
                let existing = self
                    .load_usage_reservation(&reservation.invoice_month, &reservation.reservation_id)
                    .await?;
                Ok(if existing.as_ref() == Some(reservation) {
                    ConditionalWriteOutcome::Committed
                } else {
                    ConditionalWriteOutcome::Conflict
                })
            }
            Err(_) => Err(StorageError::Service {
                operation: "reserve usage",
            }),
        }
    }

    async fn update_reservation(
        &self,
        expected_version: u64,
        reservation: &UsageReservation,
        trusted_measured_micro_inr: Option<u64>,
    ) -> Result<ConditionalWriteOutcome, StorageError> {
        if let Some(existing) = self
            .load_usage_reservation(&reservation.invoice_month, &reservation.reservation_id)
            .await?
        {
            if existing == *reservation {
                return Ok(ConditionalWriteOutcome::Committed);
            }
            if existing.state != UsageReservationState::Reserved
                || existing.workflow_id != reservation.workflow_id
                || existing.operation_class != reservation.operation_class
                || existing.estimate_micro_inr != reservation.estimate_micro_inr
                || existing.pricing_version != reservation.pricing_version
            {
                return Ok(ConditionalWriteOutcome::Conflict);
            }
        } else {
            return Ok(ConditionalWriteOutcome::Conflict);
        }

        let pk = partition_key(&reservation.invoice_month);
        let sk = reservation_sort_key(&reservation.reservation_id);
        let reservation_update = Update::builder()
            .table_name(self.table_name())
            .key("pk", string_attr(pk.clone()))
            .key("sk", string_attr(sk))
            .update_expression("SET #state = :next_state, trusted_measured_micro_inr = :measured")
            .condition_expression("#state = :reserved AND workflow_id = :workflow_id")
            .expression_attribute_names("#state", "state")
            .expression_attribute_values(
                ":next_state",
                string_attr(reservation_state_label(reservation.state)),
            )
            .expression_attribute_values(":reserved", string_attr("reserved"))
            .expression_attribute_values(
                ":workflow_id",
                string_attr(reservation.workflow_id.as_str()),
            )
            .expression_attribute_values(
                ":measured",
                number_attr(trusted_measured_micro_inr.unwrap_or(0)),
            )
            .build()
            .map_err(|error| build_error("usage reservation state update", error))?;

        let aggregate_update = match reservation.state {
            UsageReservationState::Settled | UsageReservationState::Released => Update::builder()
                .table_name(self.table_name())
                .key("pk", string_attr(pk))
                .key("sk", string_attr("AGGREGATE"))
                .update_expression(
                    "SET reserved_micro_inr = reserved_micro_inr - :estimate, settled_micro_inr = settled_micro_inr + :measured, optimistic_version = optimistic_version + :one",
                )
                .condition_expression(
                    "optimistic_version = :expected AND reserved_micro_inr >= :estimate",
                )
                .expression_attribute_values(":estimate", number_attr(reservation.estimate_micro_inr))
                .expression_attribute_values(":measured", number_attr(trusted_measured_micro_inr.unwrap_or(0)))
                .expression_attribute_values(":one", number_attr(1))
                .expression_attribute_values(":expected", number_attr(expected_version))
                .build()
                .map_err(|error| build_error("usage aggregate settlement update", error))?,
            UsageReservationState::ManualReview => Update::builder()
                .table_name(self.table_name())
                .key("pk", string_attr(pk))
                .key("sk", string_attr("AGGREGATE"))
                .update_expression("SET optimistic_version = optimistic_version + :one")
                .condition_expression("optimistic_version = :expected")
                .expression_attribute_values(":one", number_attr(1))
                .expression_attribute_values(":expected", number_attr(expected_version))
                .build()
                .map_err(|error| build_error("usage aggregate manual-review update", error))?,
            UsageReservationState::Reserved => return Ok(ConditionalWriteOutcome::Conflict),
        };

        let outcome = self
            .client()
            .transact_write_items()
            .transact_items(
                TransactWriteItem::builder()
                    .update(reservation_update)
                    .build(),
            )
            .transact_items(
                TransactWriteItem::builder()
                    .update(aggregate_update)
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
                let existing = self
                    .load_usage_reservation(&reservation.invoice_month, &reservation.reservation_id)
                    .await?;
                Ok(if existing.as_ref() == Some(reservation) {
                    ConditionalWriteOutcome::Committed
                } else {
                    ConditionalWriteOutcome::Conflict
                })
            }
            Err(_) => Err(StorageError::Service {
                operation: "update usage reservation",
            }),
        }
    }
}

fn parse_snapshot(item: &HashMap<String, AttributeValue>) -> Result<UsageSnapshot, StorageError> {
    if required_string(item, AGGREGATE_ENTITY, "entity")? != AGGREGATE_ENTITY {
        return Err(StorageError::CorruptItem {
            entity: AGGREGATE_ENTITY,
            field: "entity",
        });
    }
    Ok(UsageSnapshot {
        invoice_month: InvoiceMonth::new(required_string(item, AGGREGATE_ENTITY, "invoice_month")?)
            .map_err(|_| StorageError::CorruptItem {
                entity: AGGREGATE_ENTITY,
                field: "invoice_month",
            })?,
        settled_micro_inr: required_u64(item, AGGREGATE_ENTITY, "settled_micro_inr")?,
        reserved_micro_inr: required_u64(item, AGGREGATE_ENTITY, "reserved_micro_inr")?,
        reconciled_micro_inr: required_u64(item, AGGREGATE_ENTITY, "reconciled_micro_inr")?,
        optimistic_version: required_u64(item, AGGREGATE_ENTITY, "optimistic_version")?,
    })
}

fn parse_reservation(
    item: &HashMap<String, AttributeValue>,
) -> Result<UsageReservation, StorageError> {
    if required_string(item, RESERVATION_ENTITY, "entity")? != RESERVATION_ENTITY {
        return Err(StorageError::CorruptItem {
            entity: RESERVATION_ENTITY,
            field: "entity",
        });
    }
    Ok(UsageReservation {
        reservation_id: StorageRecordId::new(required_string(
            item,
            RESERVATION_ENTITY,
            "reservation_id",
        )?)
        .map_err(|_| StorageError::CorruptItem {
            entity: RESERVATION_ENTITY,
            field: "reservation_id",
        })?,
        workflow_id: WorkflowId::new(required_string(item, RESERVATION_ENTITY, "workflow_id")?)
            .map_err(|_| StorageError::CorruptItem {
                entity: RESERVATION_ENTITY,
                field: "workflow_id",
            })?,
        invoice_month: InvoiceMonth::new(required_string(
            item,
            RESERVATION_ENTITY,
            "invoice_month",
        )?)
        .map_err(|_| StorageError::CorruptItem {
            entity: RESERVATION_ENTITY,
            field: "invoice_month",
        })?,
        operation_class: parse_operation_class(required_string(
            item,
            RESERVATION_ENTITY,
            "operation_class",
        )?)?,
        estimate_micro_inr: required_u64(item, RESERVATION_ENTITY, "estimate_micro_inr")?,
        state: parse_reservation_state(required_string(item, RESERVATION_ENTITY, "state")?)?,
        pricing_version: StorageRecordId::new(required_string(
            item,
            RESERVATION_ENTITY,
            "pricing_version",
        )?)
        .map_err(|_| StorageError::CorruptItem {
            entity: RESERVATION_ENTITY,
            field: "pricing_version",
        })?,
        created_at: WorkflowTimestamp::from_unix_seconds(required_u64(
            item,
            RESERVATION_ENTITY,
            "created_at",
        )?),
    })
}
