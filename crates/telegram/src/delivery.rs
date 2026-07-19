use domain::identity::{ChatId, TopicSessionId};
use domain::retry::{AttemptNumber, JitterSample, RetryDecision, RetryPolicy};

use crate::client::TelegramBot;
use crate::privacy::{PayloadClassification, classify_payload};

/// The outcome of a delivery attempt with retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The message was sent successfully.
    Sent,
    /// All retry attempts were exhausted; the delivery failed permanently.
    TerminalFailure {
        /// Number of attempts that were tried.
        attempts: u8,
        /// The `Display` representation of the last error.
        last_error_label: String,
    },
}

/// Delivers topic-safe messages to a forum topic.
///
/// Topic delivery refuses to send any OAuth-bearing payload. It uses the
/// configured [`RetryPolicy`] for up to [`domain::retry::MAX_EXTERNAL_OPERATION_ATTEMPTS`]
/// attempts.
pub struct TopicDelivery<B> {
    bot: B,
    session: TopicSessionId,
    retry_policy: RetryPolicy,
}

/// Delivers OAuth messages to a private chat.
///
/// This is the only path for OAuth URLs, authorization codes, tokens, and
/// credential-bearing error messages. Topic-safe payloads can also be
/// delivered through this channel.
pub struct PrivateDelivery<B> {
    bot: B,
    chat_id: ChatId,
    retry_policy: RetryPolicy,
}

/// Error returned when topic delivery is asked to send OAuth-bearing content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyViolation {
    classification: PayloadClassification,
}

impl core::fmt::Display for PrivacyViolation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "topic delivery refused: payload classified as {:?}",
            self.classification
        )
    }
}

impl std::error::Error for PrivacyViolation {}

impl<B: TelegramBot> TopicDelivery<B> {
    /// Create a new topic delivery handle.
    ///
    /// `retry_policy` must satisfy `max_attempts <= MAX_EXTERNAL_OPERATION_ATTEMPTS`.
    pub fn new(bot: B, session: TopicSessionId, retry_policy: RetryPolicy) -> Self {
        Self {
            bot,
            session,
            retry_policy,
        }
    }

    /// Send a topic-safe message with retry.
    ///
    /// Returns [`PrivacyViolation`] if the payload is classified as
    /// OAuth-bearing.
    pub fn send(&self, text: &str) -> Result<DeliveryOutcome, PrivacyViolation> {
        let classification = classify_payload(text);
        if matches!(classification, PayloadClassification::OAuthBearing) {
            return Err(PrivacyViolation { classification });
        }
        Ok(send_with_retry(self.retry_policy, || {
            self.bot.send_message(self.session.chat_id(), text)
        }))
    }
}

impl<B: TelegramBot> PrivateDelivery<B> {
    /// Create a new private delivery handle.
    pub fn new(bot: B, chat_id: ChatId, retry_policy: RetryPolicy) -> Self {
        Self {
            bot,
            chat_id,
            retry_policy,
        }
    }

    /// Send an OAuth-bearing message with retry.
    ///
    /// This is the only path for OAuth URLs, codes, and tokens.
    pub fn send_oauth(&self, text: &str) -> DeliveryOutcome {
        send_with_retry(self.retry_policy, || {
            self.bot.send_message(self.chat_id, text)
        })
    }

    /// Send a topic-safe message through the private channel.
    pub fn send_topic_safe(&self, text: &str) -> DeliveryOutcome {
        send_with_retry(self.retry_policy, || {
            self.bot.send_message(self.chat_id, text)
        })
    }

    /// Access the chat id this delivery targets.
    pub fn chat_id(&self) -> ChatId {
        self.chat_id
    }
}

