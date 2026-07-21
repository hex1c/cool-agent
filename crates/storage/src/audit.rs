use domain::{AuthorizedActionAudit, TransitionAudit};
use serde::{Deserialize, Serialize};

/// Versioned stored audit record that is the same for every `AUDIT#<revision>`
/// item regardless of whether the transition was purely mechanical or
/// authorised by an approved participant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoredAudit {
    /// Simple transition without confirmation-bound authorisation
    /// (creation, collection, extraction, calculation, delivery, etc.).
    Transition(TransitionAudit),
    /// Transition authorised by a current approved participant
    /// (confirmation consume, correction, or cancellation).
    AuthorizedTransition {
        transition: TransitionAudit,
        authorization: AuthorizedActionAudit,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use domain::identity::{ChatId, MessageId, MessageThreadId, ParticipantId, TopicSessionId};
    use domain::{
        MembershipAuthorizationSource, WorkflowRevision, WorkflowStateKind, WorkflowTimestamp,
    };

    fn audit() -> TransitionAudit {
        TransitionAudit {
            owner: ParticipantId::new(101).expect("valid participant"),
            actor: ParticipantId::new(202).expect("valid participant"),
            source_message: MessageId::new(7).expect("valid message"),
            from: WorkflowStateKind::WaitingForConfirmation,
            to: WorkflowStateKind::SheetOrDocWriteStarted,
            old_revision: WorkflowRevision::new(8),
            new_revision: WorkflowRevision::new(9),
            timestamp: WorkflowTimestamp::from_unix_seconds(100),
        }
    }

    fn authorization_audit() -> AuthorizedActionAudit {
        AuthorizedActionAudit {
            actor: ParticipantId::new(202).expect("valid participant"),
            topic: TopicSessionId::new(
                ChatId::new(-1001),
                MessageThreadId::new(77).expect("valid thread"),
            ),
            source_message: MessageId::new(8).expect("valid message"),
            authorized_at: WorkflowTimestamp::from_unix_seconds(8),
            membership_source: MembershipAuthorizationSource::Live,
        }
    }

    #[test]
    fn transition_audit_serialization_round_trips() {
        let envelope = StoredAudit::Transition(audit());
        let json = serde_json::to_string(&envelope).expect("serialize");
        let parsed: StoredAudit = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(envelope, parsed);
        assert!(matches!(parsed, StoredAudit::Transition(_)));
    }

    #[test]
    fn authorized_transition_audit_serialization_round_trips() {
        let envelope = StoredAudit::AuthorizedTransition {
            transition: audit(),
            authorization: authorization_audit(),
        };
        let json = serde_json::to_string(&envelope).expect("serialize");
        let parsed: StoredAudit = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(envelope, parsed);
        assert!(matches!(parsed, StoredAudit::AuthorizedTransition { .. }));
    }

    #[test]
    fn unknown_variant_rejects_on_deserialization() {
        let json = r#"{"kind":"other_transition"}"#;
        let result: Result<StoredAudit, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
