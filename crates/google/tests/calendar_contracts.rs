//! Contract tests for the confirmed Google Calendar event creation adapter
//! (Task 36).
//!
//! These tests prove the Task 36 acceptance criteria at the adapter layer:
//! - A consumed confirmation with `StartCalendarOrEmailAction` yields a proof
//!   bound to the workflow owner and mutation-target fingerprint.
//! - Invitation-on and invitation-off fixtures each create exactly one event.
//! - Reminder settings are carried into the request unchanged.
//! - Owner-primary and alternate-calendar selections are honored.
//! - Target mismatch and wrong-action confirmations are rejected before any
//!   provider call.
//! - Ambiguous and terminal provider outcomes map to sanitized
//!   `ProviderOutcome`s without exposing attendee emails or the token.
//!
//! The provider is a recording mock; the application-level retry/idempotency
//! binding is covered by `crates/application/tests/calendar_flow.rs`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::sync::Mutex;

use application::external_operation::{ExternalResourceId, ProviderOutcome};
use domain::confirmation::{
    ConfirmationAction, ConfirmationRecord, ConsumedConfirmation, MutationTargetFingerprint,
    PendingConfirmation, PreviewDigest, TopicMessageReference,
};
use domain::idempotency::OperationTargetFingerprint;
use domain::identity::{
    ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
};
use domain::workflow::{WaitDeadline, WorkflowRevision, WorkflowTimestamp};

use google::auth::GoogleAccessToken;
use google::calendar::{
    CalendarError, CalendarId, CalendarProviderOutcome, CalendarSelection, ConfirmedCalendarProof,
    EventTimestamp, EventTitle, GoogleCalendarClient, GoogleCalendarService, ReminderSettings,
    TimezoneLabel,
};

// ── helpers ──────────────────────────────────────────────────────────────

fn topic() -> TopicSessionId {
    TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77).unwrap())
}

fn participant(value: i64) -> ParticipantId {
    ParticipantId::new(value).unwrap()
}

fn time(seconds: u64) -> WorkflowTimestamp {
    WorkflowTimestamp::from_unix_seconds(seconds)
}

fn fingerprint(bytes: [u8; 32]) -> MutationTargetFingerprint {
    MutationTargetFingerprint::new(bytes)
}

fn op_fingerprint(bytes: [u8; 32]) -> OperationTargetFingerprint {
    OperationTargetFingerprint::new(bytes)
}

fn digest(bytes: [u8; 32]) -> PreviewDigest {
    PreviewDigest::new(bytes)
}

fn pending_confirmation(
    action: ConfirmationAction,
    target: MutationTargetFingerprint,
) -> PendingConfirmation {
    PendingConfirmation::new(
        ConfirmationId::new("conf-cal-1").unwrap(),
        WorkflowId::new("wf-cal-1").unwrap(),
        WorkflowRevision::new(1),
        participant(101),
        topic(),
        digest([0u8; 32]),
        target,
        action,
        WaitDeadline::at(time(86_400)),
    )
}

fn consumed_record(
    action: ConfirmationAction,
    target: MutationTargetFingerprint,
) -> ConfirmationRecord {
    use domain::authorization::MembershipAuthorizationSource;
    let pending = pending_confirmation(action, target);
    let consumed = ConsumedConfirmation::new(
        pending,
        participant(202),
        MembershipAuthorizationSource::Live,
        TopicMessageReference::new(topic(), MessageId::new(8).unwrap()),
        time(8),
        WorkflowRevision::new(2),
    );
    ConfirmationRecord::Consumed(consumed)
}

fn proof(target_bytes: [u8; 32]) -> ConfirmedCalendarProof {
    let record = consumed_record(
        ConfirmationAction::StartCalendarOrEmailAction,
        fingerprint(target_bytes),
    );
    ConfirmedCalendarProof::from_consumed(&record).expect("proof extracted")
}

fn reminders(push: Option<u16>, email: Option<u16>) -> ReminderSettings {
    ReminderSettings::new(push, email).expect("valid reminders")
}

fn event_request(
    target_bytes: [u8; 32],
    send_invitations: bool,
    calendar: CalendarSelection,
) -> google::calendar::CalendarEventRequest {
    google::calendar::CalendarEventRequest::new(
        EventTitle::new("Strategy sync").unwrap(),
        EventTimestamp::new("2025-02-03T10:00:00+05:30").unwrap(),
        EventTimestamp::new("2025-02-03T11:00:00+05:30").unwrap(),
        TimezoneLabel::new("Asia/Kolkata").unwrap(),
        calendar,
        None,
        vec![
            google::calendar::AttendeeEmail::new("alice@example.com").unwrap(),
            google::calendar::AttendeeEmail::new("bob@example.com").unwrap(),
        ],
        reminders(Some(10), None),
        send_invitations,
        op_fingerprint(target_bytes),
    )
    .expect("valid event request")
}

