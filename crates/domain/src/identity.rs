use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    Empty { kind: &'static str },
    InvalidNumeric { kind: &'static str },
}

impl Display for IdentityError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty { kind } => write!(formatter, "{kind} cannot be empty"),
            Self::InvalidNumeric { kind } => write!(formatter, "{kind} must be positive"),
        }
    }
}

impl std::error::Error for IdentityError {}

macro_rules! string_identity {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(IdentityError::Empty { kind: $kind });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_identity!(WorkflowId, "workflow id");
string_identity!(AttachmentId, "attachment id");

macro_rules! positive_i64_identity {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(i64);

        impl $name {
            pub fn new(value: i64) -> Result<Self, IdentityError> {
                if value <= 0 {
                    return Err(IdentityError::InvalidNumeric { kind: $kind });
                }
                Ok(Self(value))
            }

            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = i64::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

positive_i64_identity!(MessageId, "message id");
positive_i64_identity!(ParticipantId, "participant id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChatId(i64);

impl ChatId {
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> i64 {
        self.0
    }
}

impl Display for ChatId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

positive_i64_identity!(MessageThreadId, "message thread id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopicSessionId {
    pub chat_id: ChatId,
    pub message_thread_id: MessageThreadId,
}

impl TopicSessionId {
    pub fn new(chat_id: ChatId, message_thread_id: MessageThreadId) -> Self {
        Self {
            chat_id,
            message_thread_id,
        }
    }

    pub const fn chat_id(self) -> ChatId {
        self.chat_id
    }

    pub const fn message_thread_id(self) -> MessageThreadId {
        self.message_thread_id
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttachmentId, IdentityError, MessageId, MessageThreadId, ParticipantId, WorkflowId,
    };

    #[test]
    fn identity_types_reject_empty_or_invalid_values() {
        assert!(matches!(
            WorkflowId::new(""),
            Err(IdentityError::Empty { .. })
        ));
        assert!(matches!(
            AttachmentId::new("  "),
            Err(IdentityError::Empty { .. })
        ));
        assert!(ParticipantId::new(0).is_err());
        assert!(MessageId::new(0).is_err());
        assert!(MessageThreadId::new(-1).is_err());
    }

    #[test]
    fn identity_types_keep_values_typed() {
        let workflow = WorkflowId::new("workflow-1");
        let participant = ParticipantId::new(42);
        let message = MessageId::new(7);
        assert!(matches!(workflow, Ok(ref w) if w.as_str() == "workflow-1"));
        assert!(matches!(participant, Ok(p) if p.get() == 42));
        assert!(matches!(message, Ok(m) if m.get() == 7));
    }

    #[test]
    fn identity_deserialization_rejects_invalid_values() {
        let empty: Result<WorkflowId, _> = serde_json::from_str("\"\"");
        assert!(empty.is_err());

        let whitespace: Result<AttachmentId, _> = serde_json::from_str("\"  \"");
        assert!(whitespace.is_err());

        let zero: Result<ParticipantId, _> = serde_json::from_str("0");
        assert!(zero.is_err());

        let invalid_message: Result<MessageId, _> = serde_json::from_str("-1");
        assert!(invalid_message.is_err());

        let negative: Result<MessageThreadId, _> = serde_json::from_str("-1");
        assert!(negative.is_err());
    }

    #[test]
    fn identity_deserialization_accepts_valid_values() {
        let deser_wf: Result<WorkflowId, _> = serde_json::from_str("\"valid-wf\"");
        let ctor_wf = WorkflowId::new("valid-wf");
        assert!(matches!((&deser_wf, &ctor_wf), (Ok(d), Ok(c)) if d == c));

        let deser_pid: Result<ParticipantId, _> = serde_json::from_str("42");
        let ctor_pid = ParticipantId::new(42);
        assert!(matches!((&deser_pid, &ctor_pid), (Ok(d), Ok(c)) if d == c));

        let deser_message: Result<MessageId, _> = serde_json::from_str("7");
        let ctor_message = MessageId::new(7);
        assert!(matches!(
            (&deser_message, &ctor_message),
            (Ok(deserialized), Ok(constructed)) if deserialized == constructed
        ));
    }
}
