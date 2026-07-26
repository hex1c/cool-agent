#![allow(clippy::expect_used)]

use domain::identity::{ChatId, MessageThreadId, ParticipantId, TopicSessionId};
use domain::retry::{MAX_EXTERNAL_OPERATION_ATTEMPTS, RetryPolicy};
use domain::routing::Route;
use std::cell::Cell;
use std::rc::Rc;
use telegram::client::{MembershipStatus, TelegramBot};
use telegram::commands::{CommandParseError, parse_topic_command};
use telegram::delivery::{DeliveryOutcome, PrivateDelivery, TopicDelivery};
use telegram::normalize::EventKind;

// ── Helpers ─────────────────────────────────────────────────────────────

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

fn default_retry_policy() -> RetryPolicy {
    RetryPolicy::new(MAX_EXTERNAL_OPERATION_ATTEMPTS, 10, 100, 0).expect("valid retry policy")
}

fn topic_session() -> TopicSessionId {
    TopicSessionId::new(
        ChatId::new(-1001234567890),
        MessageThreadId::new(10).expect("valid thread id"),
    )
}

// ── Mock TelegramBot ────────────────────────────────────────────────────

struct MockBot {
    call_count: Rc<Cell<u32>>,
    succeed_after_failures: u32,
}

impl MockBot {
    fn new(succeed_after_failures: u32) -> Self {
        Self {
            call_count: Rc::new(Cell::new(0)),
            succeed_after_failures,
        }
    }
}

#[derive(Debug)]
struct MockError(String);

impl core::fmt::Display for MockError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "mock: {}", self.0)
    }
}

impl std::error::Error for MockError {}

impl TelegramBot for MockBot {
    type Error = MockError;

    async fn send_message(&self, _chat_id: ChatId, _text: &str) -> Result<(), Self::Error> {
        let count = self.call_count.get();
        self.call_count.set(count + 1);
        if count < self.succeed_after_failures {
            Err(MockError(format!("fail {}", count.saturating_add(1))))
        } else {
            Ok(())
        }
    }

    async fn get_chat_member(
        &self,
        _chat_id: ChatId,
        _user_id: ParticipantId,
    ) -> Result<MembershipStatus, Self::Error> {
        Ok(MembershipStatus::Member)
    }
}

// ── Command contract tests ──────────────────────────────────────────────

mod commands {
    use super::*;

    #[test]
    fn all_five_commands_parse_in_forum_topic() {
        let cmds = [
            ("/done", None, "Done"),
            ("/status", None, "Status"),
            ("/correct", Some("fix it"), "Correct"),
            ("/confirm", None, "Confirm"),
            ("/stop", None, "Stop"),
        ];
        for (name, args, label) in &cmds {
            let result = parse_topic_command(&cmd(name, *args), &forum_route());
            assert!(
                result.is_ok(),
                "expected {label} to parse OK, got {result:?}"
            );
        }
    }

    #[test]
    fn all_commands_rejected_in_private_chat() {
        let cmds = [
            ("/done", None),
            ("/status", None),
            ("/correct", Some("correction")),
            ("/confirm", None),
            ("/stop", None),
        ];
        for (name, args) in &cmds {
            let result = parse_topic_command(&cmd(name, *args), &private_route());
            assert!(
                matches!(result, Err(CommandParseError::PrivateChatRejected)),
                "expected PrivateChatRejected for {name}, got {result:?}"
            );
        }
    }

    #[test]
    fn done_status_confirm_stop_reject_session_name_args() {
        let names = ["/done", "/status", "/confirm", "/stop"];
        for name in &names {
            let result = parse_topic_command(&cmd(name, Some("session-1")), &forum_route());
            assert!(
                matches!(result, Err(CommandParseError::UnexpectedArgs { .. })),
                "expected UnexpectedArgs for {name} with session-1, got {result:?}"
            );
        }
    }