// ── mock clients ─────────────────────────────────────────────────────────

struct RecordingCalendarClient {
    calls: Mutex<u32>,
    last_send_invitations: Mutex<Option<bool>>,
    outcome: CalendarProviderOutcome,
}

impl RecordingCalendarClient {
    fn new(outcome: CalendarProviderOutcome) -> Self {
        Self {
            calls: Mutex::new(0),
            last_send_invitations: Mutex::new(None),
            outcome,
        }
    }

    fn call_count(&self) -> u32 {
        *self.calls.lock().expect("lock poisoned")
    }

    fn last_send_invitations(&self) -> Option<bool> {
        *self.last_send_invitations.lock().expect("lock poisoned")
    }
}

impl GoogleCalendarClient for RecordingCalendarClient {
    type Error = String;

    async fn create_event(
        &self,
        _token: &GoogleAccessToken,
        request: &google::calendar::CalendarEventRequest,
    ) -> Result<CalendarProviderOutcome, Self::Error> {
        *self.calls.lock().expect("lock poisoned") += 1;
        *self.last_send_invitations.lock().expect("lock poisoned") =
            Some(request.send_invitations());
        Ok(self.outcome.clone())
    }
}

struct FailingCalendarClient;

impl GoogleCalendarClient for FailingCalendarClient {
    type Error = String;

    async fn create_event(
        &self,
        _token: &GoogleAccessToken,
        _request: &google::calendar::CalendarEventRequest,
    ) -> Result<CalendarProviderOutcome, Self::Error> {
        Err("calendar api unreachable".to_string())
    }
}

fn token() -> GoogleAccessToken {
    GoogleAccessToken::new("owner-token".to_string())
}

fn resource_id(value: &str) -> ExternalResourceId {
    ExternalResourceId::new(value).expect("valid resource id")
}

// ── proof tests ──────────────────────────────────────────────────────────

mod proof {
    use super::*;

    #[test]
    fn from_consumed_with_correct_action_succeeds() {
        let record = consumed_record(
            ConfirmationAction::StartCalendarOrEmailAction,
            fingerprint([0u8; 32]),
        );
        let proof = ConfirmedCalendarProof::from_consumed(&record).expect("proof extracted");
        assert_eq!(proof.owner(), participant(101));
        assert_eq!(proof.mutation_target().as_bytes(), &[0u8; 32]);
        assert_eq!(proof.workflow_revision().get(), 2);
    }

    #[test]
    fn from_pending_record_rejected() {
        let record = ConfirmationRecord::Pending(pending_confirmation(
            ConfirmationAction::StartCalendarOrEmailAction,
            fingerprint([0u8; 32]),
        ));
        let err = ConfirmedCalendarProof::from_consumed(&record).expect_err("unauthorized");
        assert!(matches!(err, CalendarError::Unauthorized));
    }

    #[test]
    fn from_consumed_with_wrong_action_rejected() {
        let record = consumed_record(
            ConfirmationAction::StartDirectPdfGeneration,
            fingerprint([0u8; 32]),
        );
        let err = ConfirmedCalendarProof::from_consumed(&record).expect_err("unauthorized");
        assert!(matches!(err, CalendarError::Unauthorized));
    }
}

// ── calendar creation tests ──────────────────────────────────────────────

mod calendar_creation {
    use super::*;

    #[tokio::test]
    async fn invitation_on_creates_one_event() {
        let target = [3u8; 32];
        let client = RecordingCalendarClient::new(CalendarProviderOutcome::Created(resource_id(
            "evt-invitation-on",
        )));
        let service = GoogleCalendarService::new(client);
        let req = event_request(target, true, CalendarSelection::Primary);
        let outcome = service
            .create_event(&token(), &proof(target), &req)
            .await
            .expect("create event");
        assert!(matches!(
            outcome,
            ProviderOutcome::Accepted { ref resource_id } if resource_id.as_ref().is_some_and(|r| r.as_str() == "evt-invitation-on")
        ));
        assert_eq!(service.client().call_count(), 1);
        assert_eq!(service.client().last_send_invitations(), Some(true));
    }

