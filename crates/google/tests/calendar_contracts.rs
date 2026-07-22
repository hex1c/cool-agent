//! Contract tests for the confirmed Google Calendar adapter (Task 36).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::sync::Mutex;

use application::calendar::{CalendarPreview, CalendarReminders, CalendarTarget};
use application::external_operation::{ExternalResourceId, ProviderOutcome};
use domain::confirmation::{
    ConfirmationAction, ConfirmationRecord, ConsumedConfirmation, MutationTargetFingerprint,
    PendingConfirmation, PreviewDigest, TopicMessageReference,
};
use domain::identity::{
    ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
};
use domain::workflow::{WaitDeadline, WorkflowRevision, WorkflowTimestamp};
use google::auth::{GoogleAccessToken, OwnerTokenSource};
use google::calendar::{
    CalendarAccess, CalendarError, CalendarProviderOutcome, CalendarSelection,
    ConfirmedCalendarProof, GoogleCalendarClient, GoogleCalendarService,
};

fn topic() -> TopicSessionId {
    TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77).unwrap())
}

fn participant(value: i64) -> ParticipantId {
    ParticipantId::new(value).unwrap()
}

fn time(seconds: u64) -> WorkflowTimestamp {
    WorkflowTimestamp::from_unix_seconds(seconds)
}

fn pending_confirmation(
    action: ConfirmationAction,
    target: MutationTargetFingerprint,
    digest: PreviewDigest,
) -> PendingConfirmation {
    PendingConfirmation::new(
        ConfirmationId::new("conf-cal-1").unwrap(),
        WorkflowId::new("wf-cal-1").unwrap(),
        WorkflowRevision::new(1),
        participant(101),
        topic(),
        digest,
        target,
        action,
        WaitDeadline::at(time(86_400)),
    )
}

fn consumed_record(
    action: ConfirmationAction,
    target: MutationTargetFingerprint,
    digest: PreviewDigest,
) -> ConfirmationRecord {
    use domain::authorization::MembershipAuthorizationSource;

    ConfirmationRecord::Consumed(ConsumedConfirmation::new(
        pending_confirmation(action, target, digest),
        participant(202),
        MembershipAuthorizationSource::Live,
        TopicMessageReference::new(topic(), MessageId::new(8).unwrap()),
        time(8),
        WorkflowRevision::new(2),
    ))
}

fn preview(send_invitations: bool, calendar: CalendarTarget) -> CalendarPreview {
    CalendarPreview::new(
        "Strategy sync".to_owned(),
        "2025-02-03T10:00:00+05:30".to_owned(),
        "2025-02-03T11:00:00+05:30".to_owned(),
        "Asia/Kolkata".to_owned(),
        calendar,
        Some("Quarterly planning".to_owned()),
        vec!["alice@example.com".to_owned(), "bob@example.com".to_owned()],
        CalendarReminders {
            push_minutes: Some(10),
            email_minutes: None,
        },
        send_invitations,
    )
    .expect("valid calendar preview")
}

fn proof(preview: &CalendarPreview) -> ConfirmedCalendarProof {
    let record = consumed_record(
        ConfirmationAction::StartCalendarOrEmailAction,
        preview.mutation_target().expect("target"),
        preview.digest().expect("digest"),
    );
    ConfirmedCalendarProof::from_consumed(&record).expect("proof")
}

fn resource_id(value: &str) -> ExternalResourceId {
    ExternalResourceId::new(value).expect("resource id")
}

#[derive(Debug, Clone, Copy)]
enum AccessBehavior {
    Writable,
    Denied,
    Error,
}

#[derive(Debug, Clone)]
enum CreateBehavior {
    Outcome(CalendarProviderOutcome),
    Error,
}

struct RecordingCalendarClient {
    access_behavior: AccessBehavior,
    create_behavior: CreateBehavior,
    access_calls: Mutex<u32>,
    create_calls: Mutex<u32>,
    last_send_invitations: Mutex<Option<bool>>,
}

impl RecordingCalendarClient {
    fn new(access_behavior: AccessBehavior, create_behavior: CreateBehavior) -> Self {
        Self {
            access_behavior,
            create_behavior,
            access_calls: Mutex::new(0),
            create_calls: Mutex::new(0),
            last_send_invitations: Mutex::new(None),
        }
    }

    fn access_call_count(&self) -> u32 {
        *self.access_calls.lock().expect("access calls")
    }

    fn create_call_count(&self) -> u32 {
        *self.create_calls.lock().expect("create calls")
    }

    fn last_send_invitations(&self) -> Option<bool> {
        *self.last_send_invitations.lock().expect("invitations")
    }
}

impl GoogleCalendarClient for RecordingCalendarClient {
    type Error = String;

