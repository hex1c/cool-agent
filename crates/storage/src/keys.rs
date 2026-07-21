use std::fmt::{Display, Formatter};

use domain::WorkflowRevision;
use domain::identity::{ConfirmationId, TopicSessionId, WorkflowId};
use domain::{IdempotencyKey, OperationKind, OperationTargetFingerprint};

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

impl Display for KeyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid DynamoDB key: {self:?}")
    }
}

impl std::error::Error for KeyError {}

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

/// Produce the partition key and sort key for a confirmation record.
pub fn confirmation(
    workflow_id: &WorkflowId,
    confirmation_id: &ConfirmationId,
) -> Result<(String, String), KeyError> {
    let pk = format!("WF#{}", workflow_id);
    let sk = format!("CONFIRMATION#{}", confirmation_id);
    validate_key_component("confirmation pk", &pk, MAX_PARTITION_KEY_LENGTH)?;
    validate_key_component("confirmation sk", &sk, MAX_SORT_KEY_LENGTH)?;
    Ok((pk, sk))
}

/// Return the stable snake_case representation used in DynamoDB keys.
fn operation_kind_key(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::AiProviderCall => "ai_provider_call",
        OperationKind::TelegramSend => "telegram_send",
        OperationKind::GoogleWrite => "google_write",
        OperationKind::SmtpSend => "smtp_send",
        OperationKind::S3Write => "s3_write",
        OperationKind::PdfRender => "pdf_render",
    }
}

/// Lowercase hexadecimal encoding of 32 bytes.
fn target_hex(target: &OperationTargetFingerprint) -> String {
    let bytes = target.as_bytes();
    let mut hex = String::with_capacity(64);
    for byte in bytes {
        let hi = HEX_CHARS
            .get(usize::from(byte >> 4))
            .copied()
            .unwrap_or('0');
        let lo = HEX_CHARS
            .get(usize::from(byte & 0x0F))
            .copied()
            .unwrap_or('0');
        hex.push(hi);
        hex.push(lo);
    }
    hex
}

const HEX_CHARS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// Produce the partition key and sort key for an external-operation
/// journal item. The operation kind uses stable snake_case and the
/// target fingerprint uses lowercase hexadecimal.
pub fn operation_journal(key: &IdempotencyKey) -> Result<(String, String), KeyError> {
    let kind_str = operation_kind_key(key.operation_kind());
    let target_str = target_hex(&key.target());
    let pk = format!(
        "OP#{}#{:020}#{}#{}",
        key.workflow_id(),
        key.workflow_revision().get(),
        kind_str,
        target_str
    );
    let sk = "META".to_owned();
    validate_key_component("operation pk", &pk, MAX_PARTITION_KEY_LENGTH)?;
    validate_key_component("operation sk", &sk, MAX_SORT_KEY_LENGTH)?;
    Ok((pk, sk))
}
#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use domain::identity::{ChatId, ConfirmationId, MessageThreadId, WorkflowId};
    use domain::{OperationKind, OperationTargetFingerprint, WorkflowRevision};

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

    #[test]
    fn confirmation_key_is_stable() {
        let wf = WorkflowId::new("workflow-17").expect("valid workflow id");
        let cid = ConfirmationId::new("confirmation-4").expect("valid confirmation id");
        let (pk, sk) = confirmation(&wf, &cid).expect("confirmation key should be valid");
        assert_eq!(pk, "WF#workflow-17");
        assert_eq!(sk, "CONFIRMATION#confirmation-4");
    }

    #[test]
    fn operation_journal_key_uses_stable_kind_encoding_and_lowercase_hex() {
        let key = IdempotencyKey::new(
            WorkflowId::new("workflow-17").expect("valid workflow id"),
            WorkflowRevision::new(4),
            OperationKind::GoogleWrite,
            OperationTargetFingerprint::new([
                0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
                0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
                0x01, 0x01, 0x01, 0x01,
            ]),
        );
        let (pk, sk) = operation_journal(&key).expect("operation key should be valid");
        let expected_target = "0101010101010101010101010101010101010101010101010101010101010101";
        assert_eq!(
            pk,
            format!("OP#workflow-17#00000000000000000004#google_write#{expected_target}")
        );
        assert_eq!(sk, "META");
    }

    #[test]
    fn operation_journal_key_respects_storage_key_cases_fixture() {
        let key = IdempotencyKey::new(
            WorkflowId::new("workflow-17").expect("valid workflow id"),
            WorkflowRevision::new(4),
            OperationKind::GoogleWrite,
            OperationTargetFingerprint::new([
                0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
                0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
                0x01, 0x01, 0x01, 0x01,
            ]),
        );
        let (pk, _) = operation_journal(&key).expect("operation key should be valid");
        let expected = "OP#workflow-17#00000000000000000004#google_write#0101010101010101010101010101010101010101010101010101010101010101";
        assert_eq!(pk, expected);
    }

    #[test]
    fn all_operation_kinds_have_explicit_key_encoding() {
        for kind in [
            OperationKind::AiProviderCall,
            OperationKind::TelegramSend,
            OperationKind::GoogleWrite,
            OperationKind::SmtpSend,
            OperationKind::S3Write,
            OperationKind::PdfRender,
        ] {
            let encoded = operation_kind_key(kind);
            assert!(!encoded.is_empty());
            assert!(
                encoded
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            );
        }
    }
}
