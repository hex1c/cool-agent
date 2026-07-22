//! Google Calendar confirmed-event creation adapter (Task 36).
//!
//! Pure adapter layer that binds a consumed confirmation to a validated
//! [`CalendarEventRequest`] and a [`GoogleCalendarClient`] invocation.  Like
//! the Sheet/Doc create and existing-file mutation adapters, every method is
//! pure — it validates domain types and returns a [`ProviderOutcome`] for the
//! shared external-operation executor.  Persistence and retry are the caller's
//! responsibility.
//!
//! The proof enforces the Task 36 contract:
//! - The confirmation must be consumed with `StartCalendarOrEmailAction`.
//! - The operation target fingerprint must match the confirmation's
//!   mutation-target fingerprint.
//! - Owner-bound execution: the proof carries the workflow owner, and the
//!   caller must obtain a Google access token for that owner via
//!   [`crate::auth::OwnerTokenSource`].

use std::fmt::{self, Display};

use application::external_operation::{
    ExternalResourceId, FailureCode, OperationFailure, ProviderOutcome, SanitizedSummary,
};
use domain::confirmation::{ConfirmationAction, ConfirmationRecord, ConfirmationStatus};
use domain::confirmation::{MutationTargetFingerprint, PreviewDigest};
use domain::idempotency::OperationTargetFingerprint;
use domain::identity::ParticipantId;
use domain::workflow::WorkflowRevision;

use crate::auth::GoogleAccessToken;

// ── bounded value types ───────────────────────────────────────────────

const MAX_TITLE_BYTES: usize = 256;
const MAX_TIMEZONE_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 2048;
const MAX_CALENDAR_ID_BYTES: usize = 256;
const MAX_EMAIL_BYTES: usize = 320;
const MAX_ATTENDEES: usize = 200;
const MAX_TIMESTAMP_BYTES: usize = 64;
const MAX_REMINDER_MINUTES: u16 = 40_320; // 28 days

/// A validated, non-empty event title.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EventTitle(String);

