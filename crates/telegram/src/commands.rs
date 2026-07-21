use domain::routing::Route;

use crate::normalize::EventKind;

/// Workflow commands that operate on the current topic's existing workflow.
///
/// These are follow-up commands, not workflow-start commands. They do not
/// accept session names and are rejected in private chats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicCommand {
    /// Signal that all input for the current stage has been provided.
    Done,
    /// Request a status summary of the current workflow.
    Status,
    /// Submit a correction to the current preview.
    Correct(String),
    /// Confirm the current preview and execute the next stage.
    Confirm,
    /// Stop the current workflow.
    Stop,
}

/// Errors that can occur when parsing a topic command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandParseError {
    /// The event was not a `Command` kind.
    NotACommand,
    /// Topic commands are not allowed in private chats.
    PrivateChatRejected,
    /// The command is not recognized as a topic command.
    UnknownCommand { command: String },
    /// The command does not accept arguments but some were provided.
    UnexpectedArgs { command: String, provided: String },
    /// The command requires arguments but none were provided.
    MissingArgs { command: String },
}

impl core::fmt::Display for CommandParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotACommand => f.write_str("event is not a bot command"),
            Self::PrivateChatRejected => {
                f.write_str("topic commands are only available in forum topics")
            }
            Self::UnknownCommand { command } => {
                write!(f, "unknown topic command: {command}")
            }
            Self::UnexpectedArgs { command, provided } => {
                write!(
                    f,
                    "command {command} does not accept arguments, got: {provided}"
                )
            }
            Self::MissingArgs { command } => {
                write!(f, "command {command} requires a correction argument")
            }
        }
    }
}

impl std::error::Error for CommandParseError {}