    #[test]
    fn correct_rejects_empty_or_whitespace_args() {
        for correction in [None, Some(""), Some("   ")] {
            let result = parse_topic_command(&cmd("/correct", correction), &forum_route());
            assert!(
                matches!(result, Err(CommandParseError::MissingArgs { .. })),
                "expected MissingArgs, got {result:?}"
            );
        }
    }
}

// ── Callback contract tests ─────────────────────────────────────────────

mod callbacks {
    use domain::confirmation::{PendingConfirmation, PreviewDigest};
    use telegram::callbacks::{CallbackData, validate_callback_against_pending};

    fn zero_digest() -> serde_json::Value {
        serde_json::Value::Array(std::iter::repeat_n(serde_json::Value::from(0), 32).collect())
    }

    fn make_pending(revision: u64, digest_bytes: [u8; 32]) -> PendingConfirmation {
        let digest_val = serde_json::to_value(digest_bytes).expect("serialize [u8; 32]");
        let json = serde_json::json!({
            "confirmation_id": "conf-int",
            "workflow_id": "wf-int",
            "workflow_revision": revision,
            "owner": 42,
            "topic": {
                "chat_id": -1001234567890_i64,
                "message_thread_id": 10
            },
            "preview_digest": digest_val,
            "mutation_target": zero_digest(),
            "action": "start_sheet_or_doc_write",
            "expires_at": 2000000000_u64
        });
        serde_json::from_value(json).expect("valid pending confirmation")
    }

    fn load_fixture(name: &str) -> String {
        let path = format!(
            "{}/../../tests/fixtures/telegram/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).expect("failed to read fixture")
    }

    #[test]
    fn valid_callback_passes() {
        let digest = [0x42u8; 32];
        let pending = make_pending(7, digest);
        let data = CallbackData::new(7, &PreviewDigest::new(digest));
        let raw = data.to_callback_string().expect("serialize");
        let result = validate_callback_against_pending(&raw, &pending);
        assert!(result.is_ok());
    }

    #[test]
    fn callback_with_stale_revision_is_rejected() {
        let digest = [0x42u8; 32];
        let pending = make_pending(10, digest);
        let data = CallbackData::new(5, &PreviewDigest::new(digest));
        let raw = data.to_callback_string().expect("serialize");
        let result = validate_callback_against_pending(&raw, &pending);
        assert!(result.is_err());
    }

    #[test]
    fn callback_with_mismatched_digest_is_rejected() {
        let pending = make_pending(3, [0x11u8; 32]);
        let data = CallbackData::new(3, &PreviewDigest::new([0x22u8; 32]));
        let raw = data.to_callback_string().expect("serialize");
        let result = validate_callback_against_pending(&raw, &pending);
        assert!(result.is_err());
    }

    #[test]
    fn stale_callback_fixture_is_rejected_against_current_revision() {
        // The fixture has revision 1; validate against revision 5.
        let raw = load_fixture("callback_stale");
        let pending = make_pending(5, [0xabu8; 32]);
        let result = validate_callback_against_pending(raw.trim(), &pending);
        assert!(result.is_err());
    }

    #[test]
    fn callback_with_garbage_data_is_rejected() {
        let pending = make_pending(1, [0u8; 32]);
        let result = validate_callback_against_pending("not-json", &pending);
        assert!(result.is_err());
    }
}

// ── Delivery privacy contract tests ─────────────────────────────────────

mod delivery_privacy {
    use telegram::delivery::PrivacyViolation;

    use super::*;

