#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

//! Integration tests for the external-actions Lambda handlers (Task 37A).
//!
//! Fake runners exercise every ProviderOutcome category without real
//! Google or SMTP credentials.

use std::sync::Arc;

use application::external_operation::{ExternalResourceId, OperationFailure, ProviderOutcome};
use domain::confirmation::{MutationTargetFingerprint, PreviewDigest};
use email::message::SendError;
use external_actions_functions::EVENT_SCHEMA_VERSION;
use external_actions_functions::{
    CalendarActionEvent, CalendarActionRunner, EmailActionEvent, EmailActionRunner,
    GoogleCreateEvent, GoogleCreateRunner, GoogleMutationEvent, GoogleMutationRunner,
    process_calendar_action, process_email_action, process_google_create, process_google_mutation,
};
use google::calendar::CreateEventError;
use google::mutations::MutationError;
use google::sheets_docs::CreateFileError;

// ── Fake runners ──────────────────────────────────────────────────────

struct FakeRunner<T> {
    outcome: Arc<T>,
}

impl<T> FakeRunner<T> {
    fn new(outcome: T) -> Self {
        Self {
            outcome: Arc::new(outcome),
        }
    }
}

impl GoogleCreateRunner for FakeRunner<Result<ProviderOutcome, CreateFileError>> {
    async fn run(
        &self,
        _proof: &google::sheets_docs::ConfirmedCreateProof,
        _request: &google::sheets_docs::NewFileRequest,
    ) -> Result<ProviderOutcome, CreateFileError> {
        self.outcome.as_ref().clone().map_err(|e| e.clone())
    }
}

impl GoogleMutationRunner for FakeRunner<Result<ProviderOutcome, MutationError>> {
    async fn run(
        &self,
        _proof: &google::existing_files::ConfirmedMutationProof,
        _payload: &google::existing_files::MutationPayload,
    ) -> Result<ProviderOutcome, MutationError> {
        self.outcome.as_ref().clone().map_err(|e| e.clone())
    }
}

impl CalendarActionRunner for FakeRunner<Result<ProviderOutcome, CreateEventError>> {
    async fn run(
        &self,
        _proof: &google::calendar::ConfirmedCalendarProof,
        _request: &google::calendar::CalendarEventRequest,
    ) -> Result<ProviderOutcome, CreateEventError> {
        self.outcome.as_ref().clone().map_err(|e| e.clone())
    }
}

impl EmailActionRunner for FakeRunner<Result<ProviderOutcome, SendError>> {
    async fn run(
        &self,
        _proof: &email::message::ConfirmedEmailProof,
        _request: &email::message::EmailMessageRequest,
    ) -> Result<ProviderOutcome, SendError> {
        self.outcome.as_ref().clone().map_err(|e| e.clone())
    }
}

fn accepted_outcome() -> ProviderOutcome {
    ProviderOutcome::Accepted {
        resource_id: Some(ExternalResourceId::new("res-001").expect("valid id")),
    }
}

fn accepted_no_id() -> ProviderOutcome {
    ProviderOutcome::Accepted { resource_id: None }
}

fn retryable() -> ProviderOutcome {
    let code = application::external_operation::FailureCode::new("transient").expect("code");
    let summary =
        application::external_operation::SanitizedSummary::new("transient failure").expect("sum");
    ProviderOutcome::RetryableFailure(OperationFailure::new(code, summary))
}

fn terminal() -> ProviderOutcome {
    let code = application::external_operation::FailureCode::new("permanent").expect("code");
    let summary =
        application::external_operation::SanitizedSummary::new("permanent failure").expect("sum");
    ProviderOutcome::TerminalFailure(OperationFailure::new(code, summary))
}

fn ambiguous() -> ProviderOutcome {
    let code = application::external_operation::FailureCode::new("ambiguous").expect("code");
    let summary =
        application::external_operation::SanitizedSummary::new("outcome unknown").expect("sum");
    ProviderOutcome::Ambiguous(OperationFailure::new(code, summary))
}

