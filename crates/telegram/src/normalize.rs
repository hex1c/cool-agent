use domain::identity::{ChatId, MessageId, MessageThreadId, ParticipantId};
use domain::routing::{self, ChatKind, Route, RoutingError};

/// Errors that can occur during update normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeError {
    /// The JSON body could not be parsed as a Telegram Update.
    ParseError(String),
    /// The update has no sender (`from` field missing).
    MissingSender,
    /// The chat type is not supported by this worker (group, channel, non-forum supergroup).
    UnsupportedChat { chat_id: ChatId },
    /// A forum message arrived without a `message_thread_id`.
    MissingThreadId { chat_id: ChatId },
    /// No actionable content found in the update (no message text, no callback, no media).
    EmptyUpdate,
}

impl core::fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ParseError(msg) => write!(f, "failed to parse Telegram update: {msg}"),
            Self::MissingSender => f.write_str("update has no sender (from field missing)"),
            Self::UnsupportedChat { chat_id } => {
                write!(f, "unsupported chat type for chat {chat_id}")
            }
            Self::MissingThreadId { chat_id } => {
                write!(f, "forum chat {chat_id} is missing message_thread_id")
            }
            Self::EmptyUpdate => f.write_str("update contains no actionable content"),
        }
    }
}

impl std::error::Error for NormalizeError {}

impl From<serde_json::Error> for NormalizeError {
    fn from(err: serde_json::Error) -> Self {
        Self::ParseError(err.to_string())
    }
}

impl From<RoutingError> for NormalizeError {
    fn from(err: RoutingError) -> Self {
        match err {
            RoutingError::ForumWithoutThread { chat_id } => Self::MissingThreadId { chat_id },
        }
    }
}

/// The kind of media attached to a Telegram message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaKind {
    /// A photo message (at least one photo size present).
    Photo,
    /// A document message (file attached as document).
    Document,
}

/// The classified event extracted from a Telegram update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// A message containing a bot mention (e.g. `@bot_username start ping`).
    Mention { text: String, from: ParticipantId },
    /// A reply to a bot message in a forum topic.
    Reply {
        text: String,
        from: ParticipantId,
        reply_to: MessageId,
    },
    /// A callback query from an inline keyboard button press.
    Callback {
        data: String,
        from: ParticipantId,
        callback_id: String,
    },
    /// A message with a photo or document attachment.
    Media {
        caption: Option<String>,
        from: ParticipantId,
        media_kind: MediaKind,
    },
    /// A bot command message (e.g. `/done`, `/status`).
    Command {
        command: String,
        args: Option<String>,
        from: ParticipantId,
    },
}

/// A fully normalized Telegram update ready for domain-level processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedUpdate {
    /// Telegram's monotonic update identifier used for deduplication.
    pub update_id: i64,
    /// The routing outcome for this update's chat and topic.
    pub route: Route,
    /// The classified business event.
    pub event: EventKind,
}

// ── Raw Telegram API types for deserialization ──────────────────────────

mod raw {
    use serde::Deserialize;