impl EventTitle {
    pub fn new(value: impl Into<String>) -> Result<Self, CalendarError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_TITLE_BYTES {
            return Err(CalendarError::InvalidTitle);
        }
        if value.chars().any(char::is_control) {
            return Err(CalendarError::InvalidTitle);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated IANA-style timezone label (e.g. `Asia/Kolkata`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimezoneLabel(String);

impl TimezoneLabel {
    pub fn new(value: impl Into<String>) -> Result<Self, CalendarError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_TIMEZONE_BYTES {
            return Err(CalendarError::InvalidTimezone);
        }
        // IANA timezone identifiers are ASCII path segments separated by '/'.
        if value
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+')))
        {
            return Err(CalendarError::InvalidTimezone);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated RFC3339-style event boundary timestamp.
///
/// The adapter does not parse the wall-clock value; it bounds its length and
/// character set so the provider receives only a small, opaque, ASCII string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EventTimestamp(String);

impl EventTimestamp {
    pub fn new(value: impl Into<String>) -> Result<Self, CalendarError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_TIMESTAMP_BYTES {
            return Err(CalendarError::InvalidTimestamp);
        }
        if value.bytes().any(|b| b.is_ascii_control() || b == b' ') {
            return Err(CalendarError::InvalidTimestamp);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated Google Calendar identifier (`primary` or an opaque calendar id).
///
/// `None` selects the workflow owner's primary calendar (PRD §6.5 step 3).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CalendarId(String);

impl CalendarId {
    pub fn new(value: impl Into<String>) -> Result<Self, CalendarError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_CALENDAR_ID_BYTES {
            return Err(CalendarError::InvalidCalendarId);
        }
        if value.chars().any(char::is_control) {
            return Err(CalendarError::InvalidCalendarId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated attendee email address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttendeeEmail(String);

impl AttendeeEmail {
    pub fn new(value: impl Into<String>) -> Result<Self, CalendarError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_EMAIL_BYTES {
            return Err(CalendarError::InvalidAttendee);
        }
        // Minimal, conservative email shape: one '@', local and domain parts
        // present, ASCII printable without control characters or spaces.
        if value.chars().any(|c| c.is_ascii_control() || c == ' ') {
            return Err(CalendarError::InvalidAttendee);
        }
        let Some((local, domain)) = value.split_once('@') else {
            return Err(CalendarError::InvalidAttendee);
        };
        if local.is_empty() || domain.is_empty() {
            return Err(CalendarError::InvalidAttendee);
        }
        if domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        {
            Ok(Self(value))
        } else {
            Err(CalendarError::InvalidAttendee)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Workflow-owner reminder settings bound to the confirmation.
///
/// PRD §6.6: Google Calendar reminders are for the workflow owner. Both
/// notification channels are optional; setting both to `None` is rejected so
/// the preview always shows at least one reminder (or an explicit "none"
/// encoded as zero-minute reminders by the caller).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderSettings {
    push_minutes: Option<u16>,
    email_minutes: Option<u16>,
}

impl ReminderSettings {
    pub fn new(
        push_minutes: Option<u16>,
        email_minutes: Option<u16>,
    ) -> Result<Self, CalendarError> {
        if let Some(m) = push_minutes
            && m > MAX_REMINDER_MINUTES
        {
            return Err(CalendarError::InvalidReminder);
        }
        if let Some(m) = email_minutes
            && m > MAX_REMINDER_MINUTES
        {
            return Err(CalendarError::InvalidReminder);
        }
        if push_minutes.is_none() && email_minutes.is_none() {
            return Err(CalendarError::NoReminders);
        }
        Ok(Self {
            push_minutes,
            email_minutes,
        })
    }

    pub const fn push_minutes(&self) -> Option<u16> {
        self.push_minutes
    }

    pub const fn email_minutes(&self) -> Option<u16> {
        self.email_minutes
    }
}

/// A validated event description (optional, bounded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventDescription(String);

impl EventDescription {
    pub fn new(value: impl Into<String>) -> Result<Self, CalendarError> {
        let value = value.into();
        if value.len() > MAX_DESCRIPTION_BYTES {
            return Err(CalendarError::InvalidDescription);
        }
        if value.chars().any(char::is_control) {
            return Err(CalendarError::InvalidDescription);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ── request + proof ───────────────────────────────────────────────────

/// Which calendar the event targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarSelection {
    /// The workflow owner's primary calendar (PRD default).
    Primary,
    /// An alternate calendar the workflow owner's account can access
    /// (PRD §6.5 step 5 — only when an approved participant selects it).
    Alternate(CalendarId),
}

/// A request to create one Calendar event, validated against a confirmation proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEventRequest {
    title: EventTitle,
    start: EventTimestamp,
    end: EventTimestamp,
    timezone: TimezoneLabel,
    calendar: CalendarSelection,
    description: Option<EventDescription>,
    attendees: Vec<AttendeeEmail>,
    reminders: ReminderSettings,
    send_invitations: bool,
    target_fingerprint: OperationTargetFingerprint,
}

impl CalendarEventRequest {
    /// Build a validated event request.
    ///
    /// The `target_fingerprint` must equal the confirmation's
    /// mutation-target fingerprint; the service enforces that before invoking
    /// the provider.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        title: EventTitle,
        start: EventTimestamp,
        end: EventTimestamp,
        timezone: TimezoneLabel,
        calendar: CalendarSelection,
        description: Option<EventDescription>,
        attendees: Vec<AttendeeEmail>,
        reminders: ReminderSettings,
        send_invitations: bool,
        target_fingerprint: OperationTargetFingerprint,
    ) -> Result<Self, CalendarError> {
        if attendees.len() > MAX_ATTENDEES {
            return Err(CalendarError::TooManyAttendees);
        }
        // Attendee invitations require at least one attendee when requested.
        if send_invitations && attendees.is_empty() {
            return Err(CalendarError::InvitationsWithoutAttendees);
        }
        Ok(Self {
            title,
            start,
            end,
            timezone,
            calendar,
            description,
            attendees,
            reminders,
            send_invitations,
            target_fingerprint,
        })
    }

    pub fn target_fingerprint(&self) -> OperationTargetFingerprint {
        self.target_fingerprint
    }

    pub fn title(&self) -> &EventTitle {
        &self.title
    }

    pub fn start(&self) -> &EventTimestamp {
        &self.start
    }

    pub fn end(&self) -> &EventTimestamp {
        &self.end
    }

    pub fn timezone(&self) -> &TimezoneLabel {
        &self.timezone
    }

    pub fn calendar(&self) -> &CalendarSelection {
        &self.calendar
    }

    pub fn description(&self) -> Option<&EventDescription> {
        self.description.as_ref()
    }

    pub fn attendees(&self) -> &[AttendeeEmail] {
        &self.attendees
    }

    pub fn reminders(&self) -> &ReminderSettings {
        &self.reminders
    }

    pub fn send_invitations(&self) -> bool {
        self.send_invitations
    }
}

/// Proof extracted from a consumed confirmation that authorizes event creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedCalendarProof {
    owner: ParticipantId,
    mutation_target: MutationTargetFingerprint,
    preview_digest: PreviewDigest,
    workflow_revision: WorkflowRevision,
}

impl ConfirmedCalendarProof {
    pub fn from_consumed(record: &ConfirmationRecord) -> Result<Self, CalendarError> {
        if record.status() != ConfirmationStatus::Consumed {
            return Err(CalendarError::Unauthorized);
        }
        if record.action() != ConfirmationAction::StartCalendarOrEmailAction {
            return Err(CalendarError::Unauthorized);
        }
        Ok(Self {
            owner: record.owner(),
            mutation_target: record.mutation_target(),
            preview_digest: record.preview_digest(),
            workflow_revision: record
                .resulting_workflow_revision()
                .ok_or(CalendarError::Unauthorized)?,
        })
    }

    pub const fn owner(&self) -> ParticipantId {
        self.owner
    }

    pub const fn mutation_target(&self) -> MutationTargetFingerprint {
        self.mutation_target
    }

    pub const fn preview_digest(&self) -> PreviewDigest {
        self.preview_digest
    }

    pub const fn workflow_revision(&self) -> WorkflowRevision {
        self.workflow_revision
    }
}

// ── client + service ──────────────────────────────────────────────────

/// Provider outcome reported by a calendar client to the service.
///
/// `Created` → the event was created and a stable resource id returned.
/// `Ambiguous` → outcome unknown (e.g. timeout before the response arrived).
/// `Terminal` → the provider permanently rejected the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarProviderOutcome {
    Created(ExternalResourceId),
    Ambiguous,
    Terminal,
}

/// Google Calendar provider client boundary.
///
/// Implementations translate the validated [`CalendarEventRequest`] into a
/// Calendar API insert call using the workflow owner's access token. They must
/// not recompute or mutate any extracted value.
#[allow(async_fn_in_trait)]
pub trait GoogleCalendarClient {
    type Error: Display + fmt::Debug;

    async fn create_event(
        &self,
        token: &GoogleAccessToken,
        request: &CalendarEventRequest,
    ) -> Result<CalendarProviderOutcome, Self::Error>;
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum CalendarError {
    #[error("invalid event title")]
    InvalidTitle,
    #[error("invalid timezone")]
    InvalidTimezone,
    #[error("invalid event timestamp")]
    InvalidTimestamp,
    #[error("invalid calendar id")]
    InvalidCalendarId,
    #[error("invalid attendee email")]
    InvalidAttendee,
    #[error("invalid reminder settings")]
    InvalidReminder,
    #[error("at least one reminder channel is required")]
    NoReminders,
    #[error("invalid event description")]
    InvalidDescription,
    #[error("too many attendees")]
    TooManyAttendees,
    #[error("send_invitations set without any attendees")]
    InvitationsWithoutAttendees,
    #[error("confirmation does not authorize this calendar action")]
    Unauthorized,
    #[error("mutation target mismatch")]
    TargetMismatch,
    #[error("sanitized operation value invalid")]
    Sanitization,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum CreateEventError {
    #[error("confirmation does not authorize this calendar action")]
    Unauthorized,
    #[error("mutation target mismatch")]
    TargetMismatch,
    #[error("sanitized operation value invalid")]
    Sanitization,
}

fn failure(
    code: &'static str,
    summary: &'static str,
) -> Result<OperationFailure, CreateEventError> {
    let code = FailureCode::new(code).map_err(|_| CreateEventError::Sanitization)?;
    let summary = SanitizedSummary::new(summary).map_err(|_| CreateEventError::Sanitization)?;
    Ok(OperationFailure::new(code, summary))
}

/// Owner-authorized Calendar event creation service.
pub struct GoogleCalendarService<Client: GoogleCalendarClient> {
    client: Client,
}

impl<Client: GoogleCalendarClient> GoogleCalendarService<Client> {
    pub const fn new(client: Client) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Validate that the proof authorizes this request's target.
    pub fn validate(
        proof: &ConfirmedCalendarProof,
        request: &CalendarEventRequest,
    ) -> Result<(), CreateEventError> {
        if proof.mutation_target().as_bytes() != request.target_fingerprint().as_bytes() {
            return Err(CreateEventError::TargetMismatch);
        }
        Ok(())
    }

    /// Create the event after validating the proof, returning a
    /// [`ProviderOutcome`] for the shared executor.
    ///
    /// `Accepted` carries the stable event resource id. `Ambiguous` and
    /// `TerminalFailure` carry sanitized failure metadata only — never the
    /// request payload, attendee emails, or token.
    pub async fn create_event(
        &self,
        token: &GoogleAccessToken,
        proof: &ConfirmedCalendarProof,
        request: &CalendarEventRequest,
    ) -> Result<ProviderOutcome, CreateEventError> {
        Self::validate(proof, request)?;

        match self.client.create_event(token, request).await {
            Ok(CalendarProviderOutcome::Created(id)) => Ok(ProviderOutcome::Accepted {
                resource_id: Some(id),
            }),
            Ok(CalendarProviderOutcome::Ambiguous) => {
                let f = failure(
                    "google_calendar_ambiguous",
                    "google calendar outcome is ambiguous",
                )?;
                Ok(ProviderOutcome::Ambiguous(f))
            }
            Ok(CalendarProviderOutcome::Terminal) => {
                let f = failure(
                    "google_calendar_terminal",
                    "google calendar permanently rejected the event",
                )?;
                Ok(ProviderOutcome::TerminalFailure(f))
            }
            Err(_) => {
                let f = failure(
                    "google_calendar_failed",
                    "google calendar create attempt failed",
                )?;
                Ok(ProviderOutcome::RetryableFailure(f))
            }
        }
    }
}

/// `()` is not a real client; used only for the pure `validate` unit tests
/// below where no provider invocation occurs.
#[cfg(test)]
impl GoogleCalendarClient for () {
    type Error = std::convert::Infallible;

    async fn create_event(
        &self,
        _token: &GoogleAccessToken,
        _request: &CalendarEventRequest,
    ) -> Result<CalendarProviderOutcome, Self::Error> {
        unreachable!("unit validate tests never invoke the client")
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod tests {
    use super::*;

    fn op_target(bytes: [u8; 32]) -> OperationTargetFingerprint {
        OperationTargetFingerprint::new(bytes)
    }

    fn target_fingerprint() -> OperationTargetFingerprint {
        op_target([7u8; 32])
    }

    fn reminders() -> ReminderSettings {
        ReminderSettings::new(Some(10), None).expect("reminders")
    }

    fn attendees() -> Vec<AttendeeEmail> {
        vec![
            AttendeeEmail::new("alice@example.com").expect("attendee"),
            AttendeeEmail::new("bob@example.com").expect("attendee"),
        ]
    }

    fn valid_request(send_invitations: bool) -> CalendarEventRequest {
        CalendarEventRequest::new(
            EventTitle::new("Strategy sync").unwrap(),
            EventTimestamp::new("2025-02-03T10:00:00+05:30").unwrap(),
            EventTimestamp::new("2025-02-03T11:00:00+05:30").unwrap(),
            TimezoneLabel::new("Asia/Kolkata").unwrap(),
            CalendarSelection::Primary,
            None,
            attendees(),
            reminders(),
            send_invitations,
            target_fingerprint(),
        )
        .expect("valid request")
    }

    #[test]
    fn rejects_empty_or_oversized_title() {
        assert!(EventTitle::new("").is_err());
        assert!(EventTitle::new("   ").is_err());
        assert!(EventTitle::new("x".repeat(MAX_TITLE_BYTES + 1)).is_err());
        assert!(EventTitle::new("control\x00char").is_err());
    }

    #[test]
    fn rejects_invalid_timezone() {
        assert!(TimezoneLabel::new("").is_err());
        assert!(TimezoneLabel::new("Asia Kolkata").is_err());
        assert!(TimezoneLabel::new("Asia/Kolkata\n").is_err());
        assert!(TimezoneLabel::new("Asia/Kolkata").is_ok());
    }

    #[test]
    fn rejects_invalid_attendee_email() {
        assert!(AttendeeEmail::new("not-an-email").is_err());
        assert!(AttendeeEmail::new("a@").is_err());
        assert!(AttendeeEmail::new("@example.com").is_err());
        assert!(AttendeeEmail::new("a b@example.com").is_err());
        assert!(AttendeeEmail::new("a@example.com").is_ok());
    }

    #[test]
    fn reminder_settings_require_at_least_one_channel() {
        assert!(ReminderSettings::new(None, None).is_err());
        assert!(ReminderSettings::new(Some(0), None).is_ok());
        assert!(ReminderSettings::new(None, Some(30)).is_ok());
        assert!(ReminderSettings::new(Some(10), Some(60)).is_ok());
        assert!(ReminderSettings::new(Some(MAX_REMINDER_MINUTES + 1), None).is_err());
    }

    #[test]
    fn request_rejects_invitations_without_attendees() {
        let err = CalendarEventRequest::new(
            EventTitle::new("Sync").unwrap(),
            EventTimestamp::new("2025-02-03T10:00:00Z").unwrap(),
            EventTimestamp::new("2025-02-03T11:00:00Z").unwrap(),
            TimezoneLabel::new("UTC").unwrap(),
            CalendarSelection::Primary,
            None,
            Vec::new(),
            reminders(),
            true,
            target_fingerprint(),
        )
        .expect_err("invitations without attendees rejected");
        assert!(matches!(err, CalendarError::InvitationsWithoutAttendees));
    }

    #[test]
    fn request_rejects_too_many_attendees() {
        let mut many = Vec::new();
        for i in 0..(MAX_ATTENDEES + 1) {
            many.push(AttendeeEmail::new(format!("a{i}@example.com")).unwrap());
        }
        let err = CalendarEventRequest::new(
            EventTitle::new("Sync").unwrap(),
            EventTimestamp::new("2025-02-03T10:00:00Z").unwrap(),
            EventTimestamp::new("2025-02-03T11:00:00Z").unwrap(),
            TimezoneLabel::new("UTC").unwrap(),
            CalendarSelection::Primary,
            None,
            many,
            reminders(),
            false,
            target_fingerprint(),
        )
        .expect_err("too many attendees rejected");
        assert!(matches!(err, CalendarError::TooManyAttendees));
    }

    #[test]
    fn request_allows_primary_calendar_default() {
        let req = valid_request(true);
        assert_eq!(req.calendar(), &CalendarSelection::Primary);
        assert!(req.send_invitations());
        assert_eq!(req.attendees().len(), 2);
    }

    #[test]
    fn request_allows_alternate_calendar_selection() {
        let alt = CalendarId::new("work-calendar-123").unwrap();
        let req = CalendarEventRequest::new(
            EventTitle::new("Sync").unwrap(),
            EventTimestamp::new("2025-02-03T10:00:00Z").unwrap(),
            EventTimestamp::new("2025-02-03T11:00:00Z").unwrap(),
            TimezoneLabel::new("UTC").unwrap(),
            CalendarSelection::Alternate(alt),
            None,
            attendees(),
            reminders(),
            true,
            target_fingerprint(),
        )
        .unwrap();
        assert!(matches!(req.calendar(), CalendarSelection::Alternate(_)));
    }

    #[test]
    fn validate_rejects_target_mismatch() {
        // Proof from a different confirmation target than the request.
        let proof = ConfirmedCalendarProof {
            owner: ParticipantId::new(101).unwrap(),
            mutation_target: MutationTargetFingerprint::new([9u8; 32]),
            preview_digest: PreviewDigest::new([1u8; 32]),
            workflow_revision: WorkflowRevision::new(2),
        };
        let req = valid_request(false);
        let result = GoogleCalendarService::<()>::validate(&proof, &req);
        assert!(matches!(result, Err(CreateEventError::TargetMismatch)));
    }

    #[test]
    fn validate_accepts_matching_target() {
        let proof = ConfirmedCalendarProof {
            owner: ParticipantId::new(101).unwrap(),
            mutation_target: MutationTargetFingerprint::new([7u8; 32]),
            preview_digest: PreviewDigest::new([1u8; 32]),
            workflow_revision: WorkflowRevision::new(2),
        };
        let req = valid_request(false);
        GoogleCalendarService::<()>::validate(&proof, &req).expect("target matches");
    }
}