fn dummy_proof_hex() -> (String, String) {
    let target = MutationTargetFingerprint::new([42u8; 32]);
    let digest = PreviewDigest::new([17u8; 32]);
    (
        hex::encode(target.as_bytes()),
        hex::encode(digest.as_bytes()),
    )
}

// ── Google create tests ───────────────────────────────────────────────

fn google_create_event() -> GoogleCreateEvent {
    let (target_hex, digest_hex) = dummy_proof_hex();
    GoogleCreateEvent {
        schema_version: EVENT_SCHEMA_VERSION.to_owned(),
        kind: external_actions_functions::google::FileKindDto {
            kind: "sheet".to_owned(),
        },
        title: "Q1 Report".to_owned(),
        destination_folder_id: Some("folder-123".to_owned()),
        destination_shared_drive_id: None,
        owner: 101,
        mutation_target_hex: target_hex,
        preview_digest_hex: digest_hex,
        workflow_revision: 3,
    }
}

#[tokio::test]
async fn google_create_accepted() {
    let event = google_create_event();
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let result = process_google_create(event, &runner)
        .await
        .expect("accepted");
    assert_eq!(result.outcome, "accepted");
    assert_eq!(result.resource_id.as_deref(), Some("res-001"));
}

#[tokio::test]
async fn google_create_accepted_no_resource_id() {
    let event = google_create_event();
    let runner = FakeRunner::new(Ok(accepted_no_id()));
    let result = process_google_create(event, &runner)
        .await
        .expect("accepted");
    assert_eq!(result.outcome, "accepted");
    assert!(result.resource_id.is_none());
}

#[tokio::test]
async fn google_create_retryable() {
    let event = google_create_event();
    let runner = FakeRunner::new(Ok(retryable()));
    let result = process_google_create(event, &runner)
        .await
        .expect("retryable");
    assert_eq!(result.outcome, "retryable_failure");
    assert_eq!(result.failure_code.as_deref(), Some("transient"));
}

#[tokio::test]
async fn google_create_terminal() {
    let event = google_create_event();
    let runner = FakeRunner::new(Ok(terminal()));
    let result = process_google_create(event, &runner)
        .await
        .expect("terminal");
    assert_eq!(result.outcome, "terminal_failure");
}

#[tokio::test]
async fn google_create_ambiguous() {
    let event = google_create_event();
    let runner = FakeRunner::new(Ok(ambiguous()));
    let result = process_google_create(event, &runner)
        .await
        .expect("ambiguous");
    assert_eq!(result.outcome, "ambiguous");
}

#[tokio::test]
async fn google_create_schema_mismatch() {
    let mut event = google_create_event();
    event.schema_version = "novus.external-actions.v0".to_owned();
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let err = process_google_create(event, &runner)
        .await
        .expect_err("schema mismatch");
    let msg = err.to_string();
    assert!(msg.contains("schema version mismatch"));
}

#[tokio::test]
async fn google_create_invalid_owner() {
    let mut event = google_create_event();
    event.owner = 0;
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let err = process_google_create(event, &runner)
        .await
        .expect_err("invalid owner");
    assert!(err.to_string().contains("invalid owner"));
}

// ── Google mutation tests ─────────────────────────────────────────────

fn google_mutation_event() -> GoogleMutationEvent {
    let (target_hex, digest_hex) = dummy_proof_hex();
    GoogleMutationEvent {
        schema_version: EVENT_SCHEMA_VERSION.to_owned(),
        file_id: "file-abc".to_owned(),
        file_kind: external_actions_functions::google::FileKindDto {
            kind: "sheet".to_owned(),
        },
        section: "Sheet1!A1".to_owned(),
        updates: vec![external_actions_functions::google::FieldUpdateDto {
            field: "status".to_owned(),
            resulting_value: "approved".to_owned(),
        }],
        owner: 101,
        actor: 202,
        mutation_target_hex: target_hex,
        preview_digest_hex: digest_hex,
        workflow_revision: 3,
    }
}