/// Parse a [`TopicCommand`] from an [`EventKind::Command`].
///
/// # Errors
///
/// Returns [`CommandParseError`] when:
/// - The event is not a `Command` kind.
/// - The route is not a [`Route::ForumTopic`] (private chat rejected).
/// - The command name is unknown.
/// - Arguments are present when they should not be, or missing when required.
pub fn parse_topic_command(
    event: &EventKind,
    route: &Route,
) -> Result<TopicCommand, CommandParseError> {
    let (command, args, _from) = match event {
        EventKind::Command {
            command,
            args,
            from,
        } => (command, args, from),
        _ => return Err(CommandParseError::NotACommand),
    };

    // Topic commands only operate in forum topics.
    if !matches!(route, Route::ForumTopic { .. }) {
        return Err(CommandParseError::PrivateChatRejected);
    }

    match command.as_str() {
        "/done" => {
            if let Some(extra) = args {
                return Err(CommandParseError::UnexpectedArgs {
                    command: command.clone(),
                    provided: extra.clone(),
                });
            }
            Ok(TopicCommand::Done)
        }
        "/status" => {
            if let Some(extra) = args {
                return Err(CommandParseError::UnexpectedArgs {
                    command: command.clone(),
                    provided: extra.clone(),
                });
            }
            Ok(TopicCommand::Status)
        }
        "/correct" => match args {
            Some(correction) if !correction.trim().is_empty() => {
                Ok(TopicCommand::Correct(correction.clone()))
            }
            _ => Err(CommandParseError::MissingArgs {
                command: command.clone(),
            }),
        },
        "/confirm" => {
            if let Some(extra) = args {
                return Err(CommandParseError::UnexpectedArgs {
                    command: command.clone(),
                    provided: extra.clone(),
                });
            }
            Ok(TopicCommand::Confirm)
        }
        "/stop" => {
            if let Some(extra) = args {
                return Err(CommandParseError::UnexpectedArgs {
                    command: command.clone(),
                    provided: extra.clone(),
                });
            }
            Ok(TopicCommand::Stop)
        }
        other => Err(CommandParseError::UnknownCommand {
            command: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use domain::identity::{ChatId, MessageThreadId, ParticipantId, TopicSessionId};

    use super::*;

    fn forum_route() -> Route {
        Route::ForumTopic {
            session: TopicSessionId::new(
                ChatId::new(-1001234567890),
                MessageThreadId::new(10).expect("valid thread id"),
            ),
        }
    }

    fn private_route() -> Route {
        Route::OAuthPrivate {
            chat_id: ChatId::new(12345),
        }
    }

    fn participant() -> ParticipantId {
        ParticipantId::new(111).expect("valid participant")
    }

    fn cmd(name: &str, args: Option<&str>) -> EventKind {
        EventKind::Command {
            command: name.to_string(),
            args: args.map(String::from),
            from: participant(),
        }
    }

    #[test]
    fn done_parses_without_args() {
        let result = parse_topic_command(&cmd("/done", None), &forum_route());
        assert!(matches!(result, Ok(TopicCommand::Done)));
    }

    #[test]
    fn done_rejects_extra_args() {
        let result = parse_topic_command(&cmd("/done", Some("something")), &forum_route());
        assert!(matches!(
            result,
            Err(CommandParseError::UnexpectedArgs { .. })
        ));
    }

    #[test]
    fn status_parses_without_args() {
        let result = parse_topic_command(&cmd("/status", None), &forum_route());
        assert!(matches!(result, Ok(TopicCommand::Status)));
    }

    #[test]
    fn status_rejects_extra_args() {
        let result = parse_topic_command(&cmd("/status", Some("extra")), &forum_route());
        assert!(matches!(
            result,
            Err(CommandParseError::UnexpectedArgs { .. })
        ));
    }

    #[test]
    fn correct_requires_args() {
        let result = parse_topic_command(&cmd("/correct", Some("fix the amount")), &forum_route());
        assert!(matches!(result, Ok(TopicCommand::Correct(ref text)) if text == "fix the amount"));
    }

    #[test]
    fn correct_rejects_missing_args() {
        let result = parse_topic_command(&cmd("/correct", None), &forum_route());
        assert!(matches!(result, Err(CommandParseError::MissingArgs { .. })));
    }

    #[test]
    fn correct_rejects_empty_args() {
        let result = parse_topic_command(&cmd("/correct", Some("  ")), &forum_route());
        assert!(matches!(result, Err(CommandParseError::MissingArgs { .. })));
    }

    #[test]
    fn confirm_parses_without_args() {
        let result = parse_topic_command(&cmd("/confirm", None), &forum_route());
        assert!(matches!(result, Ok(TopicCommand::Confirm)));
    }

    #[test]
    fn confirm_rejects_extra_args() {
        let result = parse_topic_command(&cmd("/confirm", Some("now")), &forum_route());
        assert!(matches!(
            result,
            Err(CommandParseError::UnexpectedArgs { .. })
        ));
    }

    #[test]
    fn stop_parses_without_args() {
        let result = parse_topic_command(&cmd("/stop", None), &forum_route());
        assert!(matches!(result, Ok(TopicCommand::Stop)));
    }

    #[test]
    fn stop_rejects_extra_args() {
        let result = parse_topic_command(&cmd("/stop", Some("please")), &forum_route());
        assert!(matches!(
            result,
            Err(CommandParseError::UnexpectedArgs { .. })
        ));
    }

    #[test]
    fn all_commands_rejected_in_private_chat() {
        for (name, args) in [
            ("/done", None),
            ("/status", None),
            ("/correct", Some("fix")),
            ("/confirm", None),
            ("/stop", None),
        ] {
            let result = parse_topic_command(&cmd(name, args), &private_route());
            assert!(
                matches!(result, Err(CommandParseError::PrivateChatRejected)),
                "expected PrivateChatRejected for {name}, got {result:?}"
            );
        }
    }

    #[test]
    fn non_command_event_is_rejected() {
        let mention = EventKind::Mention {
            text: "hello".into(),
            from: participant(),
        };
        let result = parse_topic_command(&mention, &forum_route());
        assert!(matches!(result, Err(CommandParseError::NotACommand)));
    }

    #[test]
    fn unknown_command_is_rejected() {
        let result = parse_topic_command(&cmd("/unknown", None), &forum_route());
        assert!(matches!(
            result,
            Err(CommandParseError::UnknownCommand { .. })
        ));
    }

    #[test]
    fn command_with_session_name_arg_is_treated_as_extra_args() {
        // Session names are not accepted; any arg on /done is rejected.
        let result = parse_topic_command(&cmd("/done", Some("session-1")), &forum_route());
        assert!(matches!(
            result,
            Err(CommandParseError::UnexpectedArgs { .. })
        ));
    }
}