    fn load_fixture(name: &str) -> String {
        let path = format!(
            "{}/../../tests/fixtures/telegram/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).expect("failed to read fixture")
    }

    #[tokio::test]
    async fn topic_delivery_refuses_oauth_access_token() {
        let bot = MockBot::new(0);
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let result = delivery.send("access_token: ya29.something").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn topic_delivery_refuses_google_oauth_url() {
        let fixture = load_fixture("oauth_payload");
        let parsed: serde_json::Value = serde_json::from_str(&fixture).expect("valid json fixture");
        let url = parsed
            .get("oauth_connect_url")
            .and_then(|v| v.as_str())
            .expect("oauth_connect_url field");

        let bot = MockBot::new(0);
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let result = delivery.send(url).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn topic_delivery_refuses_authorization_code() {
        let fixture = load_fixture("oauth_payload");
        let parsed: serde_json::Value = serde_json::from_str(&fixture).expect("valid json fixture");
        let code = parsed
            .get("authorization_code_example")
            .and_then(|v| v.as_str())
            .expect("auth code field");

        let bot = MockBot::new(0);
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let result = delivery.send(code).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn topic_delivery_refuses_oauth_command() {
        let bot = MockBot::new(0);
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let result = delivery.send("/connect_google").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn private_delivery_accepts_oauth_bearing_payloads() {
        let fixture = load_fixture("oauth_payload");
        let parsed: serde_json::Value = serde_json::from_str(&fixture).expect("valid json fixture");

        let oauth_texts = [
            parsed
                .get("oauth_connect_url")
                .and_then(|v| v.as_str())
                .expect("url"),
            parsed
                .get("access_token_example")
                .and_then(|v| v.as_str())
                .expect("token"),
            parsed
                .get("authorization_code_example")
                .and_then(|v| v.as_str())
                .expect("code"),
            "/connect_google",
            "/disconnect_google",
            "/oauth_status",
        ];

        for text in &oauth_texts {
            let bot = MockBot::new(0);
            let delivery = PrivateDelivery::new(bot, ChatId::new(12345), default_retry_policy());
            let outcome = delivery.send_oauth(text).await;
            assert!(
                matches!(outcome, DeliveryOutcome::Sent),
                "expected Sent for oauth payload, got {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn topic_delivery_sends_topic_safe_messages() {
        let topic_safe_texts = [
            "Stage 2 complete: quotation draft ready.",
            "Please review the attached preview.",
            "Workflow status: waiting for confirmation.",
            "/status",
            "/done",
            "/confirm",
            "The discount code is SUMMER2024",
        ];

        for text in &topic_safe_texts {
            let bot = MockBot::new(0);
            let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
            let outcome = delivery.send(text).await;
            assert!(
                matches!(outcome, Ok(DeliveryOutcome::Sent)),
                "expected Ok(Sent) for topic-safe text {text:?}, got {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn private_delivery_also_sends_topic_safe_messages() {
        let bot = MockBot::new(0);
        let delivery = PrivateDelivery::new(bot, ChatId::new(12345), default_retry_policy());
        let outcome = delivery.send_topic_safe("Workflow status: complete.").await;
        assert!(matches!(outcome, DeliveryOutcome::Sent));
    }

    #[tokio::test]
    async fn topic_delivery_retry_eventually_succeeds() {
        let bot = MockBot::new(2); // 0,1 fail; 2 succeeds
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let outcome = delivery.send("status update").await.expect("topic-safe");
        assert_eq!(outcome, DeliveryOutcome::Sent);
    }

    #[tokio::test]
    async fn topic_delivery_reports_terminal_failure_on_exhaustion() {
        let bot = MockBot::new(99);
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let outcome = delivery.send("update").await.expect("topic-safe");
        assert!(
            matches!(
                outcome,
                DeliveryOutcome::TerminalFailure { attempts, .. }
                if attempts == MAX_EXTERNAL_OPERATION_ATTEMPTS
            ),
            "expected TerminalFailure with {MAX_EXTERNAL_OPERATION_ATTEMPTS} attempts, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn private_delivery_retry_eventually_succeeds() {
        let bot = MockBot::new(1);
        let delivery = PrivateDelivery::new(bot, ChatId::new(12345), default_retry_policy());
        let outcome = delivery.send_oauth("/connect_google").await;
        assert_eq!(outcome, DeliveryOutcome::Sent);
    }

    #[tokio::test]
    async fn privacy_violation_is_typed_error() {
        let bot = MockBot::new(0);
        let delivery = TopicDelivery::new(bot, topic_session(), default_retry_policy());
        let result = delivery.send("Here is an access_token: ya29.xyz").await;
        assert!(matches!(result, Err(PrivacyViolation { .. })));
    }
}
