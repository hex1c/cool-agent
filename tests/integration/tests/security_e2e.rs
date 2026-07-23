#![deny(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    dead_code
)]

//! End-to-end security slice (Task 42).
//!
//! Orchestrates cross-cutting security invariants across crates: malformed
//! documents are rejected by normalization before AI use, stale callback
//! revisions are rejected before any provider write, removed participants fail
//! authorization, and unsupported chat types are rejected at intake.
//! No AWS credentials or Docker required.

use std::path::PathBuf;

use application::config::NormalizationConfig;
use application::normalization::{self, NormalizationError};
use domain::authorization::{
    AuthorizationError, LiveMembershipEvidence, MembershipStatus, authorize_participant,
};
use domain::confirmation::PendingConfirmation;
use domain::identity::{ChatId, ParticipantId};
use domain::workflow::WorkflowTimestamp;
use telegram::callbacks::{self, CallbackData, CallbackValidationError};
use telegram::normalize::{self as tg_normalize, NormalizeError};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn telegram_fixture(name: &str) -> String {
    let path = repo_root()
        .join("tests/fixtures/telegram")
        .join(format!("{name}.json"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("fixture {name} must exist at {}", path.display()))
}

fn default_normalization_config() -> NormalizationConfig {
    NormalizationConfig {
        max_image_pixels: 100_000_000,
        max_pdf_pages: 50,
        max_decompressed_bytes: 200_000_000,
        max_normalized_text_bytes: 1_000_000,
        max_csv_rows: 10_000,
        max_csv_cells: 100_000,
        max_office_uncompressed_bytes: 200_000_000,
        max_compression_ratio: 200,
    }
}

fn build_pdf_with_stream(length: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"%PDF-1.4\n");
    buf.extend_from_slice(
        format!("1 0 obj\n<</Length {length} /Filter /FlateDecode>>\nstream\n").as_bytes(),
    );
    buf.push(b'x');
    buf.extend_from_slice(b"\nendstream\nendobj\n");
    buf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    buf.extend_from_slice(b"trailer\n<</Size 2>>\nstartxref\n0\n%%EOF\n");
    buf
}

// ── Malformed documents rejected before AI use ────────────────────────────

#[test]
fn malformed_pdf_rejected_by_normalization() {
    let bytes = b"this is not a pdf despite the claim";
    let err = normalization::normalize(&default_normalization_config(), "application/pdf", bytes)
        .expect_err("malformed pdf rejected");
    assert!(
        matches!(
            err,
            NormalizationError::MediaTypeMismatch { .. }
                | NormalizationError::UnknownMediaType
                | NormalizationError::MalformedPdf
        ),
        "expected media-type mismatch/unknown/malformed, got {err:?}"
    );
}

#[test]
fn decompression_bomb_pdf_rejected() {
    let config = default_normalization_config();
    let bytes = build_pdf_with_stream(config.max_decompressed_bytes + 1);
    let err = normalization::normalize(&config, "application/pdf", &bytes)
        .expect_err("decompression bomb rejected");
    assert!(
        matches!(err, NormalizationError::DecompressionBomb { .. }),
        "expected DecompressionBomb, got {err:?}"
    );
}

// ── Stale callback rejected before any provider write ─────────────────────

fn zero_target() -> serde_json::Value {
    serde_json::Value::Array(std::iter::repeat_n(serde_json::Value::from(0), 32).collect())
}

fn make_pending(revision: u64, digest_bytes: [u8; 32]) -> PendingConfirmation {
    let digest_val = serde_json::to_value(digest_bytes).expect("serialize [u8; 32]");
    let json = serde_json::json!({
        "confirmation_id": "conf-sec",
        "workflow_id": "wf-sec",
        "workflow_revision": revision,
        "owner": 42,
        "topic": {
            "chat_id": -1001234567890_i64,
            "message_thread_id": 10
        },
        "preview_digest": digest_val,
        "mutation_target": zero_target(),
        "action": "start_sheet_or_doc_write",
        "expires_at": 2000000000_u64
    });
    serde_json::from_value(json).expect("valid pending confirmation")
}

#[test]
fn stale_callback_revision_rejected() {
    let digest = [0x42u8; 32];
    let pending = make_pending(7, digest);
    let stale_data = CallbackData::new(6, &domain::confirmation::PreviewDigest::new(digest));
    let raw = stale_data.to_callback_string().expect("serialize");
    let err = callbacks::validate_callback_against_pending(&raw, &pending)
        .expect_err("stale revision rejected");
    assert!(
        matches!(err, CallbackValidationError::StaleRevision { .. }),
        "expected StaleRevision, got {err:?}"
    );
}

#[test]
fn callback_with_mismatched_digest_rejected() {
    let pending = make_pending(7, [0x42u8; 32]);
    let other_digest = [0x99u8; 32];
    let bad_data = CallbackData::new(7, &domain::confirmation::PreviewDigest::new(other_digest));
    let raw = bad_data.to_callback_string().expect("serialize");
    let err = callbacks::validate_callback_against_pending(&raw, &pending)
        .expect_err("mismatched digest rejected");
    assert!(
        matches!(err, CallbackValidationError::PreviewDigestMismatch),
        "expected PreviewDigestMismatch, got {err:?}"
    );
}

// ── Removed participants fail authorization ───────────────────────────────

#[test]
fn removed_participant_fails_authorization() {
    let forum = ChatId::new(-1001234567890);
    let actor = ParticipantId::new(42).expect("valid participant");
    let now = WorkflowTimestamp::from_unix_seconds(1_700_000_000);
    let evidence = LiveMembershipEvidence::new(forum, actor, MembershipStatus::NotApproved, now);
    let err = authorize_participant(forum, actor, &evidence, now)
        .expect_err("removed participant rejected");
    assert!(
        matches!(err, AuthorizationError::NotApproved),
        "expected NotApproved, got {err:?}"
    );
}

#[test]
fn mismatched_forum_evidence_rejected() {
    let forum = ChatId::new(-1001234567890);
    let other_forum = ChatId::new(-1009999999999);
    let actor = ParticipantId::new(42).expect("valid participant");
    let now = WorkflowTimestamp::from_unix_seconds(1_700_000_000);
    let evidence = LiveMembershipEvidence::new(other_forum, actor, MembershipStatus::Approved, now);
    let err =
        authorize_participant(forum, actor, &evidence, now).expect_err("mismatched forum rejected");
    assert!(
        matches!(err, AuthorizationError::ForumMismatch { .. }),
        "expected ForumMismatch, got {err:?}"
    );
}

// ── Unsupported chat types rejected at intake ─────────────────────────────

#[test]
fn unsupported_chat_rejected_at_intake() {
    let json = serde_json::json!({
        "update_id": 9001,
        "message": {
            "message_id": 1,
            "from": { "id": 7, "is_bot": false },
            "chat": { "id": -100777, "type": "channel", "title": "Broadcast" },
            "text": "@novus_bot start"
        }
    });
    let body = serde_json::to_vec(&json).expect("serialize");
    let err = tg_normalize::normalize(&body).expect_err("channel rejected");
    assert!(
        matches!(err, NormalizeError::UnsupportedChat { .. }),
        "expected UnsupportedChat, got {err:?}"
    );
}