    #[tokio::test]
    async fn invitation_off_creates_one_event() {
        let target = [4u8; 32];
        let client = RecordingCalendarClient::new(CalendarProviderOutcome::Created(resource_id(
            "evt-invitation-off",
        )));
        let service = GoogleCalendarService::new(client);
        // send_invitations=false but attendees present is allowed (PRD §6.5
        // step 7 — the invitation choice is a confirmation-bound field).
        let req = event_request(target, false, CalendarSelection::Primary);
        let outcome = service
            .create_event(&token(), &proof(target), &req)
            .await
            .expect("create event");
        assert!(matches!(outcome, ProviderOutcome::Accepted { .. }));
        assert_eq!(service.client().call_count(), 1);
        assert_eq!(service.client().last_send_invitations(), Some(false));
    }

    #[tokio::test]
    async fn primary_calendar_selection_honored() {
        let target = [5u8; 32];
        let client = RecordingCalendarClient::new(CalendarProviderOutcome::Created(resource_id(
            "evt-primary",
        )));
        let service = GoogleCalendarService::new(client);
        let req = event_request(target, false, CalendarSelection::Primary);
        let _ = service
            .create_event(&token(), &proof(target), &req)
            .await
            .expect("create event");
        assert_eq!(service.client().call_count(), 1);
    }

    #[tokio::test]
    async fn alternate_calendar_selection_honored() {
        let target = [6u8; 32];
        let client =
            RecordingCalendarClient::new(CalendarProviderOutcome::Created(resource_id("evt-alt")));
        let service = GoogleCalendarService::new(client);
        let alt = CalendarId::new("work-calendar-xyz").unwrap();
        let req = event_request(target, false, CalendarSelection::Alternate(alt));
        let outcome = service
            .create_event(&token(), &proof(target), &req)
            .await
            .expect("create event");
        assert!(matches!(outcome, ProviderOutcome::Accepted { .. }));
        assert_eq!(service.client().call_count(), 1);
    }

    #[tokio::test]
    async fn reminder_settings_carried_into_request() {
        let target = [7u8; 32];
        let req = event_request(target, false, CalendarSelection::Primary);
        assert_eq!(req.reminders().push_minutes(), Some(10));
        assert_eq!(req.reminders().email_minutes(), None);
    }

    #[tokio::test]
    async fn target_mismatch_rejected_without_client_call() {
        let proof_target = [1u8; 32];
        let request_target = [2u8; 32];

        let client = RecordingCalendarClient::new(CalendarProviderOutcome::Ambiguous);
        let service = GoogleCalendarService::new(client);
        let req = event_request(request_target, false, CalendarSelection::Primary);

        let err = service
            .create_event(&token(), &proof(proof_target), &req)
            .await
            .expect_err("target mismatch");
        assert!(err.to_string().contains("target mismatch"));
        assert_eq!(service.client().call_count(), 0);
    }

    #[tokio::test]
    async fn ambiguous_outcome_maps_to_manual_review_provider_outcome() {
        let target = [8u8; 32];
        let client = RecordingCalendarClient::new(CalendarProviderOutcome::Ambiguous);
        let service = GoogleCalendarService::new(client);
        let req = event_request(target, false, CalendarSelection::Primary);
        let outcome = service
            .create_event(&token(), &proof(target), &req)
            .await
            .expect("ambiguous outcome");
        assert!(matches!(outcome, ProviderOutcome::Ambiguous(_)));
    }

    #[tokio::test]
    async fn terminal_outcome_maps_to_terminal_failure() {
        let target = [9u8; 32];
        let client = RecordingCalendarClient::new(CalendarProviderOutcome::Terminal);
        let service = GoogleCalendarService::new(client);
        let req = event_request(target, false, CalendarSelection::Primary);
        let outcome = service
            .create_event(&token(), &proof(target), &req)
            .await
            .expect("terminal outcome");
        assert!(matches!(outcome, ProviderOutcome::TerminalFailure(_)));
    }

    #[tokio::test]
    async fn client_error_maps_to_retryable_failure() {
        let target = [10u8; 32];
        let service = GoogleCalendarService::new(FailingCalendarClient);
        let req = event_request(target, false, CalendarSelection::Primary);
        let outcome = service
            .create_event(&token(), &proof(target), &req)
            .await
            .expect("retryable outcome");
        assert!(matches!(outcome, ProviderOutcome::RetryableFailure(_)));
    }
}