#[tokio::test]
async fn google_mutation_accepted() {
    let event = google_mutation_event();
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let result = process_google_mutation(event, &runner)
        .await
        .expect("accepted");
    assert_eq!(result.outcome, "accepted");
    assert_eq!(result.resource_id.as_deref(), Some("res-001"));
}

#[tokio::test]
async fn google_mutation_retryable() {
    let event = google_mutation_event();
    let runner = FakeRunner::new(Ok(retryable()));
    let result = process_google_mutation(event, &runner)
        .await
        .expect("retryable");
    assert_eq!(result.outcome, "retryable_failure");
}

#[tokio::test]
async fn google_mutation_terminal() {
    let event = google_mutation_event();
    let runner = FakeRunner::new(Ok(terminal()));
    let result = process_google_mutation(event, &runner)
        .await
        .expect("terminal");
    assert_eq!(result.outcome, "terminal_failure");
}

#[tokio::test]
async fn google_mutation_ambiguous() {
    let event = google_mutation_event();
    let runner = FakeRunner::new(Ok(ambiguous()));
    let result = process_google_mutation(event, &runner)
        .await
        .expect("ambiguous");
    assert_eq!(result.outcome, "ambiguous");
}

#[tokio::test]
async fn google_mutation_schema_mismatch() {
    let mut event = google_mutation_event();
    event.schema_version = "v0".to_owned();
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let err = process_google_mutation(event, &runner)
        .await
        .expect_err("schema mismatch");
    assert!(err.to_string().contains("schema version mismatch"));
}

// ── Calendar tests ────────────────────────────────────────────────────

fn calendar_preview() -> application::calendar::CalendarPreview {
    application::calendar::CalendarPreview::new(
        "Strategy sync".to_owned(),
        "2025-02-03T10:00:00+05:30".to_owned(),
        "2025-02-03T11:00:00+05:30".to_owned(),
        "Asia/Kolkata".to_owned(),
        application::calendar::CalendarTarget::Primary,
        Some("Quarterly planning".to_owned()),
        vec!["alice@example.com".to_owned()],
        application::calendar::CalendarReminders {
            push_minutes: Some(10),
            email_minutes: None,
        },
        true,
    )
    .expect("valid preview")
}

fn calendar_event() -> CalendarActionEvent {
    let preview = calendar_preview();
    let target = preview.mutation_target().expect("target");
    let digest = preview.digest().expect("digest");
    CalendarActionEvent {
        schema_version: EVENT_SCHEMA_VERSION.to_owned(),
        title: preview.title().to_owned(),
        start: preview.start().to_owned(),
        end: preview.end().to_owned(),
        timezone: preview.timezone().to_owned(),
        calendar_id: None,
        description: preview.description().map(str::to_owned),
        attendees: preview.attendees().to_vec(),
        reminders_push_minutes: preview.reminders().push_minutes,
        reminders_email_minutes: preview.reminders().email_minutes,
        send_invitations: preview.send_invitations(),
        owner: 101,
        mutation_target_hex: hex::encode(target.as_bytes()),
        preview_digest_hex: hex::encode(digest.as_bytes()),
        workflow_revision: 3,
    }
}

#[tokio::test]
async fn calendar_accepted() {
    let event = calendar_event();
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let result = process_calendar_action(event, &runner)
        .await
        .expect("accepted");
    assert_eq!(result.outcome, "accepted");
    assert_eq!(result.resource_id.as_deref(), Some("res-001"));
}

#[tokio::test]
async fn calendar_retryable() {
    let event = calendar_event();
    let runner = FakeRunner::new(Ok(retryable()));
    let result = process_calendar_action(event, &runner)
        .await
        .expect("retryable");
    assert_eq!(result.outcome, "retryable_failure");
}

#[tokio::test]
async fn calendar_terminal() {
    let event = calendar_event();
    let runner = FakeRunner::new(Ok(terminal()));
    let result = process_calendar_action(event, &runner)
        .await
        .expect("terminal");
    assert_eq!(result.outcome, "terminal_failure");
}

