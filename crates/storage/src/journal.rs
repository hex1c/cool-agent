use std::collections::HashMap;

use application::external_operation::{
    AttemptClaim, BeginAttemptOutcome, CompletedAttempt, ExecutionOutcome, ExternalResourceId,
    FailureCode, JournalPreparation, JournalState, OperationFailure, OperationJournal,
    ProviderOutcome, SanitizedSummary,
};
use aws_sdk_dynamodb::operation::put_item::PutItemError;
use aws_sdk_dynamodb::operation::update_item::UpdateItemError;
use aws_sdk_dynamodb::types::AttributeValue;
use domain::{AttemptNumber, IdempotencyKey};
use serde::{Deserialize, Serialize};

use crate::confirmations::StoredOperationJournal;
use crate::dynamodb::{DynamoDbStore, StorageError, deserialize_payload, serialize_payload};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFailure {
    code: String,
    summary: String,
}

impl StoredFailure {
    fn from_domain(value: &OperationFailure) -> Self {
        Self {
            code: value.code().as_str().to_owned(),
            summary: value.summary().as_str().to_owned(),
        }
    }

    fn into_domain(self) -> Result<OperationFailure, StorageError> {
        let code = FailureCode::new(self.code).map_err(|_| corrupt("failure code"))?;
        let summary =
            SanitizedSummary::new(self.summary).map_err(|_| corrupt("failure summary"))?;
        Ok(OperationFailure::new(code, summary))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum StoredProviderOutcome {
    Accepted { resource_id: Option<String> },
    RetryableFailure { failure: StoredFailure },
    TerminalFailure { failure: StoredFailure },
    Ambiguous { failure: StoredFailure },
}

impl StoredProviderOutcome {
    fn from_domain(value: &ProviderOutcome) -> Self {
        match value {
            ProviderOutcome::Accepted { resource_id } => Self::Accepted {
                resource_id: resource_id.as_ref().map(|value| value.as_str().to_owned()),
            },
            ProviderOutcome::RetryableFailure(failure) => Self::RetryableFailure {
                failure: StoredFailure::from_domain(failure),
            },
            ProviderOutcome::TerminalFailure(failure) => Self::TerminalFailure {
                failure: StoredFailure::from_domain(failure),
            },
            ProviderOutcome::Ambiguous(failure) => Self::Ambiguous {
                failure: StoredFailure::from_domain(failure),
            },
        }
    }

    fn into_domain(self) -> Result<ProviderOutcome, StorageError> {
        match self {
            Self::Accepted { resource_id } => Ok(ProviderOutcome::Accepted {
                resource_id: resource_id
                    .map(ExternalResourceId::new)
                    .transpose()
                    .map_err(|_| corrupt("external resource id"))?,
            }),
            Self::RetryableFailure { failure } => {
                Ok(ProviderOutcome::RetryableFailure(failure.into_domain()?))
            }
            Self::TerminalFailure { failure } => {
                Ok(ProviderOutcome::TerminalFailure(failure.into_domain()?))
            }
            Self::Ambiguous { failure } => Ok(ProviderOutcome::Ambiguous(failure.into_domain()?)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAttempt {
    attempt: u8,
    outcome: StoredProviderOutcome,
    delay_before_ms: Option<u32>,
}

impl StoredAttempt {
    fn from_domain(value: &CompletedAttempt) -> Self {
        Self {
            attempt: value.attempt().get(),
            outcome: StoredProviderOutcome::from_domain(value.outcome()),
            delay_before_ms: value.delay_before_ms(),
        }
    }

    fn into_domain(self) -> Result<CompletedAttempt, StorageError> {
        Ok(CompletedAttempt::new(
            AttemptNumber::new(self.attempt).map_err(|_| corrupt("attempt number"))?,
            self.outcome.into_domain()?,
            self.delay_before_ms,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum StoredExecutionOutcome {
    Accepted {
        attempt: u8,
        resource_id: Option<String>,
    },
    TerminalFailure {
        attempt: u8,
        failure: StoredFailure,
    },
    ManualReview {
        attempt: u8,
        failure: StoredFailure,
        completed_attempts: Vec<StoredAttempt>,
    },
    Exhausted {
        completed_attempts: Vec<StoredAttempt>,
    },
}

impl StoredExecutionOutcome {
    fn from_domain(value: &ExecutionOutcome) -> Self {
        match value {
            ExecutionOutcome::Accepted {
                attempt,
                resource_id,
            } => Self::Accepted {
                attempt: attempt.get(),
                resource_id: resource_id.as_ref().map(|value| value.as_str().to_owned()),
            },
            ExecutionOutcome::TerminalFailure { attempt, failure } => Self::TerminalFailure {
                attempt: attempt.get(),
                failure: StoredFailure::from_domain(failure),
            },
            ExecutionOutcome::ManualReview {
                attempt,
                failure,
                completed_attempts,
            } => Self::ManualReview {
                attempt: attempt.get(),
                failure: StoredFailure::from_domain(failure),
                completed_attempts: completed_attempts
                    .iter()
                    .map(StoredAttempt::from_domain)
                    .collect(),
            },
            ExecutionOutcome::Exhausted { completed_attempts } => Self::Exhausted {
                completed_attempts: completed_attempts
                    .iter()
                    .map(StoredAttempt::from_domain)
                    .collect(),
            },
        }
    }

    fn into_domain(self) -> Result<ExecutionOutcome, StorageError> {
        match self {
            Self::Accepted {
                attempt,
                resource_id,
            } => Ok(ExecutionOutcome::Accepted {
                attempt: AttemptNumber::new(attempt).map_err(|_| corrupt("attempt number"))?,
                resource_id: resource_id
                    .map(ExternalResourceId::new)
                    .transpose()
                    .map_err(|_| corrupt("external resource id"))?,
            }),
            Self::TerminalFailure { attempt, failure } => Ok(ExecutionOutcome::TerminalFailure {
                attempt: AttemptNumber::new(attempt).map_err(|_| corrupt("attempt number"))?,
                failure: failure.into_domain()?,
            }),
            Self::ManualReview {
                attempt,
                failure,
                completed_attempts,
            } => Ok(ExecutionOutcome::ManualReview {
                attempt: AttemptNumber::new(attempt).map_err(|_| corrupt("attempt number"))?,
                failure: failure.into_domain()?,
                completed_attempts: decode_attempts(completed_attempts)?,
            }),
            Self::Exhausted { completed_attempts } => Ok(ExecutionOutcome::Exhausted {
                completed_attempts: decode_attempts(completed_attempts)?,
            }),
        }
    }
}

fn decode_attempts(values: Vec<StoredAttempt>) -> Result<Vec<CompletedAttempt>, StorageError> {
    values.into_iter().map(StoredAttempt::into_domain).collect()
}

fn corrupt(field: &'static str) -> StorageError {
    StorageError::CorruptItem {
        entity: "operation journal",
        field,
    }
}

fn string_attr(value: impl Into<String>) -> AttributeValue {
    AttributeValue::S(value.into())
}

fn number_attr(value: impl std::fmt::Display) -> AttributeValue {
    AttributeValue::N(format!("{value}"))
}

fn required_string<'a>(
    item: &'a HashMap<String, AttributeValue>,
    field: &'static str,
) -> Result<&'a str, StorageError> {
    item.get(field)
        .and_then(|value| value.as_s().ok())
        .map(String::as_str)
        .ok_or_else(|| corrupt(field))
}

fn required_u8(
    item: &HashMap<String, AttributeValue>,
    field: &'static str,
) -> Result<u8, StorageError> {
    required_string_number(item, field)?
        .parse()
        .map_err(|_| corrupt(field))
}

fn required_usize(
    item: &HashMap<String, AttributeValue>,
    field: &'static str,
) -> Result<usize, StorageError> {
    required_string_number(item, field)?
        .parse()
        .map_err(|_| corrupt(field))
}

fn required_string_number<'a>(
    item: &'a HashMap<String, AttributeValue>,
    field: &'static str,
) -> Result<&'a str, StorageError> {
    item.get(field)
        .and_then(|value| value.as_n().ok())
        .map(String::as_str)
        .ok_or_else(|| corrupt(field))
}

fn decode_completed(
    item: &HashMap<String, AttributeValue>,
) -> Result<Vec<CompletedAttempt>, StorageError> {
    let values = match item.get("completed_attempts") {
        None => Vec::new(),
        Some(value) => value
            .as_l()
            .map_err(|_| corrupt("completed_attempts"))?
            .clone(),
    };
    let mut completed = Vec::with_capacity(values.len());
    for value in values {
        let json = value.as_s().map_err(|_| corrupt("completed_attempts"))?;
        let stored: StoredAttempt = deserialize_payload("completed attempt", json)?;
        completed.push(stored.into_domain()?);
    }
    if completed.len() != required_usize(item, "completed_count")? {
        return Err(corrupt("completed_count"));
    }
    Ok(completed)
}

fn conditional_put(error: &PutItemError) -> bool {
    error.is_conditional_check_failed_exception()
}

fn conditional_update(error: &UpdateItemError) -> bool {
    error.is_conditional_check_failed_exception()
}

impl DynamoDbStore {
    async fn load_journal_item(
        &self,
        key: &IdempotencyKey,
    ) -> Result<Option<HashMap<String, AttributeValue>>, StorageError> {
        let (pk, sk) = crate::keys::operation_journal(key).map_err(|error| {
            StorageError::Validation(crate::dynamodb::StoreValidationError::Key(error))
        })?;
        self.client()
            .get_item()
            .table_name(self.table_name())
            .key("pk", string_attr(pk))
            .key("sk", string_attr(sk))
            .consistent_read(true)
            .send()
            .await
            .map(|output| output.item)
            .map_err(|_| StorageError::Service {
                operation: "load operation journal",
            })
    }

    fn decode_journal(
        &self,
        key: &IdempotencyKey,
        item: HashMap<String, AttributeValue>,
    ) -> Result<JournalPreparation, StorageError> {
        if required_string(&item, "entity")? != "operation_journal" {
            return Err(corrupt("entity"));
        }
        let identity: StoredOperationJournal =
            deserialize_payload("operation journal", required_string(&item, "payload")?)?;
        if &identity.operation_key != key {
            return Err(corrupt("operation key"));
        }
        let completed = decode_completed(&item)?;
        let state = required_string(&item, "state")?;
        if matches!(state, "prepared" | "retry_ready" | "attempt_started") {
            let next_attempt = required_u8(&item, "next_attempt")?;
            let expected_next = u8::try_from(completed.len())
                .ok()
                .and_then(|count| count.checked_add(1))
                .ok_or_else(|| corrupt("next_attempt"))?;
            if next_attempt != expected_next {
                return Err(corrupt("next_attempt"));
            }
        }
        match state {
            "prepared" | "retry_ready" => Ok(JournalPreparation::new(JournalState::Ready {
                completed_attempts: completed,
            })),
            "attempt_started" => {
                let attempt = AttemptNumber::new(required_u8(&item, "started_attempt")?)
                    .map_err(|_| corrupt("started_attempt"))?;
                if attempt.get() != required_u8(&item, "next_attempt")? {
                    return Err(corrupt("started_attempt"));
                }
                Ok(JournalPreparation::new(JournalState::InProgress {
                    attempt,
                    completed_attempts: completed,
                }))
            }
            "final" => {
                let stored: StoredExecutionOutcome = deserialize_payload(
                    "execution outcome",
                    required_string(&item, "final_payload")?,
                )?;
                Ok(JournalPreparation::new(JournalState::Final(
                    stored.into_domain()?,
                )))
            }
            _ => Err(corrupt("state")),
        }
    }

    async fn current_preparation(
        &self,
        key: &IdempotencyKey,
    ) -> Result<Option<JournalPreparation>, StorageError> {
        self.load_journal_item(key)
            .await?
            .map(|item| self.decode_journal(key, item))
            .transpose()
    }
}

impl OperationJournal for DynamoDbStore {
    type Error = StorageError;

    async fn prepare(&self, key: &IdempotencyKey) -> Result<JournalPreparation, Self::Error> {
        if let Some(preparation) = self.current_preparation(key).await? {
            return Ok(preparation);
        }
        let (pk, sk) = crate::keys::operation_journal(key).map_err(|error| {
            StorageError::Validation(crate::dynamodb::StoreValidationError::Key(error))
        })?;
        let identity = serialize_payload(
            "operation journal",
            &StoredOperationJournal::prepared(key.clone()),
        )?;
        let put = self
            .client()
            .put_item()
            .table_name(self.table_name())
            .item("pk", string_attr(pk))
            .item("sk", string_attr(sk))
            .item("entity", string_attr("operation_journal"))
            .item("state", string_attr("prepared"))
            .item("next_attempt", number_attr(1))
            .item("completed_count", number_attr(0))
            .item("payload", string_attr(identity))
            .condition_expression("attribute_not_exists(pk)")
            .send()
            .await;
        match put {
            Ok(_) => Ok(JournalPreparation::new(JournalState::Ready {
                completed_attempts: Vec::new(),
            })),
            Err(error) if error.as_service_error().is_some_and(conditional_put) => self
                .current_preparation(key)
                .await?
                .ok_or_else(|| corrupt("missing after prepare race")),
            Err(_) => Err(StorageError::Service {
                operation: "prepare operation journal",
            }),
        }
    }

    async fn begin_attempt(
        &self,
        key: &IdempotencyKey,
        attempt: AttemptNumber,
    ) -> Result<BeginAttemptOutcome, Self::Error> {
        let (pk, sk) = crate::keys::operation_journal(key).map_err(|error| {
            StorageError::Validation(crate::dynamodb::StoreValidationError::Key(error))
        })?;
        let claim = AttemptClaim::new(uuid::Uuid::new_v4().to_string())
            .map_err(|_| corrupt("generated claim"))?;
        let identity = serialize_payload(
            "operation journal",
            &StoredOperationJournal::prepared(key.clone()),
        )?;
        let update = self
            .client()
            .update_item()
            .table_name(self.table_name())
            .key("pk", string_attr(pk))
            .key("sk", string_attr(sk))
            .update_expression(
                "SET #state = :started, started_attempt = :attempt, claim = :claim",
            )
            .condition_expression(
                "#entity = :entity AND payload = :payload AND (#state = :prepared OR #state = :retry_ready) AND next_attempt = :attempt",
            )
            .expression_attribute_names("#state", "state")
            .expression_attribute_names("#entity", "entity")
            .expression_attribute_values(":entity", string_attr("operation_journal"))
            .expression_attribute_values(":payload", string_attr(identity))
            .expression_attribute_values(":prepared", string_attr("prepared"))
            .expression_attribute_values(":retry_ready", string_attr("retry_ready"))
            .expression_attribute_values(":started", string_attr("attempt_started"))
            .expression_attribute_values(":attempt", number_attr(attempt.get()))
            .expression_attribute_values(":claim", string_attr(claim.as_str()))
            .send()
            .await;
        match update {
            Ok(_) => Ok(BeginAttemptOutcome::Acquired(claim)),
            Err(error) if error.as_service_error().is_some_and(conditional_update) => {
                Ok(BeginAttemptOutcome::RaceLost)
            }
            Err(_) => Err(StorageError::Service {
                operation: "begin operation attempt",
            }),
        }
    }

    async fn complete_attempt(
        &self,
        key: &IdempotencyKey,
        claim: &AttemptClaim,
        attempt: &CompletedAttempt,
    ) -> Result<(), Self::Error> {
        let item = self
            .load_journal_item(key)
            .await?
            .ok_or_else(|| corrupt("missing journal"))?;
        if !matches!(
            self.decode_journal(key, item.clone())?.state,
            JournalState::InProgress { attempt: stored, .. } if stored == attempt.attempt()
        ) {
            return Err(corrupt("started attempt"));
        }
        if required_string(&item, "state")? != "attempt_started"
            || required_string(&item, "claim")? != claim.as_str()
            || required_u8(&item, "started_attempt")? != attempt.attempt().get()
        {
            return Err(corrupt("attempt claim"));
        }
        let mut completed = decode_completed(&item)?;
        let previous_count = required_usize(&item, "completed_count")?;
        completed.push(attempt.clone());
        let stored_attempt =
            serialize_payload("completed attempt", &StoredAttempt::from_domain(attempt))?;
        let completed_count = completed.len();
        let next_attempt = u16::from(attempt.attempt().get()) + 1;
        let final_outcome = match attempt.outcome() {
            ProviderOutcome::RetryableFailure(_) => None,
            ProviderOutcome::Accepted { resource_id } => Some(ExecutionOutcome::Accepted {
                attempt: attempt.attempt(),
                resource_id: resource_id.clone(),
            }),
            ProviderOutcome::TerminalFailure(failure) => Some(ExecutionOutcome::TerminalFailure {
                attempt: attempt.attempt(),
                failure: failure.clone(),
            }),
            ProviderOutcome::Ambiguous(failure) => Some(ExecutionOutcome::ManualReview {
                attempt: attempt.attempt(),
                failure: failure.clone(),
                completed_attempts: completed.clone(),
            }),
        };
        let (pk, sk) = crate::keys::operation_journal(key).map_err(|error| {
            StorageError::Validation(crate::dynamodb::StoreValidationError::Key(error))
        })?;
        let identity = serialize_payload(
            "operation journal",
            &StoredOperationJournal::prepared(key.clone()),
        )?;
        let mut builder = self
            .client()
            .update_item()
            .table_name(self.table_name())
            .key("pk", string_attr(pk))
            .key("sk", string_attr(sk))
            .condition_expression(
                "#entity = :entity AND payload = :payload AND #state = :started AND claim = :claim AND started_attempt = :attempt AND next_attempt = :attempt AND completed_count = :previous_count",
            )
            .expression_attribute_names("#entity", "entity")
            .expression_attribute_names("#state", "state")
            .expression_attribute_values(":entity", string_attr("operation_journal"))
            .expression_attribute_values(":payload", string_attr(identity))
            .expression_attribute_values(":started", string_attr("attempt_started"))
            .expression_attribute_values(":claim", string_attr(claim.as_str()))
            .expression_attribute_values(":attempt", number_attr(attempt.attempt().get()))
            .expression_attribute_values(":previous_count", number_attr(previous_count))
            .expression_attribute_values(":completed_count", number_attr(completed_count))
            .expression_attribute_values(
                ":new_attempts",
                AttributeValue::L(vec![string_attr(stored_attempt)]),
            )
            .expression_attribute_values(":empty", AttributeValue::L(Vec::new()));
        if let Some(outcome) = final_outcome {
            let final_payload = serialize_payload(
                "execution outcome",
                &StoredExecutionOutcome::from_domain(&outcome),
            )?;
            builder = builder
                .update_expression(
                    "SET #state = :final, completed_attempts = list_append(if_not_exists(completed_attempts, :empty), :new_attempts), completed_count = :completed_count, final_payload = :final_payload REMOVE started_attempt, claim",
                )
                .expression_attribute_values(":final", string_attr("final"))
                .expression_attribute_values(":final_payload", string_attr(final_payload));
        } else {
            builder = builder
                .update_expression(
                    "SET #state = :retry_ready, completed_attempts = list_append(if_not_exists(completed_attempts, :empty), :new_attempts), completed_count = :completed_count, next_attempt = :next_attempt REMOVE started_attempt, claim",
                )
                .expression_attribute_values(":retry_ready", string_attr("retry_ready"))
                .expression_attribute_values(":next_attempt", number_attr(next_attempt));
        }
        match builder.send().await {
            Ok(_) => Ok(()),
            Err(error) if error.as_service_error().is_some_and(conditional_update) => {
                Err(corrupt("attempt completion condition"))
            }
            Err(_) => Err(StorageError::Service {
                operation: "complete operation attempt",
            }),
        }
    }

    async fn record_exhausted(
        &self,
        key: &IdempotencyKey,
        completed_attempts: &[CompletedAttempt],
    ) -> Result<(), Self::Error> {
        let current = self
            .current_preparation(key)
            .await?
            .ok_or_else(|| corrupt("missing journal"))?;
        if !matches!(
            current.state,
            JournalState::Ready { completed_attempts: stored } if stored == completed_attempts
        ) {
            return Err(corrupt("exhaustion history"));
        }
        let outcome = ExecutionOutcome::Exhausted {
            completed_attempts: completed_attempts.to_vec(),
        };
        let final_payload = serialize_payload(
            "execution outcome",
            &StoredExecutionOutcome::from_domain(&outcome),
        )?;
        let (pk, sk) = crate::keys::operation_journal(key).map_err(|error| {
            StorageError::Validation(crate::dynamodb::StoreValidationError::Key(error))
        })?;
        let identity = serialize_payload(
            "operation journal",
            &StoredOperationJournal::prepared(key.clone()),
        )?;
        let result = self
            .client()
            .update_item()
            .table_name(self.table_name())
            .key("pk", string_attr(pk))
            .key("sk", string_attr(sk))
            .update_expression("SET #state = :final, final_payload = :final_payload")
            .condition_expression(
                "#entity = :entity AND payload = :payload AND (#state = :retry_ready OR #state = :prepared) AND completed_count = :completed_count AND attribute_not_exists(started_attempt)",
            )
            .expression_attribute_names("#entity", "entity")
            .expression_attribute_names("#state", "state")
            .expression_attribute_values(":entity", string_attr("operation_journal"))
            .expression_attribute_values(":payload", string_attr(identity))
            .expression_attribute_values(":retry_ready", string_attr("retry_ready"))
            .expression_attribute_values(":prepared", string_attr("prepared"))
            .expression_attribute_values(":final", string_attr("final"))
            .expression_attribute_values(":completed_count", number_attr(completed_attempts.len()))
            .expression_attribute_values(":final_payload", string_attr(final_payload))
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if error.as_service_error().is_some_and(conditional_update) => {
                Err(corrupt("exhaustion condition"))
            }
            Err(_) => Err(StorageError::Service {
                operation: "exhaust operation journal",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn failure() -> OperationFailure {
        OperationFailure::new(
            FailureCode::new("temporary").expect("valid code"),
            SanitizedSummary::new("sanitized failure").expect("valid summary"),
        )
    }

    #[test]
    fn stored_attempt_round_trips_all_outcomes() {
        let outcomes = [
            ProviderOutcome::Accepted {
                resource_id: Some(ExternalResourceId::new("resource-1").expect("valid id")),
            },
            ProviderOutcome::RetryableFailure(failure()),
            ProviderOutcome::TerminalFailure(failure()),
            ProviderOutcome::Ambiguous(failure()),
        ];
        for outcome in outcomes {
            let attempt = CompletedAttempt::new(
                AttemptNumber::new(1).expect("valid attempt"),
                outcome,
                Some(25),
            );
            let stored = StoredAttempt::from_domain(&attempt);
            let json = serde_json::to_string(&stored).expect("serialize");
            let decoded: StoredAttempt = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(
                decoded.into_domain().expect("valid stored attempt"),
                attempt
            );
        }
    }

    #[test]
    fn execution_outcomes_round_trip() {
        let attempt = AttemptNumber::new(1).expect("valid attempt");
        let values = [
            ExecutionOutcome::Accepted {
                attempt,
                resource_id: None,
            },
            ExecutionOutcome::TerminalFailure {
                attempt,
                failure: failure(),
            },
            ExecutionOutcome::ManualReview {
                attempt,
                failure: failure(),
                completed_attempts: Vec::new(),
            },
            ExecutionOutcome::Exhausted {
                completed_attempts: Vec::new(),
            },
        ];
        for value in values {
            let stored = StoredExecutionOutcome::from_domain(&value);
            let json = serde_json::to_string(&stored).expect("serialize");
            let decoded: StoredExecutionOutcome = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded.into_domain().expect("valid outcome"), value);
        }
    }

    #[test]
    fn invalid_stored_values_fail_closed() {
        let stored = StoredAttempt {
            attempt: 0,
            outcome: StoredProviderOutcome::Accepted { resource_id: None },
            delay_before_ms: None,
        };
        assert!(stored.into_domain().is_err());
        let failure = StoredFailure {
            code: "has space".to_owned(),
            summary: "safe".to_owned(),
        };
        assert!(failure.into_domain().is_err());
    }
}