    #[derive(Debug, Clone, Deserialize)]
    pub struct Update {
        pub update_id: i64,
        #[serde(default)]
        pub message: Option<Message>,
        #[serde(default)]
        pub callback_query: Option<CallbackQuery>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct Message {
        pub message_id: i64,
        #[serde(default)]
        pub from: Option<User>,
        pub chat: Chat,
        #[serde(default)]
        pub message_thread_id: Option<i64>,
        #[serde(default)]
        #[allow(dead_code)]
        pub is_topic_message: Option<bool>,
        #[serde(default)]
        pub text: Option<String>,
        #[serde(default)]
        pub caption: Option<String>,
        #[serde(default)]
        pub entities: Option<Vec<MessageEntity>>,
        #[serde(default)]
        pub photo: Option<Vec<serde_json::Value>>,
        #[serde(default)]
        pub document: Option<serde_json::Value>,
        #[serde(default)]
        pub reply_to_message: Option<Box<Message>>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct Chat {
        pub id: i64,
        #[serde(rename = "type")]
        pub chat_type: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct User {
        pub id: i64,
        #[serde(default)]
        pub is_bot: bool,
        #[serde(default)]
        #[allow(dead_code)]
        pub first_name: Option<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct MessageEntity {
        #[serde(rename = "type")]
        pub entity_type: String,
        #[serde(default)]
        pub offset: Option<u32>,
        #[serde(default)]
        pub length: Option<u32>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct CallbackQuery {
        pub id: String,
        pub from: User,
        #[serde(default)]
        pub message: Option<Message>,
        #[serde(default)]
        pub data: Option<String>,
    }
}

// ── Chat type classification ────────────────────────────────────────────

fn classify_chat(chat: &raw::Chat) -> ChatKind {
    match chat.chat_type.as_str() {
        "supergroup" => ChatKind::Forum,
        "private" => ChatKind::Private,
        _ => ChatKind::Unsupported,
    }
}

fn classify_chat_from_message(msg: &raw::Message) -> ChatKind {
    classify_chat(&msg.chat)
}

// ── Entity helpers ──────────────────────────────────────────────────────

fn has_entity_of_type(entities: &[raw::MessageEntity], kind: &str) -> bool {
    entities.iter().any(|e| e.entity_type == kind)
}

fn find_command(entities: &[raw::MessageEntity], text: &str) -> Option<String> {
    for entity in entities {
        if entity.entity_type == "bot_command" {
            let offset = entity.offset.unwrap_or(0) as usize;
            let length = entity.length.unwrap_or(0) as usize;
            if let Some(cmd_slice) = text.get(offset..offset.saturating_add(length)) {
                let command = cmd_slice
                    .split_once('@')
                    .map_or(cmd_slice, |(cmd, _bot)| cmd);
                return Some(command.to_string());
            }
        }
    }
    None
}

// ── Event classification ────────────────────────────────────────────────

fn classify_message_event(msg: &raw::Message) -> Result<EventKind, NormalizeError> {
    let from = msg.from.as_ref().ok_or(NormalizeError::MissingSender)?;
    if from.is_bot {
        return Err(NormalizeError::EmptyUpdate);
    }
    let participant = ParticipantId::new(from.id).map_err(|_| NormalizeError::MissingSender)?;

    let entities: &[raw::MessageEntity] = msg.entities.as_deref().unwrap_or(&[]);

    // Commands take priority — they are explicit intents.
    if has_entity_of_type(entities, "bot_command")
        && let Some(text) = &msg.text
        && let Some(command) = find_command(entities, text)
    {
        let args = extract_command_args(text, entities);
        return Ok(EventKind::Command {
            command,
            args,
            from: participant,
        });
    }

    // Mentions (someone tagged the bot).
    if has_entity_of_type(entities, "mention")
        && let Some(text) = &msg.text
    {
        return Ok(EventKind::Mention {
            text: text.clone(),
            from: participant,
        });
    }

    // Replies to a bot message.
    if let Some(ref reply) = msg.reply_to_message {
        let reply_text = msg.text.clone().unwrap_or_default();
        let reply_to = MessageId::new(reply.message_id).map_err(|_| NormalizeError::EmptyUpdate)?;
        return Ok(EventKind::Reply {
            text: reply_text,
            from: participant,
            reply_to,
        });
    }

    // Media (photo or document).
    if msg.photo.is_some() {
        return Ok(EventKind::Media {
            caption: msg.caption.clone(),
            from: participant,
            media_kind: MediaKind::Photo,
        });
    }
    if msg.document.is_some() {
        return Ok(EventKind::Media {
            caption: msg.caption.clone(),
            from: participant,
            media_kind: MediaKind::Document,
        });
    }

    // If there's text but no recognized event type, treat as a generic
    // mention if it has text (for interaction in the topic).
    if msg.text.as_ref().is_some_and(|t| !t.is_empty()) {
        return Ok(EventKind::Mention {
            text: msg.text.clone().unwrap_or_default(),
            from: participant,
        });
    }

    Err(NormalizeError::EmptyUpdate)
}

fn classify_callback_event(cb: &raw::CallbackQuery) -> Result<EventKind, NormalizeError> {
    if cb.from.is_bot {
        return Err(NormalizeError::EmptyUpdate);
    }
    let participant = ParticipantId::new(cb.from.id).map_err(|_| NormalizeError::MissingSender)?;
    let data = cb.data.clone().unwrap_or_default();
    Ok(EventKind::Callback {
        data,
        from: participant,
        callback_id: cb.id.clone(),
    })
}

fn extract_command_args(text: &str, entities: &[raw::MessageEntity]) -> Option<String> {
    for entity in entities {
        if entity.entity_type == "bot_command" {
            let offset = entity.offset.unwrap_or(0) as usize;
            let length = entity.length.unwrap_or(0) as usize;
            let after_cmd = offset.saturating_add(length);
            let remainder = text.get(after_cmd..).unwrap_or("");
            let trimmed = remainder.trim();
            if trimmed.is_empty() {
                return None;
            }
            return Some(trimmed.to_string());
        }
    }
    None
}

// ── Public API ──────────────────────────────────────────────────────────

/// Normalize a raw Telegram webhook JSON body into a typed [`NormalizedUpdate`].
///
/// # Errors
///
/// Returns [`NormalizeError`] when:
/// - The JSON is not a valid Telegram Update.
/// - The chat type is unsupported.
/// - A forum message is missing `message_thread_id`.
/// - The update has no sender or no actionable content.
pub fn normalize(raw_body: &[u8]) -> Result<NormalizedUpdate, NormalizeError> {
    let update: raw::Update = serde_json::from_slice(raw_body)?;

    // Prefer callback_query over message when both are present.
    if let Some(ref cb) = update.callback_query {
        let chat = cb
            .message
            .as_ref()
            .map(classify_chat_from_message)
            .unwrap_or(ChatKind::Unsupported);
        let chat_id = cb
            .message
            .as_ref()
            .map(|m| ChatId::new(m.chat.id))
            .unwrap_or(ChatId::new(0));
        let thread_id = cb
            .message
            .as_ref()
            .and_then(|m| m.message_thread_id)
            .and_then(|id| MessageThreadId::new(id).ok());

        let route = routing::route(chat, chat_id, thread_id)?;
        if matches!(route, Route::Unsupported) {
            return Err(NormalizeError::UnsupportedChat { chat_id });
        }

        let event = classify_callback_event(cb)?;
        return Ok(NormalizedUpdate {
            update_id: update.update_id,
            route,
            event,
        });
    }

    if let Some(ref msg) = update.message {
        let chat = classify_chat_from_message(msg);
        let chat_id = ChatId::new(msg.chat.id);
        let thread_id = msg
            .message_thread_id
            .and_then(|id| MessageThreadId::new(id).ok());

        let route = routing::route(chat, chat_id, thread_id)?;
        if matches!(route, Route::Unsupported) {
            return Err(NormalizeError::UnsupportedChat { chat_id });
        }

        let event = classify_message_event(msg)?;
        return Ok(NormalizedUpdate {
            update_id: update.update_id,
            route,
            event,
        });
    }

    Err(NormalizeError::EmptyUpdate)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/../../tests/fixtures/telegram/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).expect("failed to read fixture")
    }

    #[test]
    fn mention_event_normalizes_correctly() {
        let json = fixture("mention");
        let result = normalize(json.as_bytes()).expect("mention normalization failed");
        assert_eq!(result.update_id, 1001);
        assert!(matches!(result.event, EventKind::Mention { .. }));
        assert!(matches!(result.route, Route::ForumTopic { .. }));
    }

    #[test]
    fn reply_event_normalizes_correctly() {
        let json = fixture("reply");
        let result = normalize(json.as_bytes()).expect("reply normalization failed");
        assert_eq!(result.update_id, 1002);
        assert!(matches!(result.event, EventKind::Reply { .. }));
        if let EventKind::Reply { reply_to, .. } = &result.event {
            assert_eq!(reply_to.get(), 10);
        }
    }

    #[test]
    fn callback_event_normalizes_correctly() {
        let json = fixture("callback");
        let result = normalize(json.as_bytes()).expect("callback normalization failed");
        assert_eq!(result.update_id, 1003);
        assert!(matches!(result.event, EventKind::Callback { .. }));
    }

    #[test]
    fn media_event_normalizes_correctly() {
        let json = fixture("media");
        let result = normalize(json.as_bytes()).expect("media normalization failed");
        assert_eq!(result.update_id, 1004);
        assert!(matches!(result.event, EventKind::Media { .. }));
    }

    #[test]
    fn command_event_normalizes_correctly() {
        let json = fixture("command");
        let result = normalize(json.as_bytes()).expect("command normalization failed");
        assert_eq!(result.update_id, 1005);
        assert!(matches!(result.event, EventKind::Command { .. }));
        if let EventKind::Command { command, args, .. } = &result.event {
            assert_eq!(command, "/done");
            assert!(args.is_none());
        }
    }

    #[test]
    fn duplicate_update_id_produces_identical_event() {
        let json = fixture("command");
        let first = normalize(json.as_bytes()).expect("first normalization failed");
        let second = normalize(json.as_bytes()).expect("second normalization failed");
        assert_eq!(first, second);
        assert_eq!(first.update_id, second.update_id);
    }

    #[test]
    fn cross_topic_reply_routes_to_message_thread_not_reply_thread() {
        // When a message is in topic 10 but replies to a message in topic 20,
        // the route must be TopicSessionId(chat=123, thread=10), not 20.
        let json = fixture("reply");
        let result = normalize(json.as_bytes()).expect("cross-topic normalization failed");
        assert!(
            matches!(&result.route, Route::ForumTopic { session }
                if session.chat_id.get() == -1001234567890
                && session.message_thread_id.get() == 10),
            "expected ForumTopic with chat=-1001234567890, thread=10, got {:?}",
            result.route
        );
    }

    #[test]
    fn unsupported_chat_type_is_rejected() {
        let json = r#"{
            "update_id": 2001,
            "message": {
                "message_id": 1,
                "from": {"id": 111, "is_bot": false, "first_name": "Test"},
                "chat": {"id": -200, "type": "group"},
                "text": "hello"
            }
        }"#;
        let result = normalize(json.as_bytes());
        assert!(matches!(
            result,
            Err(NormalizeError::UnsupportedChat { .. })
        ));
    }

    #[test]
    fn invalid_json_is_rejected() {
        let result = normalize(b"not json");
        assert!(matches!(result, Err(NormalizeError::ParseError(_))));
    }

    #[test]
    fn empty_update_is_rejected() {
        let json = r#"{"update_id": 3001}"#;
        let result = normalize(json.as_bytes());
        assert!(matches!(result, Err(NormalizeError::EmptyUpdate)));
    }

    #[test]
    fn command_with_args_extracts_remainder() {
        let json = r#"{
            "update_id": 4001,
            "message": {
                "message_id": 50,
                "from": {"id": 111, "is_bot": false, "first_name": "Test"},
                "chat": {"id": -1001234567890, "type": "supergroup"},
                "message_thread_id": 10,
                "is_topic_message": true,
                "text": "/correct fix the amount to 500",
                "entities": [{"type": "bot_command", "offset": 0, "length": 8}]
            }
        }"#;
        let result = normalize(json.as_bytes()).expect("command with args");
        assert!(
            matches!(&result.event, EventKind::Command { command, args, .. }
                if command == "/correct"
                && args.as_deref() == Some("fix the amount to 500")),
            "expected Command(/correct, fix the amount to 500), got {:?}",
            result.event
        );
    }
}
