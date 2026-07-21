use std::fmt::{Display, Formatter};

use crate::identity::{ChatId, MessageThreadId, TopicSessionId};

/// The kind of Telegram chat for routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatKind {
    /// A forum supergroup where topics may host exactly one workflow.
    Forum,
    /// A private chat reserved for Google OAuth configuration only.
    Private,
    /// An unsupported chat kind (group, channel, non-forum supergroup).
    Unsupported,
}

/// The routing outcome for a chat or topic.
///
/// Forum topics route to workflow-capable sessions identified by the pair
/// `(chat_id, message_thread_id)`.  Private chats are OAuth-only and must
/// never host a workflow.  Every other chat kind is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A forum topic that can host one workflow session.
    ForumTopic { session: TopicSessionId },
    /// A private chat available for OAuth onboarding and management.
    OAuthPrivate { chat_id: ChatId },
    /// A chat where workflows are not supported.
    Unsupported,
}

/// Errors returned by routing validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingError {
    /// A forum chat was provided without a `message_thread_id`.
    ForumWithoutThread { chat_id: ChatId },
}

impl Display for RoutingError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForumWithoutThread { chat_id } => {
                write!(
                    formatter,
                    "forum chat {chat_id} requires a message_thread_id for topic routing"
                )
            }
        }
    }
}

impl std::error::Error for RoutingError {}

/// Classify a chat for routing.
///
/// This is a pure function — it does not inspect Telegram state.  The caller
/// is responsible for determining `chat_kind` from the webhook payload before
/// calling [`route`].
pub fn route(
    chat_kind: ChatKind,
    chat_id: ChatId,
    message_thread_id: Option<MessageThreadId>,
) -> Result<Route, RoutingError> {
    match chat_kind {
        ChatKind::Forum => match message_thread_id {
            Some(thread_id) => Ok(Route::ForumTopic {
                session: TopicSessionId::new(chat_id, thread_id),
            }),
            None => Err(RoutingError::ForumWithoutThread { chat_id }),
        },
        ChatKind::Private => Ok(Route::OAuthPrivate { chat_id }),
        ChatKind::Unsupported => Ok(Route::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatKind, Route, RoutingError, route};
    use crate::identity::{ChatId, MessageThreadId};

    #[test]
    fn forum_with_thread_routes_to_forum_topic() {
        let result = MessageThreadId::new(42)
            .map(|thread_id| route(ChatKind::Forum, ChatId::new(-100), Some(thread_id)));
        assert!(matches!(result, Ok(Ok(Route::ForumTopic { .. }))));
    }

    #[test]
    fn forum_without_thread_is_an_error() {
        let result = route(ChatKind::Forum, ChatId::new(-100), None);
        assert!(matches!(
            result,
            Err(RoutingError::ForumWithoutThread { .. })
        ));
    }

    #[test]
    fn private_chat_routes_to_oauth_only() {
        assert!(matches!(
            route(ChatKind::Private, ChatId::new(12345), None),
            Ok(Route::OAuthPrivate { .. })
        ));
    }

    #[test]
    fn private_chat_ignores_thread_id() {
        let result = MessageThreadId::new(1)
            .map(|thread_id| route(ChatKind::Private, ChatId::new(12345), Some(thread_id)));
        assert!(matches!(result, Ok(Ok(Route::OAuthPrivate { .. }))));
    }

    #[test]
    fn unsupported_chat_kinds_are_rejected() {
        let result = MessageThreadId::new(7)
            .map(|thread_id| route(ChatKind::Unsupported, ChatId::new(-200), Some(thread_id)));
        assert!(matches!(result, Ok(Ok(Route::Unsupported))));
    }

    #[test]
    fn distinct_topics_in_same_forum_do_not_cross_associate() {
        let result = MessageThreadId::new(10).map(|t10| {
            MessageThreadId::new(20).map(|t20| {
                (
                    route(ChatKind::Forum, ChatId::new(-100), Some(t10)),
                    route(ChatKind::Forum, ChatId::new(-100), Some(t20)),
                )
            })
        });
        assert!(matches!(
            result,
            Ok(Ok((
                Ok(Route::ForumTopic { session: ref sa }),
                Ok(Route::ForumTopic { session: ref sb }),
            )))
            if sa.chat_id == sb.chat_id
                && sa.message_thread_id != sb.message_thread_id
                && sa != sb
        ));
    }

    #[test]
    fn distinct_forums_with_same_topic_id_do_not_cross_associate() {
        let result = MessageThreadId::new(42).map(|t42| {
            let a = route(ChatKind::Forum, ChatId::new(-100), Some(t42));
            let b = route(ChatKind::Forum, ChatId::new(-200), Some(t42));
            match (a, b) {
                (Ok(Route::ForumTopic { session: sa }), Ok(Route::ForumTopic { session: sb })) => {
                    Some((sa, sb))
                }
                _ => None,
            }
        });
        assert!(matches!(
            result,
            Ok(Some((ref sa, ref sb)))
            if sa.chat_id != sb.chat_id
                && sa.message_thread_id == sb.message_thread_id
                && sa != sb
        ));
    }
}