#[tokio::test]
async fn calendar_ambiguous() {
    let event = calendar_event();
    let runner = FakeRunner::new(Ok(ambiguous()));
    let result = process_calendar_action(event, &runner)
        .await
        .expect("ambiguous");
    assert_eq!(result.outcome, "ambiguous");
}

#[tokio::test]
async fn calendar_target_mismatch() {
    let mut event = calendar_event();
    // Replace with a dummy target that won't match the preview-derived request.
    event.mutation_target_hex = hex::encode([99u8; 32]);
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let err = process_calendar_action(event, &runner)
        .await
        .expect_err("target mismatch");
    assert!(err.to_string().contains("target mismatch"));
}

#[tokio::test]
async fn calendar_schema_mismatch() {
    let mut event = calendar_event();
    event.schema_version = "v0".to_owned();
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let err = process_calendar_action(event, &runner)
        .await
        .expect_err("schema mismatch");
    assert!(err.to_string().contains("schema version mismatch"));
}

// ── Email tests ───────────────────────────────────────────────────────

fn email_preview() -> application::email::EmailPreview {
    application::email::EmailPreview::new(
        vec!["customer@example.com".to_owned()],
        vec![],
        vec![],
        "Quotation follow-up".to_owned(),
        "Please find the quotation attached.".to_owned(),
        true,
        true,
        7,
    )
    .expect("valid preview")
}

fn email_event() -> EmailActionEvent {
    let preview = email_preview();
    let target = preview.mutation_target().expect("target");
    let digest = preview.digest().expect("digest");
    EmailActionEvent {
        schema_version: EVENT_SCHEMA_VERSION.to_owned(),
        recipients: preview.recipients().to_vec(),
        cc: preview.cc().to_vec(),
        bcc: preview.bcc().to_vec(),
        subject: preview.subject().to_owned(),
        body: preview.body().to_owned(),
        attach_quotation_pdf: preview.attach_quotation_pdf(),
        include_seven_day_link: preview.include_seven_day_link(),
        link_expiry_days: preview.link_expiry_days(),
        owner: 101,
        mutation_target_hex: hex::encode(target.as_bytes()),
        preview_digest_hex: hex::encode(digest.as_bytes()),
        workflow_revision: 3,
    }
}

#[tokio::test]
async fn email_accepted() {
    let event = email_event();
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let result = process_email_action(event, &runner)
        .await
        .expect("accepted");
    assert_eq!(result.outcome, "accepted");
    assert_eq!(result.resource_id.as_deref(), Some("res-001"));
}

#[tokio::test]
async fn email_retryable() {
    let event = email_event();
    let runner = FakeRunner::new(Ok(retryable()));
    let result = process_email_action(event, &runner)
        .await
        .expect("retryable");
    assert_eq!(result.outcome, "retryable_failure");
}

#[tokio::test]
async fn email_terminal() {
    let event = email_event();
    let runner = FakeRunner::new(Ok(terminal()));
    let result = process_email_action(event, &runner)
        .await
        .expect("terminal");
    assert_eq!(result.outcome, "terminal_failure");
}

#[tokio::test]
async fn email_ambiguous() {
    let event = email_event();
    let runner = FakeRunner::new(Ok(ambiguous()));
    let result = process_email_action(event, &runner)
        .await
        .expect("ambiguous");
    assert_eq!(result.outcome, "ambiguous");
}

#[tokio::test]
async fn email_target_mismatch() {
    let mut event = email_event();
    event.mutation_target_hex = hex::encode([99u8; 32]);
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let err = process_email_action(event, &runner)
        .await
        .expect_err("target mismatch");
    assert!(err.to_string().contains("target mismatch"));
}

#[tokio::test]
async fn email_schema_mismatch() {
    let mut event = email_event();
    event.schema_version = "v0".to_owned();
    let runner = FakeRunner::new(Ok(accepted_outcome()));
    let err = process_email_action(event, &runner)
        .await
        .expect_err("schema mismatch");
    assert!(err.to_string().contains("schema version mismatch"));
}
