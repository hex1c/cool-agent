use domain::identity::{ChatId, ParticipantId};

/// Outcome of a `getChatMember` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MembershipStatus {
    /// The user is a known member of the chat.
    Member,
    /// The user is not a member or the bot cannot see them.
    NotMember,
}

/// Port for Telegram Bot API interactions needed by the operations worker.
///
/// This trait defines the minimal surface the worker requires; it contains
/// no real HTTP implementation.  Callers supply their own adapter.
pub trait TelegramBot {
    /// The concrete error type returned by the adapter.
    type Error: std::error::Error;

    /// Send a text message to a chat.
    fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), Self::Error>;

    /// Query the membership status of a user in a chat.
    fn get_chat_member(
        &self,
        chat_id: ChatId,
        user_id: ParticipantId,
    ) -> Result<MembershipStatus, Self::Error>;
}
