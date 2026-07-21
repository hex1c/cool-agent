#![allow(clippy::expect_used)]

use domain::routing::Route;
use telegram::normalize::{self, EventKind, NormalizeError};
use telegram::webhook::WebhookVerifier;

fn load_fixture(name: &str) -> String {
    let path = format!(
        "{}/../../tests/fixtures/telegram/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).expect("failed to read fixture")
}

// ── Webhook verification ────────────────────────────────────────────────

#[test]
fn webhook_accepts_matching_token() {
    let verifier = WebhookVerifier::new("dev-secret-token".into());
    assert!(verifier.verify(Some("dev-secret-token")).is_ok());
}

#[test]
fn webhook_rejects_wrong_token() {
    let verifier = WebhookVerifier::new("dev-secret-token".into());
    assert!(verifier.verify(Some("attacker-token")).is_err());
}

#[test]
fn webhook_rejects_missing_token() {
    let verifier = WebhookVerifier::new("dev-secret-token".into());
    assert!(verifier.verify(None).is_err());
}

// ── Fixture normalization ───────────────────────────────────────────────

#[test]
fn normalize_mention_fixture() {
    let json = load_fixture("mention");
    let result = normalize::normalize(json.as_bytes()).expect("mention should normalize");
    assert_eq!(result.update_id, 1001);
    assert!(matches!(result.event, EventKind::Mention { .. }));
    assert!(matches!(result.route, Route::ForumTopic { .. }));
}

#[test]
fn normalize_reply_fixture() {
    let json = load_fixture("reply");
    let result = normalize::normalize(json.as_bytes()).expect("reply should normalize");
    assert_eq!(result.update_id, 1002);
    assert!(
        matches!(&result.event, EventKind::Reply { reply_to, .. } if reply_to.get() == 10),
        "expected Reply event with reply_to=10, got {:?}",
        result.event
    );
}

#[test]
fn normalize_callback_fixture() {
    let json = load_fixture("callback");
    let result = normalize::normalize(json.as_bytes()).expect("callback should normalize");
    assert_eq!(result.update_id, 1003);
    assert!(
        matches!(&result.event, EventKind::Callback { data, callback_id, .. }
            if data == "confirm_quote" && callback_id == "cbk_abc123"),
        "expected Callback(confirm_quote, cbk_abc123), got {:?}",
        result.event
    );
}

#[test]
fn normalize_media_fixture() {
    let json = load_fixture("media");
    let result = normalize::normalize(json.as_bytes()).expect("media should normalize");
    assert_eq!(result.update_id, 1004);
    assert!(matches!(result.event, EventKind::Media { .. }));
}

#[test]
fn normalize_command_fixture() {
    let json = load_fixture("command");
    let result = normalize::normalize(json.as_bytes()).expect("command should normalize");
    assert_eq!(result.update_id, 1005);
    assert!(
        matches!(&result.event, EventKind::Command { command, args, .. }
            if command == "/done" && args.is_none()),
        "expected Command(/done), got {:?}",
        result.event
    );
}

// ── Deduplication ───────────────────────────────────────────────────────

#[test]
fn deduplicate_by_update_id() {
    let json = load_fixture("command");
    let first = normalize::normalize(json.as_bytes()).expect("first");
    let second = normalize::normalize(json.as_bytes()).expect("second");
    assert_eq!(first, second);
    assert_eq!(first.update_id, second.update_id);
}

#[test]
fn distinct_update_ids_produce_different_updates() {
    let mention = load_fixture("mention");
    let command = load_fixture("command");
    let m = normalize::normalize(mention.as_bytes()).expect("mention");
    let c = normalize::normalize(command.as_bytes()).expect("command");
    assert_ne!(m.update_id, c.update_id);
    assert_ne!(m, c);
}

// ── Cross-topic isolation ───────────────────────────────────────────────

#[test]
fn reply_routes_to_message_thread_not_reply_thread() {
    // The reply fixture: message is in topic 10 but replies to a bot message
    // in topic 20. The route must resolve to topic 10.
    let json = load_fixture("reply");
    let result = normalize::normalize(json.as_bytes()).expect("reply should normalize");
    assert!(
        matches!(&result.route, Route::ForumTopic { session }
            if session.chat_id.get() == -1001234567890
            && session.message_thread_id.get() == 10),
        "expected route to topic 10, got {:?}",
        result.route
    );
}

#[test]
fn cross_topic_mention_routes_to_correct_session() {
    // A mention in topic 20 must never route to topic 10's session.
    let json = r#"{
        "update_id": 5001,
        "message": {
            "message_id": 40,
            "from": {"id": 222222222, "is_bot": false, "first_name": "Participant-B"},
            "chat": {"id": -1001234567890, "type": "supergroup"},
            "date": 1717000500,
            "message_thread_id": 20,
            "is_topic_message": true,
            "text": "@novus_bot help with something",
            "entities": [{"type": "mention", "offset": 0, "length": 10}]
        }
    }"#;
    let result =
        normalize::normalize(json.as_bytes()).expect("cross-topic mention should normalize");
    assert!(
        matches!(&result.route, Route::ForumTopic { session }
            if session.message_thread_id.get() == 20),
        "cross-topic mention must route to topic 20, got {:?}",
        result.route
    );
}

// ── Rejection paths ─────────────────────────────────────────────────────

#[test]
fn unsupported_group_chat_is_rejected() {
    let json = r#"{
        "update_id": 6001,
        "message": {
            "message_id": 1,
            "from": {"id": 111111111, "is_bot": false, "first_name": "Test"},
            "chat": {"id": -200, "type": "group"},
            "text": "hello"
        }
    }"#;
    let result = normalize::normalize(json.as_bytes());
    assert!(matches!(
        result,
        Err(NormalizeError::UnsupportedChat { .. })
    ));
}

#[test]
fn channel_chat_is_rejected() {
    let json = r#"{
        "update_id": 6002,
        "message": {
            "message_id": 2,
            "chat": {"id": -300, "type": "channel"},
            "text": "broadcast"
        }
    }"#;
    let result = normalize::normalize(json.as_bytes());
    assert!(matches!(
        result,
        Err(NormalizeError::UnsupportedChat { .. })
    ));
}

#[test]
fn missing_sender_is_rejected() {
    let json = r#"{
        "update_id": 6003,
        "message": {
            "message_id": 3,
            "chat": {"id": -1001234567890, "type": "supergroup"},
            "message_thread_id": 10,
            "text": "no sender here"
        }
    }"#;
    let result = normalize::normalize(json.as_bytes());
    assert!(matches!(result, Err(NormalizeError::MissingSender)));
}

#[test]
fn bot_messages_are_skipped() {
    let json = r#"{
        "update_id": 6004,
        "message": {
            "message_id": 4,
            "from": {"id": 999999999, "is_bot": true, "first_name": "novus_bot"},
            "chat": {"id": -1001234567890, "type": "supergroup"},
            "message_thread_id": 10,
            "text": "I processed your request"
        }
    }"#;
    let result = normalize::normalize(json.as_bytes());
    assert!(matches!(result, Err(NormalizeError::EmptyUpdate)));
}

#[test]
fn private_chat_oauth_routes_correctly() {
    let json = r#"{
        "update_id": 6005,
        "message": {
            "message_id": 5,
            "from": {"id": 111111111, "is_bot": false, "first_name": "Owner-A"},
            "chat": {"id": 12345678, "type": "private"},
            "text": "/connect_google"
        }
    }"#;
    let result = normalize::normalize(json.as_bytes()).expect("private chat should normalize");
    assert!(matches!(result.route, Route::OAuthPrivate { .. }));
}
