use std::fmt::Display;

use domain::identity::{ChatId, ParticipantId};
use zeroize::Zeroize;

/// Outcome of a `getChatMember` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MembershipStatus {
    /// The user is a known member of the chat.
    Member,
    /// The user is not a member or the bot cannot see them.
    NotMember,
}

/// Port for Telegram Bot API interactions needed by the operations worker.
#[allow(async_fn_in_trait)]
pub trait TelegramBot {
    /// The concrete error type returned by the adapter.
    type Error: std::error::Error;

    /// Send a text message to a chat.
    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), Self::Error>;

    /// Query the membership status of a user in a chat.
    async fn get_chat_member(
        &self,
        chat_id: ChatId,
        user_id: ParticipantId,
    ) -> Result<MembershipStatus, Self::Error>;
}

/// Reqwest-based Telegram Bot API client.
#[derive(Clone)]
pub struct ReqwestTelegramBot {
    client: reqwest::Client,
    token: String,
    api_base: String,
}

impl ReqwestTelegramBot {
    pub fn new(token: impl Into<String>) -> Result<Self, TelegramBotError> {
        Self::with_endpoints(token, "https://api.telegram.org")
    }

    pub fn with_endpoints(
        token: impl Into<String>,
        api_base: &str,
    ) -> Result<Self, TelegramBotError> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(TelegramBotError::Configuration);
        }
        Ok(Self {
            client: reqwest::Client::new(),
            token,
            api_base: api_base.to_owned(),
        })
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_base, self.token, method)
    }
}

impl Drop for ReqwestTelegramBot {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

impl std::fmt::Debug for ReqwestTelegramBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReqwestTelegramBot")
            .field("client", &"[REDACTED]")
            .field("token", &"[REDACTED]")
            .field("api_base", &self.api_base)
            .finish()
    }
}

impl Display for ReqwestTelegramBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReqwestTelegramBot([REDACTED])")
    }
}

#[allow(async_fn_in_trait)]
impl TelegramBot for ReqwestTelegramBot {
    type Error = TelegramBotError;

    async fn send_message(&self, chat_id: ChatId, text: &str) -> Result<(), Self::Error> {
        let response = self
            .client
            .post(self.api_url("sendMessage"))
            .form(&[
                ("chat_id", chat_id.get().to_string()),
                ("text", text.to_owned()),
            ])
            .send()
            .await
            .map_err(|_| TelegramBotError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(TelegramBotError::Rejected)
        }
    }

    async fn get_chat_member(
        &self,
        chat_id: ChatId,
        user_id: ParticipantId,
    ) -> Result<MembershipStatus, Self::Error> {
        let response = self
            .client
            .post(self.api_url("getChatMember"))
            .form(&[
                ("chat_id", chat_id.get().to_string()),
                ("user_id", user_id.get().to_string()),
            ])
            .send()
            .await
            .map_err(|_| TelegramBotError::Transport)?;
        if !response.status().is_success() {
            return Err(TelegramBotError::Rejected);
        }
        let payload: serde_json::Value = response
            .json()
            .await
            .map_err(|_| TelegramBotError::InvalidResponse)?;
        let status = payload
            .get("result")
            .and_then(|r| r.get("status"))
            .and_then(|s| s.as_str());
        match status {
            Some("member" | "administrator" | "creator") => Ok(MembershipStatus::Member),
            _ => Ok(MembershipStatus::NotMember),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum TelegramBotError {
    #[error("Telegram Bot configuration is invalid")]
    Configuration,
    #[error("Telegram Bot API transport failed")]
    Transport,
    #[error("Telegram Bot API rejected the request")]
    Rejected,
    #[error("Telegram Bot API returned an invalid response")]
    InvalidResponse,
}