/// Execute a fallible send operation with the configured retry policy.
///
/// Uses zero jitter (deterministic delays) so that tests are reproducible.
/// The jitter sample of 0 is always valid for `JitterSample`.
fn send_with_retry<E: std::error::Error>(
    retry_policy: RetryPolicy,
    mut send_fn: impl FnMut() -> Result<(), E>,
) -> DeliveryOutcome {
    let mut attempts: u8 = 0;

    loop {
        attempts = attempts.saturating_add(1);
        match send_fn() {
            Ok(()) => return DeliveryOutcome::Sent,
            Err(e) => {
                let last_error_label = e.to_string();
                let attempt = match AttemptNumber::new(attempts) {
                    Ok(a) => a,
                    Err(_) => {
                        return DeliveryOutcome::TerminalFailure {
                            attempts,
                            last_error_label,
                        };
                    }
                };
                // Zero jitter — always valid for JitterSample::new(0).
                let jitter = match JitterSample::new(0) {
                    Ok(j) => j,
                    Err(_) => {
                        return DeliveryOutcome::TerminalFailure {
                            attempts,
                            last_error_label,
                        };
                    }
                };
                match retry_policy.after_failure(attempt, jitter) {
                    Ok(RetryDecision::Retry { .. }) => continue,
                    _ => {
                        return DeliveryOutcome::TerminalFailure {
                            attempts,
                            last_error_label,
                        };
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::cell::Cell;
    use std::rc::Rc;

    use domain::identity::{MessageThreadId, ParticipantId};
    use domain::retry::MAX_EXTERNAL_OPERATION_ATTEMPTS;

    use crate::client::MembershipStatus;

    use super::*;

    /// A mock TelegramBot that can be programmed to succeed or fail.
    struct MockBot {
        call_count: Rc<Cell<u32>>,
        fail_count: Rc<Cell<u32>>,
        succeed_after_failures: u32,
    }

    impl MockBot {
        fn new(succeed_after_failures: u32) -> Self {
            Self {
                call_count: Rc::new(Cell::new(0)),
                fail_count: Rc::new(Cell::new(0)),
                succeed_after_failures,
            }
        }

        #[expect(dead_code)]
        fn call_count(&self) -> u32 {
            self.call_count.get()
        }
    }

    #[derive(Debug)]
    struct MockError(String);

    impl core::fmt::Display for MockError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "mock error: {}", self.0)
        }
    }

    impl std::error::Error for MockError {}

    impl TelegramBot for MockBot {
        type Error = MockError;

        fn send_message(&self, _chat_id: ChatId, _text: &str) -> Result<(), Self::Error> {
            let count = self.call_count.get();
            self.call_count.set(count + 1);
            if count < self.succeed_after_failures {
                self.fail_count.set(self.fail_count.get() + 1);
                Err(MockError(format!("attempt {}", count + 1)))
            } else {
                Ok(())
            }
        }

        fn get_chat_member(
            &self,
            _chat_id: ChatId,
            _user_id: ParticipantId,
        ) -> Result<MembershipStatus, Self::Error> {
            Ok(MembershipStatus::Member)
        }
    }

    fn default_retry_policy() -> RetryPolicy {
        RetryPolicy::new(MAX_EXTERNAL_OPERATION_ATTEMPTS, 10, 100, 0).expect("valid retry policy")
    }

    fn topic_session() -> TopicSessionId {
        TopicSessionId::new(
            ChatId::new(-1001234567890),
            MessageThreadId::new(10).expect("valid thread id"),
        )
    }

    #[test]
    fn topic_delivery_sends_topic_safe_message() {
        let bot = MockBot::new(0);
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let outcome = delivery
            .send("Stage 2 complete: quotation draft ready.")
            .expect("topic-safe");
        assert_eq!(outcome, DeliveryOutcome::Sent);
    }

    #[test]
    fn topic_delivery_refuses_oauth_bearing_payload() {
        let bot = MockBot::new(0);
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let result = delivery.send("Here is your OAuth access token: ya29.abc123");
        assert!(result.is_err());
    }

    #[test]
    fn private_delivery_sends_oauth_bearing_message() {
        let bot = MockBot::new(0);
        let delivery = PrivateDelivery::new(bot, ChatId::new(12345), default_retry_policy());
        let outcome = delivery
            .send_oauth("Your OAuth connect link: https://accounts.google.com/o/oauth2/auth?...");
        assert_eq!(outcome, DeliveryOutcome::Sent);
    }

    #[test]
    fn retry_eventually_succeeds() {
        let bot = MockBot::new(2); // succeeds on 3rd call (0,1 fail; 2 succeeds)
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let outcome = delivery.send("status update").expect("topic-safe");
        assert_eq!(outcome, DeliveryOutcome::Sent);
    }

    #[test]
    fn retry_exhausts_and_reports_terminal_failure() {
        let bot = MockBot::new(99); // never succeeds within 3 attempts
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let outcome = delivery.send("update").expect("topic-safe");
        assert!(
            matches!(outcome, DeliveryOutcome::TerminalFailure { attempts, .. } if attempts == MAX_EXTERNAL_OPERATION_ATTEMPTS)
        );
    }

    #[test]
    fn terminal_failure_carries_last_error_label() {
        let bot = MockBot::new(99);
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let outcome = delivery.send("update").expect("topic-safe");
        assert!(
            matches!(outcome, DeliveryOutcome::TerminalFailure { ref last_error_label, .. } if last_error_label.contains("mock error")),
            "expected TerminalFailure with mock error, got {outcome:?}"
        );
    }
}