    async fn calendar_access(
        &self,
        _token: &GoogleAccessToken,
        _calendar: &google::calendar::CalendarId,
    ) -> Result<CalendarAccess, Self::Error> {
        *self.access_calls.lock().expect("access calls") += 1;
        match self.access_behavior {
            AccessBehavior::Writable => Ok(CalendarAccess::Writable),
            AccessBehavior::Denied => Ok(CalendarAccess::Denied),
            AccessBehavior::Error => Err("access check failed".to_owned()),
        }
    }

    async fn create_event(
        &self,
        _token: &GoogleAccessToken,
        request: &google::calendar::CalendarEventRequest,
    ) -> Result<CalendarProviderOutcome, Self::Error> {
        *self.create_calls.lock().expect("create calls") += 1;
        *self.last_send_invitations.lock().expect("invitations") = Some(request.send_invitations());
        match &self.create_behavior {
            CreateBehavior::Outcome(outcome) => Ok(outcome.clone()),
            CreateBehavior::Error => Err("create failed".to_owned()),
        }
    }
}

#[derive(Debug)]
struct RecordingTokenSource {
    fail: bool,
    requested_owners: Mutex<Vec<ParticipantId>>,
}

impl RecordingTokenSource {
    fn available() -> Self {
        Self {
            fail: false,
            requested_owners: Mutex::new(Vec::new()),
        }
    }

    fn unavailable() -> Self {
        Self {
            fail: true,
            requested_owners: Mutex::new(Vec::new()),
        }
    }

    fn requested_owners(&self) -> Vec<ParticipantId> {
        self.requested_owners.lock().expect("owners").clone()
    }
}

impl OwnerTokenSource for RecordingTokenSource {
    type Error = String;

    async fn access_token(&self, owner: ParticipantId) -> Result<GoogleAccessToken, Self::Error> {
        self.requested_owners.lock().expect("owners").push(owner);
        if self.fail {
            Err("token unavailable".to_owned())
        } else {
            Ok(GoogleAccessToken::new("owner-token".to_owned()))
        }
    }
}

#[test]
fn proof_requires_consumed_calendar_action() {
    let preview = preview(false, CalendarTarget::Primary);
    let target = preview.mutation_target().unwrap();
    let digest = preview.digest().unwrap();

    let valid = consumed_record(
        ConfirmationAction::StartCalendarOrEmailAction,
        target,
        digest,
    );
    assert_eq!(
        ConfirmedCalendarProof::from_consumed(&valid)
            .expect("proof")
            .owner(),
        participant(101)
    );

    let pending = ConfirmationRecord::Pending(pending_confirmation(
        ConfirmationAction::StartCalendarOrEmailAction,
        target,
        digest,
    ));
    assert!(matches!(
        ConfirmedCalendarProof::from_consumed(&pending),
        Err(CalendarError::Unauthorized)
    ));

    let wrong = consumed_record(ConfirmationAction::StartDirectPdfGeneration, target, digest);
    assert!(matches!(
        ConfirmedCalendarProof::from_consumed(&wrong),
        Err(CalendarError::Unauthorized)
    ));
}

#[tokio::test]
async fn invitation_on_and_off_each_create_one_owner_event() {
    for (send_invitations, id) in [(true, "event-on"), (false, "event-off")] {
        let preview = preview(send_invitations, CalendarTarget::Primary);
        let request =
            google::calendar::CalendarEventRequest::from_preview(&preview).expect("request");
        let client = RecordingCalendarClient::new(
            AccessBehavior::Writable,
            CreateBehavior::Outcome(CalendarProviderOutcome::Created(resource_id(id))),
        );
        let service = GoogleCalendarService::new(client);
        let tokens = RecordingTokenSource::available();

        let outcome = service
            .create_event(&tokens, &proof(&preview), &request)
            .await
            .expect("create event");

        assert!(matches!(outcome, ProviderOutcome::Accepted { .. }));
        assert_eq!(service.client().create_call_count(), 1);
        assert_eq!(service.client().access_call_count(), 0);
        assert_eq!(
            service.client().last_send_invitations(),
            Some(send_invitations)
        );
        assert_eq!(tokens.requested_owners(), vec![participant(101)]);
    }
}

#[tokio::test]
async fn altered_payload_is_rejected_before_token_or_client_call() {
    let confirmed = preview(true, CalendarTarget::Primary);
    let changed = preview(false, CalendarTarget::Primary);
    let request = google::calendar::CalendarEventRequest::from_preview(&changed).expect("request");
    let client = RecordingCalendarClient::new(
        AccessBehavior::Writable,
        CreateBehavior::Outcome(CalendarProviderOutcome::Created(resource_id("event"))),
    );
    let service = GoogleCalendarService::new(client);
    let tokens = RecordingTokenSource::available();

    let result = service
        .create_event(&tokens, &proof(&confirmed), &request)
        .await;

    assert!(result.is_err());
    assert!(tokens.requested_owners().is_empty());
    assert_eq!(service.client().create_call_count(), 0);
}

