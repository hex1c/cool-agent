use domain::WorkflowRevision;
use domain::identity::{TopicSessionId, WorkflowId};

/// DynamoDB partition key maximum length in bytes.
pub const MAX_PARTITION_KEY_LENGTH: usize = 2_048;

/// DynamoDB sort key maximum length in bytes.
pub const MAX_SORT_KEY_LENGTH: usize = 1_024;

/// Error returned when key construction rejects invalid inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    ContainsControl {
        component: &'static str,
    },
    TooLong {
        component: &'static str,
        length: usize,
        maximum: usize,
    },
    Empty {
        component: &'static str,
    },
}

/// Reject control characters anywhere in a key component.
fn validate_key_component(
    component: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), KeyError> {
    if value.is_empty() {
        return Err(KeyError::Empty { component });
    }
    if value.len() > maximum {
        return Err(KeyError::TooLong {
            component,
            length: value.len(),
            maximum,
        });
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(KeyError::ContainsControl { component });
    }
    Ok(())
}

/// Produce the partition key and sort key for workflow metadata.
pub fn workflow_metadata(workflow_id: &WorkflowId) -> Result<(String, String), KeyError> {
    let pk = format!("WF#{}", workflow_id);
    let sk = "META".to_owned();
    validate_key_component("workflow pk", &pk, MAX_PARTITION_KEY_LENGTH)?;
    validate_key_component("workflow sk", &sk, MAX_SORT_KEY_LENGTH)?;
    Ok((pk, sk))
}

/// Produce the partition key and sort key for the immutable topic claim.
pub fn topic_claim(topic: TopicSessionId) -> Result<(String, String), KeyError> {
    let pk = format!(
        "TOPIC#{}#{}",
        topic.chat_id.get(),
        topic.message_thread_id.get()
    );
    let sk = "WORKFLOW".to_owned();
    validate_key_component("topic pk", &pk, MAX_PARTITION_KEY_LENGTH)?;
    validate_key_component("topic sk", &sk, MAX_SORT_KEY_LENGTH)?;
    Ok((pk, sk))
}

/// Produce the partition key and sort key for an ordered transition
/// audit entry. Revision 0 is the creation audit; every subsequent
/// transition record uses its resulting revision.
pub fn audit(
    workflow_id: &WorkflowId,
    revision: WorkflowRevision,
) -> Result<(String, String), KeyError> {
    let pk = format!("WF#{}", workflow_id);
    let sk = format!("AUDIT#{:020}", revision.get());
    validate_key_component("audit pk", &pk, MAX_PARTITION_KEY_LENGTH)?;
    validate_key_component("audit sk", &sk, MAX_SORT_KEY_LENGTH)?;
    Ok((pk, sk))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use domain::identity::{ChatId, MessageThreadId, WorkflowId};

    #[test]
    fn workflow_metadata_key_is_stable() {
        let id = WorkflowId::new("workflow-17").expect("valid workflow id");
        let (pk, sk) = workflow_metadata(&id).expect("workflow key should be valid");
        assert_eq!(pk, "WF#workflow-17");
        assert_eq!(sk, "META");
    }

    #[test]
    fn topic_claim_key_is_deterministic() {
        let topic = TopicSessionId::new(
            ChatId::new(-1001234567890),
            MessageThreadId::new(42).expect("valid thread id"),
        );
        let (pk, sk) = topic_claim(topic).expect("topic key should be valid");
        assert_eq!(pk, "TOPIC#-1001234567890#42");
        assert_eq!(sk, "WORKFLOW");
    }

    #[test]
    fn audit_key_uses_zero_padded_20_digit_revision() {
        let id = WorkflowId::new("workflow-17").expect("valid workflow id");
        let (pk, sk) = audit(&id, WorkflowRevision::new(4)).expect("audit key should be valid");
        assert_eq!(pk, "WF#workflow-17");
        assert_eq!(sk, "AUDIT#00000000000000000004");
    }

    #[test]
    fn audit_key_respects_storage_key_cases_fixture() {
        let id = WorkflowId::new("workflow-17").expect("valid workflow id");
        let (_, sk) = audit(&id, WorkflowRevision::new(4)).expect("audit key should be valid");
        assert_eq!(sk, "AUDIT#00000000000000000004");
    }

    #[test]
    fn key_components_reject_control_characters_and_oversize() {
        let long_bytes = "x".repeat(MAX_PARTITION_KEY_LENGTH + 1);
        let pk = format!("WF#{}", long_bytes);
        let long = validate_key_component("audit pk", &pk, MAX_PARTITION_KEY_LENGTH);
        assert!(matches!(long, Err(KeyError::TooLong { .. })));

        let empty = validate_key_component("test", "", 100);
        assert!(matches!(empty, Err(KeyError::Empty { .. })));

        let ctrl = validate_key_component("test", "has\ncontrol", 100);
        assert!(matches!(ctrl, Err(KeyError::ContainsControl { .. })));
    }
}