#[tokio::test]
async fn writable_alternate_calendar_is_checked_then_created() {
    let preview = preview(
        false,
        CalendarTarget::Alternate {
            calendar_id: "work-calendar".to_owned(),
        },
    );
    let request = google::calendar::CalendarEventRequest::from_preview(&preview).expect("request");
    assert!(matches!(
        request.calendar(),
        CalendarSelection::Alternate(_)
    ));
    let service = GoogleCalendarService::new(RecordingCalendarClient::new(
        AccessBehavior::Writable,
        CreateBehavior::Outcome(CalendarProviderOutcome::Created(resource_id("event-alt"))),
    ));

    let outcome = service
        .create_event(
            &RecordingTokenSource::available(),
            &proof(&preview),
            &request,
        )
        .await
        .expect("create event");

    assert!(matches!(outcome, ProviderOutcome::Accepted { .. }));
    assert_eq!(service.client().access_call_count(), 1);
    assert_eq!(service.client().create_call_count(), 1);
}

#[tokio::test]
async fn denied_alternate_calendar_performs_zero_event_writes() {
    let preview = preview(
        false,
        CalendarTarget::Alternate {
            calendar_id: "denied-calendar".to_owned(),
        },
    );
    let request = google::calendar::CalendarEventRequest::from_preview(&preview).expect("request");
    let service = GoogleCalendarService::new(RecordingCalendarClient::new(
        AccessBehavior::Denied,
        CreateBehavior::Outcome(CalendarProviderOutcome::Created(resource_id(
            "must-not-create",
        ))),
    ));

    let outcome = service
        .create_event(
            &RecordingTokenSource::available(),
            &proof(&preview),
            &request,
        )
        .await
        .expect("classified outcome");

    assert!(matches!(outcome, ProviderOutcome::TerminalFailure(_)));
    assert_eq!(service.client().access_call_count(), 1);
    assert_eq!(service.client().create_call_count(), 0);
}

#[tokio::test]
async fn failed_access_check_is_retryable_without_event_write() {
    let preview = preview(
        false,
        CalendarTarget::Alternate {
            calendar_id: "work-calendar".to_owned(),
        },
    );
    let request = google::calendar::CalendarEventRequest::from_preview(&preview).expect("request");
    let service = GoogleCalendarService::new(RecordingCalendarClient::new(
        AccessBehavior::Error,
        CreateBehavior::Outcome(CalendarProviderOutcome::Created(resource_id(
            "must-not-create",
        ))),
    ));

    let outcome = service
        .create_event(
            &RecordingTokenSource::available(),
            &proof(&preview),
            &request,
        )
        .await
        .expect("classified outcome");

    assert!(matches!(outcome, ProviderOutcome::RetryableFailure(_)));
    assert_eq!(service.client().create_call_count(), 0);
}

#[tokio::test]
async fn unavailable_owner_token_is_retryable_without_client_call() {
    let preview = preview(false, CalendarTarget::Primary);
    let request = google::calendar::CalendarEventRequest::from_preview(&preview).expect("request");
    let service = GoogleCalendarService::new(RecordingCalendarClient::new(
        AccessBehavior::Writable,
        CreateBehavior::Outcome(CalendarProviderOutcome::Created(resource_id(
            "must-not-create",
        ))),
    ));

    let outcome = service
        .create_event(
            &RecordingTokenSource::unavailable(),
            &proof(&preview),
            &request,
        )
        .await
        .expect("classified outcome");

    assert!(matches!(outcome, ProviderOutcome::RetryableFailure(_)));
    assert_eq!(service.client().access_call_count(), 0);
    assert_eq!(service.client().create_call_count(), 0);
}

#[tokio::test]
async fn provider_outcomes_map_to_shared_executor_classifications() {
    for (behavior, expected) in [
        (
            CreateBehavior::Outcome(CalendarProviderOutcome::Ambiguous),
            "ambiguous",
        ),
        (
            CreateBehavior::Outcome(CalendarProviderOutcome::Terminal),
            "terminal",
        ),
        (CreateBehavior::Error, "retryable"),
    ] {
        let preview = preview(false, CalendarTarget::Primary);
        let request =
            google::calendar::CalendarEventRequest::from_preview(&preview).expect("request");
        let service = GoogleCalendarService::new(RecordingCalendarClient::new(
            AccessBehavior::Writable,
            behavior,
        ));
        let outcome = service
            .create_event(
                &RecordingTokenSource::available(),
                &proof(&preview),
                &request,
            )
            .await
            .expect("classified outcome");
        assert!(matches!(
            (expected, outcome),
            ("ambiguous", ProviderOutcome::Ambiguous(_))
                | ("terminal", ProviderOutcome::TerminalFailure(_))
                | ("retryable", ProviderOutcome::RetryableFailure(_))
        ));
        assert_eq!(service.client().create_call_count(), 1);
    }
}
